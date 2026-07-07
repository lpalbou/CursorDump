//! Full, faithful backup of Cursor projects.
//!
//! Unlike the training export (which transforms transcripts), a backup is a
//! verbatim recursive copy of `~/.cursor/projects/<slug>/…` — every
//! transcript, subagent, asset, upload, canvas, terminal and tool file — so
//! nothing is lost if Cursor flushes its data. The copy mirrors the original
//! layout under `<out>/projects/<slug>/…`, preserving modification times, so
//! restoring is a plain copy back into `~/.cursor/projects`.
//!
//! Backups are INCREMENTAL and idempotent: pointing at the same output folder
//! again re-copies only files whose size or mtime changed. Source data is
//! never modified. The output folder is refused if it lives inside
//! `~/.cursor`.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;
use sha2::{Digest, Sha256};

/// Marker/manifest filename that identifies a directory as a CursorDump backup
/// (so re-runs into it are allowed and recognized as incremental).
const MARKER: &str = "cursordump-backup.json";

#[derive(Debug, Clone)]
pub struct BackupOptions {
    /// Restrict to these project slugs; `None` = every project.
    pub projects: Option<Vec<String>>,
    /// Skip ephemeral runtime dirs (`terminals/`, `agent-tools/`) that Cursor
    /// regenerates. Default false — a backup should lose nothing.
    pub skip_runtime: bool,
    /// Record a sha256 for `.jsonl` transcripts in the manifest (integrity).
    pub verify_transcripts: bool,
    /// Bundle the current `cursordump` executable into the backup so it can be
    /// re-explored on this machine even if Cursor (and this repo) are gone.
    pub include_app: bool,
    /// Also copy external attachments the sessions referenced by absolute path
    /// (workspace `@file`s that live OUTSIDE ~/.cursor) into
    /// `<backup>/attachments/`, so they survive even if the workspace changes.
    pub include_external_attachments: bool,
}

impl Default for BackupOptions {
    fn default() -> Self {
        Self {
            projects: None,
            skip_runtime: false,
            verify_transcripts: true,
            include_app: true,
            include_external_attachments: true,
        }
    }
}

#[derive(Debug)]
pub enum BackupEvent {
    Progress {
        done: usize,
        total: usize,
        stage: String,
    },
    Done(BackupSummary),
    Failed(String),
}

#[derive(Debug, Clone, Default)]
pub struct BackupSummary {
    pub out_dir: PathBuf,
    pub projects: usize,
    pub files_copied: usize,
    pub files_unchanged: usize,
    pub bytes_copied: u64,
    pub bytes_total: u64,
    pub warnings: Vec<String>,
}

/// Runtime directory names skipped when `skip_runtime` is set.
const RUNTIME_DIRS: &[&str] = &["terminals", "agent-tools"];

/// Directory names ALWAYS skipped: pure regenerable caches, never user data
/// (canvas dependency installs, VCS metadata). Skipping keeps backups lean
/// without losing anything a Cursor flush would actually destroy.
const ALWAYS_SKIP_DIRS: &[&str] = &["node_modules", ".git"];

fn unix(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Validate a backup destination: never inside `cursor_root`, and if it
/// already exists and is non-empty it must be a prior CursorDump backup.
pub fn validate_backup_dir(out_dir: &Path, cursor_root: &Path) -> Result<(), String> {
    let mut probe = out_dir.to_path_buf();
    let canonical = loop {
        match probe.canonicalize() {
            Ok(c) => break c,
            Err(_) => match probe.parent() {
                Some(p) => probe = p.to_path_buf(),
                None => return Err("output path has no existing ancestor".into()),
            },
        }
    };
    let cursor_canon = cursor_root
        .canonicalize()
        .unwrap_or_else(|_| cursor_root.to_path_buf());
    if canonical.starts_with(&cursor_canon) {
        return Err(format!(
            "refusing to back up inside {} — pick a directory outside ~/.cursor",
            cursor_root.display()
        ));
    }
    if out_dir.is_dir() {
        let non_empty = fs::read_dir(out_dir)
            .map(|mut it| it.next().is_some())
            .unwrap_or(false);
        if non_empty && !out_dir.join(MARKER).is_file() {
            return Err(format!(
                "{} exists and is not empty — choose a new folder or a previous CursorDump backup",
                out_dir.display()
            ));
        }
    }
    Ok(())
}

/// Blocking backup, intended for a background thread/task. `repaint` (or a
/// no-op) is invoked after progress events.
pub fn run_backup(
    root: PathBuf,
    out_dir: PathBuf,
    options: BackupOptions,
    cursor_root: PathBuf,
    tx: Sender<BackupEvent>,
    repaint: impl Fn(),
) {
    let send = |ev: BackupEvent| {
        let _ = tx.send(ev);
        repaint();
    };
    if let Err(e) = validate_backup_dir(&out_dir, &cursor_root) {
        send(BackupEvent::Failed(e));
        return;
    }

    // Resolve the set of project directories to back up.
    let project_dirs = match collect_project_dirs(&root, &options) {
        Ok(d) => d,
        Err(e) => {
            send(BackupEvent::Failed(e));
            return;
        }
    };
    let total = project_dirs.len();
    let mut summary = BackupSummary {
        out_dir: out_dir.clone(),
        ..Default::default()
    };

    let dest_projects = out_dir.join("projects");
    if let Err(e) = fs::create_dir_all(&dest_projects) {
        send(BackupEvent::Failed(format!(
            "cannot create {}: {e}",
            dest_projects.display()
        )));
        return;
    }

    let mut transcript_hashes: Vec<serde_json::Value> = Vec::new();
    let mut project_entries: Vec<serde_json::Value> = Vec::new();

    for (i, (slug, src)) in project_dirs.iter().enumerate() {
        send(BackupEvent::Progress {
            done: i,
            total,
            stage: format!("backing up {slug}"),
        });
        let dest = dest_projects.join(slug);
        let mut pfiles = 0usize;
        let mut pbytes = 0u64;
        if let Err(e) = copy_tree(
            src,
            &dest,
            &options,
            &mut summary,
            &mut pfiles,
            &mut pbytes,
            &mut transcript_hashes,
            slug,
            &out_dir,
        ) {
            summary.warnings.push(format!("{slug}: {e}"));
        }
        project_entries.push(json!({
            "slug": slug,
            "source": src.display().to_string(),
            "files": pfiles,
            "bytes": pbytes,
        }));
        summary.projects += 1;
    }

    // Load any PRIOR manifest so subset re-runs MERGE records instead of
    // clobbering them (a re-run for one project must not lose the integrity
    // records of the others). A manifest that EXISTS but cannot be parsed is
    // preserved as `.corrupt` and reported — never silently discarded.
    let prior = match fs::read_to_string(out_dir.join(MARKER)) {
        Ok(s) => match serde_json::from_str::<serde_json::Value>(&s) {
            Ok(v) => Some(v),
            Err(e) => {
                let bak = out_dir.join(format!("{MARKER}.corrupt"));
                let _ = fs::copy(out_dir.join(MARKER), &bak);
                summary.warnings.push(format!(
                    "prior manifest was unreadable ({e}); kept a copy at {} — its integrity records are not merged into this run",
                    bak.display()
                ));
                None
            }
        },
        Err(_) => None,
    };
    let prior_attachment_hashes: std::collections::HashMap<String, String> = prior
        .as_ref()
        .and_then(|m| m.get("attachments")?.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    Some((
                        a.get("file")?.as_str()?.to_string(),
                        a.get("sha256")?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();

    // Capture external referenced attachments (workspace @files) so the backup
    // is complete even for files that live outside ~/.cursor. Every attachment
    // PRESENT for this run's projects is recorded (with sha256), whether it
    // was copied now or on a previous run.
    let mut attachment_entries: Vec<serde_json::Value> = Vec::new();
    let mut external_captured = 0usize; // newly copied this run
    if options.include_external_attachments {
        send(BackupEvent::Progress {
            done: total,
            total,
            stage: "capturing external attachments".into(),
        });
        let att_dir = out_dir.join("attachments");
        let mut seen = std::collections::HashSet::new();
        for (_slug, src) in &project_dirs {
            let tdir = src.join("agent-transcripts");
            for jsonl in transcripts_in(&tdir) {
                let meta = crate::model::SessionMeta {
                    id: String::new(),
                    project_slug: String::new(),
                    path: jsonl.clone(),
                    title: String::new(),
                    modified: None,
                    size_bytes: 0,
                    is_subagent: false,
                    parent_id: None,
                };
                let parsed = crate::parser::parse_session(&meta);
                // boundary = the projects root; refs inside it are already
                // copied verbatim, so only capture those OUTSIDE it.
                for r in crate::media::extract_media_refs(&parsed, &root) {
                    if r.within_cursor || !r.exists {
                        continue;
                    }
                    if !seen.insert(r.path.clone()) {
                        continue;
                    }
                    if fs::create_dir_all(&att_dir).is_err() {
                        break;
                    }
                    let name = crate::media::attachment_filename(&r.path);
                    let dest = att_dir.join(&name);
                    // Unchanged = destination exists AND matches the source's
                    // size + mtime; a source edited since the last run is
                    // re-copied and re-hashed (same incremental rule as the
                    // main tree).
                    let unchanged = match (fs::metadata(&dest), fs::metadata(&r.path)) {
                        (Ok(d), Ok(s)) => {
                            d.len() == s.len()
                                && match (d.modified().ok(), s.modified().ok()) {
                                    (Some(a), Some(b)) => within_2s(a, b),
                                    _ => false,
                                }
                        }
                        _ => false,
                    };
                    let mut sha: Option<String> = None;
                    if unchanged {
                        // Reuse the recorded hash (avoids rehashing huge media).
                        sha = prior_attachment_hashes.get(&name).cloned();
                    } else {
                        match fs::copy(&r.path, &dest) {
                            Ok(n) => {
                                external_captured += 1;
                                summary.bytes_copied += n;
                                // Mirror the source mtime so the next run's
                                // unchanged-detection works.
                                if let Ok(smeta) = fs::metadata(&r.path) {
                                    if let (Some(t), Ok(f)) = (
                                        smeta.modified().ok(),
                                        fs::OpenOptions::new().write(true).open(&dest),
                                    ) {
                                        let _ = f.set_modified(t);
                                    }
                                }
                            }
                            Err(e) => {
                                summary.warnings.push(format!(
                                    "attachment copy failed for {}: {e}",
                                    r.path.display()
                                ));
                                continue;
                            }
                        }
                    }
                    let sha = sha.or_else(|| sha256_file(&dest).ok());
                    attachment_entries.push(json!({
                        "original_path": r.path.display().to_string(),
                        "file": name,
                        "kind": r.kind.label(),
                        "sha256": sha,
                    }));
                }
            }
        }
    }

    // Bundle the app itself: the backup layout mirrors ~/.cursor/projects, so
    // `./cursordump projects` inside the backup re-opens the full explorer
    // with no Cursor installation required.
    if options.include_app {
        match std::env::current_exe() {
            Ok(exe) => {
                let dest = out_dir.join("cursordump");
                if let Err(e) = fs::copy(&exe, &dest) {
                    summary
                        .warnings
                        .push(format!("could not bundle cursordump binary: {e}"));
                }
            }
            Err(e) => summary
                .warnings
                .push(format!("could not locate cursordump binary: {e}")),
        }
    }

    send(BackupEvent::Progress {
        done: total,
        total,
        stage: "writing manifest".into(),
    });

    // MERGE with the prior manifest: records for projects NOT part of this run
    // are preserved, so a subset re-run never loses integrity records.
    let run_slugs: std::collections::HashSet<&str> =
        project_dirs.iter().map(|(s, _)| s.as_str()).collect();
    let keep_prior = |key: &str, slug_field: &str| -> Vec<serde_json::Value> {
        prior
            .as_ref()
            .and_then(|m| m.get(key)?.as_array().cloned())
            .unwrap_or_default()
            .into_iter()
            .filter(|e| {
                e.get(slug_field)
                    .and_then(|v| v.as_str())
                    .map(|s| !run_slugs.contains(s))
                    .unwrap_or(false)
            })
            .collect()
    };
    let mut merged_projects = keep_prior("projects", "slug");
    merged_projects.extend(project_entries);
    let mut merged_transcripts = keep_prior("transcripts", "project");
    merged_transcripts.extend(transcript_hashes);
    // Attachments are keyed by captured filename (globally unique per source
    // path); keep prior entries this run did not re-record.
    let new_att_files: std::collections::HashSet<String> = attachment_entries
        .iter()
        .filter_map(|a| a.get("file").and_then(|v| v.as_str()).map(String::from))
        .collect();
    let mut merged_attachments: Vec<serde_json::Value> = prior
        .as_ref()
        .and_then(|m| m.get("attachments")?.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter(|a| {
            a.get("file")
                .and_then(|v| v.as_str())
                .map(|f| !new_att_files.contains(f))
                .unwrap_or(false)
        })
        .collect();
    merged_attachments.extend(attachment_entries);

    let merged_files: u64 = merged_projects
        .iter()
        .filter_map(|p| p.get("files").and_then(|v| v.as_u64()))
        .sum();
    let merged_bytes: u64 = merged_projects
        .iter()
        .filter_map(|p| p.get("bytes").and_then(|v| v.as_u64()))
        .sum();

    let manifest = json!({
        "tool": "CursorDump",
        "kind": "full-backup",
        "version": env!("CARGO_PKG_VERSION"),
        "created_unix": SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        "source_root": root.display().to_string(),
        "restore_hint": "Copy the `projects/` subfolders back into ~/.cursor/projects to restore.",
        "options": {
            "skip_runtime": options.skip_runtime,
            "verify_transcripts": options.verify_transcripts,
            "include_app": options.include_app,
            "include_external_attachments": options.include_external_attachments,
            "projects_filter": options.projects,
        },
        "projects": merged_projects,
        "transcripts": merged_transcripts,
        "attachments": merged_attachments,
        "totals": {
            "projects": prior_or_run_project_count(&run_slugs, prior.as_ref(), summary.projects),
            "files": merged_files,
            "bytes": merged_bytes,
            "transcripts": merged_transcripts.len(),
            "attachments": merged_attachments.len(),
        },
        "last_run": {
            "projects_in_run": project_dirs.iter().map(|(s, _)| s.clone()).collect::<Vec<_>>(),
            "files_copied": summary.files_copied,
            "files_unchanged": summary.files_unchanged,
            "bytes_copied": summary.bytes_copied,
            "bytes_total": summary.bytes_total,
            "external_attachments_copied": external_captured,
        },
        "warnings": summary.warnings,
    });

    match fs::write(
        out_dir.join(MARKER),
        serde_json::to_string_pretty(&manifest).unwrap_or_default(),
    ) {
        Ok(()) => {
            let _ = write_readme(&out_dir, &summary, &options);
            send(BackupEvent::Done(summary));
        }
        Err(e) => send(BackupEvent::Failed(format!("cannot write manifest: {e}"))),
    }
}

/// Total distinct project count after a merge (prior projects not in this run
/// plus this run's).
fn prior_or_run_project_count(
    run_slugs: &std::collections::HashSet<&str>,
    prior: Option<&serde_json::Value>,
    run_count: usize,
) -> usize {
    let kept_prior = prior
        .and_then(|m| m.get("projects")?.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|e| {
                    e.get("slug")
                        .and_then(|v| v.as_str())
                        .map(|s| !run_slugs.contains(s))
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0);
    kept_prior + run_count
}

/// All transcript `.jsonl` files (main + subagents) under an agent-transcripts
/// directory.
fn transcripts_in(tdir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(tdir) else {
        return out;
    };
    for e in entries.flatten() {
        let dir = e.path();
        if !dir.is_dir() {
            continue;
        }
        let id = e.file_name().to_string_lossy().to_string();
        let main = dir.join(format!("{id}.jsonl"));
        if main.is_file() {
            out.push(main);
        }
        if let Ok(subs) = fs::read_dir(dir.join("subagents")) {
            for s in subs.flatten() {
                let p = s.path();
                if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
                    out.push(p);
                }
            }
        }
    }
    out
}

/// (slug, source dir) for each project to back up.
fn collect_project_dirs(
    root: &Path,
    options: &BackupOptions,
) -> Result<Vec<(String, PathBuf)>, String> {
    let entries = fs::read_dir(root).map_err(|e| format!("cannot read {}: {e}", root.display()))?;
    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        let slug = entry.file_name().to_string_lossy().to_string();
        if let Some(filter) = &options.projects {
            if !filter.contains(&slug) {
                continue;
            }
        }
        dirs.push((slug, p));
    }
    dirs.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(dirs)
}

#[allow(clippy::too_many_arguments)]
fn copy_tree(
    src: &Path,
    dest: &Path,
    options: &BackupOptions,
    summary: &mut BackupSummary,
    pfiles: &mut usize,
    pbytes: &mut u64,
    transcript_hashes: &mut Vec<serde_json::Value>,
    slug: &str,
    backup_root: &Path,
) -> Result<(), String> {
    // Top-level runtime-dir skipping (relative to the project root).
    let entries = fs::read_dir(src).map_err(|e| e.to_string())?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let child = entry.path();
        let file_type = entry.file_type().map_err(|e| e.to_string())?;
        if file_type.is_symlink() {
            summary
                .warnings
                .push(format!("{slug}: skipped symlink {name}"));
            continue;
        }
        if file_type.is_dir() {
            if options.skip_runtime && RUNTIME_DIRS.contains(&name.as_str()) {
                continue;
            }
            if ALWAYS_SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            copy_dir_recursive(
                &child,
                &dest.join(&name),
                summary,
                pfiles,
                pbytes,
                transcript_hashes,
                slug,
                options.verify_transcripts,
                backup_root,
            )?;
        } else if file_type.is_file() {
            copy_one(
                &child,
                &dest.join(&name),
                summary,
                pfiles,
                pbytes,
                transcript_hashes,
                slug,
                options.verify_transcripts,
                backup_root,
            )?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn copy_dir_recursive(
    src: &Path,
    dest: &Path,
    summary: &mut BackupSummary,
    pfiles: &mut usize,
    pbytes: &mut u64,
    transcript_hashes: &mut Vec<serde_json::Value>,
    slug: &str,
    verify: bool,
    backup_root: &Path,
) -> Result<(), String> {
    fs::create_dir_all(dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
    let entries = match fs::read_dir(src) {
        Ok(e) => e,
        Err(e) => {
            summary
                .warnings
                .push(format!("{slug}: {} : {e}", src.display()));
            return Ok(());
        }
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let child = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            summary
                .warnings
                .push(format!("{slug}: skipped symlink {}", child.display()));
            continue;
        }
        if ft.is_dir() {
            if ALWAYS_SKIP_DIRS.contains(&name.to_string_lossy().as_ref()) {
                continue;
            }
            copy_dir_recursive(
                &child,
                &dest.join(name),
                summary,
                pfiles,
                pbytes,
                transcript_hashes,
                slug,
                verify,
                backup_root,
            )?;
        } else if ft.is_file() {
            copy_one(
                &child,
                &dest.join(name),
                summary,
                pfiles,
                pbytes,
                transcript_hashes,
                slug,
                verify,
                backup_root,
            )?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn copy_one(
    src: &Path,
    dest: &Path,
    summary: &mut BackupSummary,
    pfiles: &mut usize,
    pbytes: &mut u64,
    transcript_hashes: &mut Vec<serde_json::Value>,
    slug: &str,
    verify: bool,
    backup_root: &Path,
) -> Result<(), String> {
    let meta = match fs::metadata(src) {
        Ok(m) => m,
        Err(e) => {
            summary
                .warnings
                .push(format!("{slug}: stat {} : {e}", src.display()));
            return Ok(());
        }
    };
    let len = meta.len();
    let mtime = meta.modified().ok();
    *pfiles += 1;
    *pbytes += len;
    summary.bytes_total += len;

    // Incremental: skip if destination already matches size + mtime.
    if let Ok(dmeta) = fs::metadata(dest) {
        let same_len = dmeta.len() == len;
        let same_mtime = match (dmeta.modified().ok(), mtime) {
            (Some(a), Some(b)) => within_2s(a, b),
            _ => false,
        };
        if same_len && same_mtime {
            summary.files_unchanged += 1;
            maybe_hash_transcript(src, dest, slug, verify, backup_root, transcript_hashes);
            return Ok(());
        }
    }

    match fs::copy(src, dest) {
        Ok(_) => {
            summary.files_copied += 1;
            summary.bytes_copied += len;
            // Preserve the original modification time for faithful restore
            // and correct incremental detection next run.
            if let Some(t) = mtime {
                if let Ok(f) = fs::OpenOptions::new().write(true).open(dest) {
                    let _ = f.set_modified(t);
                }
            }
            maybe_hash_transcript(src, dest, slug, verify, backup_root, transcript_hashes);
        }
        Err(e) => summary
            .warnings
            .push(format!("{slug}: copy {} : {e}", src.display())),
    }
    Ok(())
}

/// Record a sha256 for `.jsonl` transcripts (integrity check on restore).
/// No-op when transcript verification is disabled (`--no-verify`).
fn maybe_hash_transcript(
    src: &Path,
    dest: &Path,
    slug: &str,
    verify: bool,
    backup_root: &Path,
    out: &mut Vec<serde_json::Value>,
) {
    if !verify || src.extension().and_then(|e| e.to_str()) != Some("jsonl") {
        return;
    }
    let Ok(hash) = sha256_file(dest) else { return };
    // `path` locates the file inside the backup for `cursordump verify`;
    // `file` (basename) is kept for compatibility with older manifests.
    let relpath = dest
        .strip_prefix(backup_root)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    out.push(json!({
        "project": slug,
        "file": dest.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        "path": relpath,
        "sha256": hash,
        "mtime_unix": fs::metadata(src).ok().and_then(|m| m.modified().ok()).map(unix),
    }));
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut f = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn within_2s(a: SystemTime, b: SystemTime) -> bool {
    let d = if a > b {
        a.duration_since(b)
    } else {
        b.duration_since(a)
    };
    d.map(|d| d.as_secs() <= 2).unwrap_or(false)
}

fn write_readme(
    out_dir: &Path,
    summary: &BackupSummary,
    options: &BackupOptions,
) -> Result<(), String> {
    let mb = summary.bytes_total as f64 / 1_048_576.0;
    let explore = if options.include_app {
        "## Re-explore WITHOUT Cursor\n\n\
         This backup is self-contained. The bundled `cursordump` app opens the\n\
         full explorer (projects, sessions, thinking, attachments, search,\n\
         dataset export) directly on this backup — Cursor does not need to be\n\
         installed:\n\n\
         ```bash\ncd \"$(dirname \"$0\")\" 2>/dev/null || true\n./cursordump projects\n```\n\n\
         (First run on macOS may require: `xattr -d com.apple.quarantine cursordump`.)\n\
         If the bundled binary doesn't match your OS/architecture, build\n\
         CursorDump from source and run `cursordump /path/to/this/backup/projects`.\n\n"
    } else {
        "## Re-explore WITHOUT Cursor\n\n\
         Run CursorDump against this backup (no Cursor installation needed):\n\n\
         ```bash\ncursordump /path/to/this/backup/projects\n```\n\n"
    };
    let card = format!(
        "# CursorDump backup\n\n\
         Faithful, complete copy of Cursor projects (transcripts, subagents, \
         assets, uploads, canvases, terminals, tool caches).\n\n\
         - Projects: {}\n- Files: {} copied, {} unchanged\n- Size: {:.1} MB\n\n\
         {explore}\
         ## Restore into Cursor\n\n\
         Copy the project folders back into your Cursor data directory:\n\n\
         ```bash\ncp -a projects/* ~/.cursor/projects/\n```\n\n\
         ## Integrity & incremental re-runs\n\n\
         `cursordump-backup.json` lists every project and a sha256 for each \
         `.jsonl` transcript so you can verify integrity. This backup was made \
         read-only from the source; re-running the backup into this same folder \
         only copies files that changed (incremental).\n",
        summary.projects, summary.files_copied, summary.files_unchanged, mb
    );
    fs::write(out_dir.join("README.md"), card).map_err(|e| e.to_string())
}

// ------------------------------------------------------------------ verify

/// Result of `verify_backup`.
#[derive(Debug, Default)]
pub struct VerifyReport {
    pub transcripts_ok: usize,
    pub transcripts_failed: Vec<String>,
    pub transcripts_missing: Vec<String>,
    pub attachments_ok: usize,
    pub attachments_failed: Vec<String>,
    pub attachments_missing: Vec<String>,
    /// Manifest entries without a hash (e.g. created with --no-verify).
    pub unhashed: usize,
    /// Transcript files present in the backup tree but absent from the
    /// manifest — their integrity is unknown (possible injection or
    /// tampering).
    pub unlisted: Vec<String>,
}

impl VerifyReport {
    pub fn is_ok(&self) -> bool {
        self.transcripts_failed.is_empty()
            && self.transcripts_missing.is_empty()
            && self.attachments_failed.is_empty()
            && self.attachments_missing.is_empty()
            && self.unlisted.is_empty()
    }
}

/// A manifest `path` must stay inside the backup: relative, no `..`
/// components. Anything else is treated as tampered.
fn safe_manifest_path(p: &str) -> bool {
    let path = Path::new(p);
    !path.is_absolute()
        && path
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// Verify a backup directory against its manifest: every recorded transcript
/// and attachment hash is recomputed and compared. Read-only.
pub fn verify_backup(backup_dir: &Path) -> Result<VerifyReport, String> {
    let manifest_path = backup_dir.join(MARKER);
    let manifest: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(&manifest_path)
            .map_err(|e| format!("not a CursorDump backup ({}): {e}", manifest_path.display()))?,
    )
    .map_err(|e| format!("manifest is not valid JSON: {e}"))?;

    let mut report = VerifyReport::default();

    // Transcripts: prefer the recorded relative `path`; fall back to locating
    // by basename under projects/<slug>/ (manifests from older versions).
    for t in manifest
        .get("transcripts")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        let Some(expected) = t.get("sha256").and_then(|v| v.as_str()) else {
            report.unhashed += 1;
            continue;
        };
        let label = t
            .get("path")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| {
                Some(format!(
                    "{}/{}",
                    t.get("project")?.as_str()?,
                    t.get("file")?.as_str()?
                ))
            })
            .unwrap_or_else(|| "<unknown>".into());
        // Candidates: a recorded relative `path` is authoritative — it must
        // stay inside the backup (no `..`/absolute; a tampered manifest fails
        // verification) and there is NO fallback if the file is gone (a
        // silent basename fallback could "pass" against a different
        // same-named copy). The basename search only serves manifests from
        // older versions that recorded no `path`; a self-forked session
        // legitimately exists twice with the same name (main + subagents
        // copy), so those entries pass if ANY candidate matches the hash.
        let candidates: Vec<PathBuf> = match t.get("path").and_then(|v| v.as_str()) {
            Some(p) => {
                if !safe_manifest_path(p) {
                    report
                        .transcripts_failed
                        .push(format!("{label} (unsafe path)"));
                    continue;
                }
                let f = backup_dir.join(p);
                if !f.is_file() {
                    report.transcripts_missing.push(label);
                    continue;
                }
                vec![f]
            }
            None => match (
                t.get("project").and_then(|v| v.as_str()),
                t.get("file").and_then(|v| v.as_str()),
            ) {
                (Some(slug), Some(name)) => {
                    find_all_by_name(&backup_dir.join("projects").join(slug), name)
                }
                _ => Vec::new(),
            },
        };
        if candidates.is_empty() {
            report.transcripts_missing.push(label);
            continue;
        }
        let matched = candidates
            .iter()
            .any(|f| sha256_file(f).is_ok_and(|h| h == expected));
        if matched {
            report.transcripts_ok += 1;
        } else {
            report.transcripts_failed.push(label);
        }
    }

    // Attachments live flat under attachments/<captured-name>.
    for a in manifest
        .get("attachments")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        let Some(name) = a.get("file").and_then(|v| v.as_str()) else {
            continue;
        };
        if !safe_manifest_path(name) {
            report
                .attachments_failed
                .push(format!("{name} (unsafe path)"));
            continue;
        }
        let Some(expected) = a.get("sha256").and_then(|v| v.as_str()) else {
            report.unhashed += 1;
            continue;
        };
        let f = backup_dir.join("attachments").join(name);
        if !f.is_file() {
            report.attachments_missing.push(name.to_string());
            continue;
        }
        match sha256_file(&f) {
            Ok(h) if h == expected => report.attachments_ok += 1,
            Ok(_) => report.attachments_failed.push(name.to_string()),
            Err(e) => report.attachments_failed.push(format!("{name}: {e}")),
        }
    }

    // Sweep for UNLISTED transcripts: a `.jsonl` in the backup tree that no
    // manifest entry covers has unknown integrity (a tampered backup could
    // have injected it, and restore would happily copy it). Only projects
    // with at least one hashed manifest entry are swept — a project backed
    // up with --no-verify has no coverage to check against (`unhashed`
    // already signals that).
    let transcripts = manifest
        .get("transcripts")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut covered_paths: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut covered_names: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    let mut hashed_projects: std::collections::HashSet<String> = std::collections::HashSet::new();
    for t in &transcripts {
        let slug = t.get("project").and_then(|v| v.as_str()).unwrap_or("");
        if t.get("sha256").and_then(|v| v.as_str()).is_some() {
            hashed_projects.insert(slug.to_string());
        }
        match t
            .get("path")
            .and_then(|v| v.as_str())
            .filter(|p| safe_manifest_path(p))
        {
            // A recorded path covers exactly that one file.
            Some(p) => {
                covered_paths.insert(backup_dir.join(p));
            }
            // Legacy entries (no path) can only cover by basename.
            None => {
                if let Some(name) = t.get("file").and_then(|v| v.as_str()) {
                    covered_names.insert((slug.to_string(), name.to_string()));
                }
            }
        }
    }
    for slug in &hashed_projects {
        let proj_dir = backup_dir.join("projects").join(slug);
        for f in find_all_with_extension(&proj_dir, "jsonl") {
            let name = f
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if covered_paths.contains(&f) || covered_names.contains(&(slug.clone(), name)) {
                continue;
            }
            report.unlisted.push(
                f.strip_prefix(backup_dir)
                    .unwrap_or(&f)
                    .to_string_lossy()
                    .to_string(),
            );
        }
    }

    Ok(report)
}

/// Depth-first listing of every file with the given extension.
fn find_all_with_extension(root: &Path, ext: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_file() && p.extension().and_then(|x| x.to_str()) == Some(ext) {
            out.push(p);
        } else if p.is_dir() {
            out.extend(find_all_with_extension(&p, ext));
        }
    }
    out
}

/// Depth-first search for ALL files with a given name. A transcript basename
/// is usually unique, but a self-forked session exists as both a main
/// transcript and a `subagents/` copy with the same `<uuid>.jsonl` name.
fn find_all_by_name(root: &Path, name: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_file() && e.file_name().to_string_lossy() == name {
            out.push(p);
        } else if p.is_dir() {
            out.extend(find_all_by_name(&p, name));
        }
    }
    out
}

// ----------------------------------------------------------------- restore

#[derive(Debug, Clone, Default)]
pub struct RestoreOptions {
    /// Restrict to these project slugs; `None` = every project in the backup.
    pub projects: Option<Vec<String>>,
    /// Report what would be copied without writing anything.
    pub dry_run: bool,
    /// Also overwrite destination files that differ (default: only copy files
    /// missing at the destination — never touch existing data).
    pub overwrite: bool,
}

#[derive(Debug, Default)]
pub struct RestoreSummary {
    pub files_copied: usize,
    pub files_skipped_existing: usize,
    pub bytes_copied: u64,
    pub warnings: Vec<String>,
    pub dry_run: bool,
}

/// Restore `<backup>/projects/*` into `dest_projects_root` (normally
/// `~/.cursor/projects`). This is the ONLY code path in CursorDump that
/// writes under `~/.cursor`, and it is deliberately conservative:
///
/// - default mode copies only files MISSING at the destination;
/// - `overwrite` additionally replaces files whose size/mtime differ;
/// - existing files are never deleted;
/// - `dry_run` reports without writing.
pub fn restore_backup(
    backup_dir: &Path,
    dest_projects_root: &Path,
    options: &RestoreOptions,
) -> Result<RestoreSummary, String> {
    let src_projects = backup_dir.join("projects");
    if !backup_dir.join(MARKER).is_file() || !src_projects.is_dir() {
        return Err(format!(
            "{} is not a CursorDump backup (missing {MARKER} or projects/)",
            backup_dir.display()
        ));
    }
    let mut summary = RestoreSummary {
        dry_run: options.dry_run,
        ..Default::default()
    };
    let entries =
        fs::read_dir(&src_projects).map_err(|e| format!("cannot read backup projects: {e}"))?;
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let slug = entry.file_name().to_string_lossy().to_string();
        if let Some(filter) = &options.projects {
            if !filter.contains(&slug) {
                continue;
            }
        }
        restore_tree(
            &entry.path(),
            &dest_projects_root.join(&slug),
            options,
            &mut summary,
        );
    }
    Ok(summary)
}

fn restore_tree(src: &Path, dest: &Path, options: &RestoreOptions, summary: &mut RestoreSummary) {
    let Ok(entries) = fs::read_dir(src) else {
        summary
            .warnings
            .push(format!("cannot read {}", src.display()));
        return;
    };
    for entry in entries.flatten() {
        let child = entry.path();
        let dest_child = dest.join(entry.file_name());
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            // Backups never contain symlinks (the backup skips them); one in
            // the tree is unexpected and following it could write content
            // from OUTSIDE the backup. Skip loudly.
            summary
                .warnings
                .push(format!("skipped symlink {}", child.display()));
            continue;
        }
        if ft.is_dir() {
            restore_tree(&child, &dest_child, options, summary);
        } else if ft.is_file() {
            let differs = match (fs::metadata(&child), fs::metadata(&dest_child)) {
                (Ok(s), Ok(d)) => {
                    s.len() != d.len()
                        || !matches!(
                            (s.modified().ok(), d.modified().ok()),
                            (Some(a), Some(b)) if within_2s(a, b)
                        )
                }
                _ => false,
            };
            let exists = dest_child.exists();
            if exists && (!options.overwrite || !differs) {
                summary.files_skipped_existing += 1;
                continue;
            }
            let len = fs::metadata(&child).map(|m| m.len()).unwrap_or(0);
            if options.dry_run {
                println!("would copy {} -> {}", child.display(), dest_child.display());
                summary.files_copied += 1;
                summary.bytes_copied += len;
                continue;
            }
            if let Some(parent) = dest_child.parent() {
                if let Err(e) = fs::create_dir_all(parent) {
                    summary
                        .warnings
                        .push(format!("create {}: {e}", parent.display()));
                    continue;
                }
            }
            match fs::copy(&child, &dest_child) {
                Ok(n) => {
                    summary.files_copied += 1;
                    summary.bytes_copied += n;
                    // Preserve the backed-up mtime.
                    if let (Ok(m), Ok(f)) = (
                        fs::metadata(&child),
                        fs::OpenOptions::new().write(true).open(&dest_child),
                    ) {
                        if let Ok(t) = m.modified() {
                            let _ = f.set_modified(t);
                        }
                    }
                }
                Err(e) => summary
                    .warnings
                    .push(format!("copy {}: {e}", child.display())),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn write(p: &Path, content: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    #[test]
    fn backs_up_faithfully_and_incrementally() {
        let base = std::env::temp_dir().join("cursordump-backup-test");
        let _ = fs::remove_dir_all(&base);
        let root = base.join("projects");
        let out = base.join("backup");
        // Two fake projects with nested structure.
        write(
            &root.join("projA/agent-transcripts/s1/s1.jsonl"),
            "{\"role\":\"user\"}\n",
        );
        write(&root.join("projA/assets/img.png"), "PNGDATA");
        write(&root.join("projB/terminals/1.txt"), "term");

        let cursor_root = base.join("fake-cursor");
        let (tx, rx) = channel();
        run_backup(
            root.clone(),
            out.clone(),
            BackupOptions::default(),
            cursor_root.clone(),
            tx,
            || {},
        );
        let mut summary = None;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                BackupEvent::Done(s) => summary = Some(s),
                BackupEvent::Failed(e) => panic!("{e}"),
                _ => {}
            }
        }
        let s = summary.unwrap();
        assert_eq!(s.projects, 2);
        assert_eq!(s.files_copied, 3);
        assert!(out
            .join("projects/projA/agent-transcripts/s1/s1.jsonl")
            .is_file());
        assert!(out.join("projects/projA/assets/img.png").is_file());
        assert!(out.join("projects/projB/terminals/1.txt").is_file());
        assert!(out.join(MARKER).is_file());

        // Manifest records a transcript hash.
        let m: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(out.join(MARKER)).unwrap()).unwrap();
        assert_eq!(m["transcripts"].as_array().unwrap().len(), 1);

        // Second run is incremental: nothing changed -> all unchanged.
        let (tx2, rx2) = channel();
        run_backup(
            root.clone(),
            out.clone(),
            BackupOptions::default(),
            cursor_root,
            tx2,
            || {},
        );
        let mut s2 = None;
        while let Ok(ev) = rx2.try_recv() {
            if let BackupEvent::Done(s) = ev {
                s2 = Some(s);
            }
        }
        let s2 = s2.unwrap();
        assert_eq!(s2.files_copied, 0, "unchanged files must be skipped");
        assert_eq!(s2.files_unchanged, 3);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn skip_runtime_excludes_terminals_and_tools() {
        let base = std::env::temp_dir().join("cursordump-backup-skip-test");
        let _ = fs::remove_dir_all(&base);
        let root = base.join("projects");
        write(&root.join("p/agent-transcripts/s/s.jsonl"), "{}\n");
        write(&root.join("p/terminals/1.txt"), "x");
        write(&root.join("p/agent-tools/cache.txt"), "y");
        let (tx, rx) = channel();
        let opts = BackupOptions {
            skip_runtime: true,
            ..Default::default()
        };
        run_backup(
            root.clone(),
            base.join("bk"),
            opts,
            base.join("fc"),
            tx,
            || {},
        );
        while rx.try_recv().is_ok() {}
        assert!(base
            .join("bk/projects/p/agent-transcripts/s/s.jsonl")
            .is_file());
        assert!(!base.join("bk/projects/p/terminals").exists());
        assert!(!base.join("bk/projects/p/agent-tools").exists());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn captures_external_referenced_attachments() {
        let base = std::env::temp_dir().join("cursordump-backup-ext-test");
        let _ = fs::remove_dir_all(&base);
        let root = base.join("projects");
        // An external workspace file the session references by absolute path.
        let ext = base.join("workspace").join("diagram.png");
        write(&ext, "PNGDATA-EXTERNAL");
        let user = serde_json::json!({"role":"user","message":{"content":[
            {"type":"text","text": format!("<user_query>look at {}</user_query>", ext.display())}
        ]}})
        .to_string();
        write(
            &root.join("p/agent-transcripts/s/s.jsonl"),
            &format!("{user}\n"),
        );

        let out = base.join("bk");
        let (tx, rx) = channel();
        run_backup(
            root.clone(),
            out.clone(),
            BackupOptions::default(),
            base.join("fc"),
            tx,
            || {},
        );
        while rx.try_recv().is_ok() {}

        // The external file is captured under attachments/<hash-name>.
        let want = out
            .join("attachments")
            .join(crate::media::attachment_filename(&ext));
        assert!(want.is_file(), "external attachment captured");
        assert_eq!(fs::read_to_string(&want).unwrap(), "PNGDATA-EXTERNAL");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn subset_rerun_merges_manifest_instead_of_clobbering() {
        let base = std::env::temp_dir().join("cursordump-backup-merge-test");
        let _ = fs::remove_dir_all(&base);
        let root = base.join("projects");
        // Two projects, one with an external attachment reference.
        let ext = base.join("ws").join("chart.png");
        write(&ext, "EXTERNAL-PNG");
        let user_a = serde_json::json!({"role":"user","message":{"content":[
            {"type":"text","text": format!("<user_query>see {}</user_query>", ext.display())}
        ]}})
        .to_string();
        write(
            &root.join("projA/agent-transcripts/s1/s1.jsonl"),
            &format!("{user_a}\n"),
        );
        write(&root.join("projB/agent-transcripts/s2/s2.jsonl"), "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"<user_query>hi</user_query>\"}]}}\n");

        let out = base.join("bk");
        let run = |opts: BackupOptions| {
            let (tx, rx) = channel();
            run_backup(root.clone(), out.clone(), opts, base.join("fc"), tx, || {});
            while rx.try_recv().is_ok() {}
        };
        // Full run: both projects + attachment recorded.
        run(BackupOptions::default());
        let m: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(out.join(MARKER)).unwrap()).unwrap();
        assert_eq!(m["projects"].as_array().unwrap().len(), 2);
        assert_eq!(m["transcripts"].as_array().unwrap().len(), 2);
        assert_eq!(m["attachments"].as_array().unwrap().len(), 1);
        assert!(m["attachments"][0]["sha256"].as_str().is_some());

        // Subset re-run (projB only): projA's records MUST survive, and the
        // attachment (owned by projA) must still be listed with its hash.
        run(BackupOptions {
            projects: Some(vec!["projB".into()]),
            ..Default::default()
        });
        let m2: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(out.join(MARKER)).unwrap()).unwrap();
        assert_eq!(m2["projects"].as_array().unwrap().len(), 2, "projA kept");
        assert_eq!(
            m2["transcripts"].as_array().unwrap().len(),
            2,
            "projA hash kept"
        );
        assert_eq!(
            m2["attachments"].as_array().unwrap().len(),
            1,
            "attachment kept"
        );
        assert_eq!(m2["totals"]["projects"], 2);

        // Incremental full re-run: attachment still RECORDED (present) even
        // though nothing new was copied this run.
        run(BackupOptions::default());
        let m3: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(out.join(MARKER)).unwrap()).unwrap();
        assert_eq!(m3["attachments"].as_array().unwrap().len(), 1);
        assert_eq!(m3["last_run"]["external_attachments_copied"], 0);
        assert!(
            m3["attachments"][0]["sha256"].as_str().is_some(),
            "hash reused"
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn verify_detects_intact_and_tampered_backups() {
        let base = std::env::temp_dir().join("cursordump-verify-test");
        let _ = fs::remove_dir_all(&base);
        let root = base.join("projects");
        write(
            &root.join("p/agent-transcripts/s/s.jsonl"),
            "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"<user_query>hi</user_query>\"}]}}\n",
        );
        let out = base.join("bk");
        let (tx, rx) = channel();
        run_backup(
            root.clone(),
            out.clone(),
            BackupOptions::default(),
            base.join("fc"),
            tx,
            || {},
        );
        while rx.try_recv().is_ok() {}

        // Intact backup verifies clean.
        let r = verify_backup(&out).unwrap();
        assert!(r.is_ok(), "failed: {r:?}");
        assert_eq!(r.transcripts_ok, 1);

        // Tamper with the transcript -> verification must fail.
        let copy = out.join("projects/p/agent-transcripts/s/s.jsonl");
        fs::write(&copy, "tampered\n").unwrap();
        let r2 = verify_backup(&out).unwrap();
        assert!(!r2.is_ok());
        assert_eq!(r2.transcripts_failed.len(), 1);

        // Delete it -> reported missing.
        fs::remove_file(&copy).unwrap();
        let r3 = verify_backup(&out).unwrap();
        assert_eq!(r3.transcripts_missing.len(), 1);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn verify_rejects_malicious_and_incomplete_manifests() {
        let base = std::env::temp_dir().join("cursordump-verify-adv-test");
        let _ = fs::remove_dir_all(&base);
        let root = base.join("projects");
        write(
            &root.join("p/agent-transcripts/s/s.jsonl"),
            "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"<user_query>hi</user_query>\"}]}}\n",
        );
        let out = base.join("bk");
        let (tx, rx) = channel();
        run_backup(
            root.clone(),
            out.clone(),
            BackupOptions::default(),
            base.join("fc"),
            tx,
            || {},
        );
        while rx.try_recv().is_ok() {}

        // 1. A `..`-traversal path in the manifest must FAIL, not hash a file
        //    outside the backup.
        let outside = base.join("outside.jsonl");
        fs::write(&outside, "outside\n").unwrap();
        let mut m: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(out.join(MARKER)).unwrap()).unwrap();
        let evil_hash = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(b"outside\n");
            format!("{:x}", h.finalize())
        };
        m["transcripts"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "project": "p", "file": "outside.jsonl",
                "path": "../outside.jsonl", "sha256": evil_hash,
            }));
        fs::write(out.join(MARKER), m.to_string()).unwrap();
        let r = verify_backup(&out).unwrap();
        assert!(
            r.transcripts_failed
                .iter()
                .any(|f| f.contains("unsafe path")),
            "traversal path must be rejected: {r:?}"
        );

        // 2. A recorded `path` whose file is gone reports MISSING — no
        //    silent basename fallback against a different copy.
        let mut m: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(out.join(MARKER)).unwrap()).unwrap();
        m["transcripts"]
            .as_array_mut()
            .unwrap()
            .retain(|t| t["path"].as_str() != Some("../outside.jsonl"));
        // Duplicate same-basename file elsewhere (self-fork layout).
        let dupe = out.join("projects/p/agent-transcripts/other/subagents/s.jsonl");
        fs::create_dir_all(dupe.parent().unwrap()).unwrap();
        fs::copy(out.join("projects/p/agent-transcripts/s/s.jsonl"), &dupe).unwrap();
        fs::remove_file(out.join("projects/p/agent-transcripts/s/s.jsonl")).unwrap();
        fs::write(out.join(MARKER), m.to_string()).unwrap();
        let r2 = verify_backup(&out).unwrap();
        assert_eq!(
            r2.transcripts_missing.len(),
            1,
            "recorded path must not fall back to the dupe: {r2:?}"
        );

        // 3. The dupe itself is UNLISTED (present in tree, not in manifest).
        assert!(
            r2.unlisted.iter().any(|u| u.contains("subagents/s.jsonl")),
            "unlisted transcript must be flagged: {r2:?}"
        );
        assert!(!r2.is_ok());
        let _ = fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn restore_skips_symlinks_with_warning() {
        let base = std::env::temp_dir().join("cursordump-restore-symlink-test");
        let _ = fs::remove_dir_all(&base);
        // A "backup" containing a symlink pointing outside itself.
        let out = base.join("bk");
        write(&out.join("projects/p/real.txt"), "real");
        fs::write(out.join(MARKER), "{}").unwrap();
        let secret = base.join("secret.txt");
        fs::write(&secret, "secret").unwrap();
        std::os::unix::fs::symlink(&secret, out.join("projects/p/link.txt")).unwrap();

        let dest = base.join("dest");
        let r = restore_backup(&out, &dest, &RestoreOptions::default()).unwrap();
        assert!(dest.join("p/real.txt").is_file());
        assert!(
            !dest.join("p/link.txt").exists(),
            "symlink content must not be restored"
        );
        assert!(
            r.warnings.iter().any(|w| w.contains("symlink")),
            "skip must be loud: {r:?}"
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn restore_copies_missing_never_deletes_and_respects_dry_run() {
        let base = std::env::temp_dir().join("cursordump-restore-test");
        let _ = fs::remove_dir_all(&base);
        let root = base.join("projects");
        write(&root.join("p/agent-transcripts/s/s.jsonl"), "{}\n");
        write(&root.join("p/assets/a.png"), "PNG");
        let out = base.join("bk");
        let (tx, rx) = channel();
        run_backup(
            root.clone(),
            out.clone(),
            BackupOptions::default(),
            base.join("fc"),
            tx,
            || {},
        );
        while rx.try_recv().is_ok() {}

        // Destination has one file already (locally modified) and lacks one.
        let dest = base.join("restored-projects");
        write(&dest.join("p/assets/a.png"), "LOCALLY-CHANGED");

        // Dry run: reports 1 would-copy, writes nothing.
        let dry = restore_backup(
            &out,
            &dest,
            &RestoreOptions {
                dry_run: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(dry.files_copied, 1);
        assert!(!dest.join("p/agent-transcripts/s/s.jsonl").exists());

        // Real run (default): copies the missing transcript, leaves the
        // locally-changed file alone.
        let r = restore_backup(&out, &dest, &RestoreOptions::default()).unwrap();
        assert_eq!(r.files_copied, 1);
        assert!(dest.join("p/agent-transcripts/s/s.jsonl").is_file());
        assert_eq!(
            fs::read_to_string(dest.join("p/assets/a.png")).unwrap(),
            "LOCALLY-CHANGED",
            "existing files are never overwritten by default"
        );

        // With overwrite: the differing file is replaced from the backup.
        let r2 = restore_backup(
            &out,
            &dest,
            &RestoreOptions {
                overwrite: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(r2.files_copied, 1);
        assert_eq!(
            fs::read_to_string(dest.join("p/assets/a.png")).unwrap(),
            "PNG"
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn refuses_backup_inside_cursor() {
        let cursor = dirs::home_dir().unwrap().join(".cursor");
        // Containment applies where ~/.cursor exists (CI machines have none).
        if cursor.is_dir() {
            assert!(validate_backup_dir(&cursor.join("projects/backup"), &cursor).is_err());
        }
        assert!(validate_backup_dir(&std::env::temp_dir().join("cd-bk-ok"), &cursor).is_ok());
    }
}
