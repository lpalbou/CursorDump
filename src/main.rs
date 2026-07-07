//! CursorDump — explore Cursor agent sessions and export SFT/CPT datasets.
//!
//! Web UI by default (local server, opens the browser); a headless mode
//! supports scripted exports:
//!
//! ```text
//! cursordump                                   # web UI on ~/.cursor/projects
//! cursordump --port 7071 --no-open [<root>]    # custom port / don't open browser
//! cursordump export --project <slug> --out <dir> [--all-formats]
//!                    [--include-subagents] [--tool-calls] [--raw-user] [--no-clean]
//! ```

use cursordump::export::{run_export, ExportEvent, ExportOptions, SubagentMode, UserContent};
use cursordump::{scanner, server};

const HELP: &str =
    "CursorDump — explore Cursor agent sessions; export SFT/CPT datasets; full backups.

USAGE:
  cursordump [--port N] [--no-open] [<projects-root>]   start the web UI (default)
  cursordump export --project <slug> --out <dir> [options]
  cursordump backup --out <dir> [--project <slug>]... [options]

EXPORT OPTIONS:
  --all-formats                       sft_chatml + sft_sharegpt + cpt + cpt_txt
  --subagent-mode inline|separate|drop   (default inline)
  --include-subagents                 shorthand for --subagent-mode separate
  --thinking tagged|verbatim|strip    (default tagged: <think> blocks)
  --val <fraction>                    validation split, e.g. 0.1
  --min-turns N                       skip sessions with fewer trainable turns
  --tool-calls --raw-user --no-clean --no-media --no-metadata --final-only

BACKUP OPTIONS:
  --project <slug>                    repeatable; default = all projects
  --skip-runtime                      omit terminals/ and agent-tools/
  --no-verify --no-app --no-attachments

The web UI serves on 127.0.0.1 only and never writes to ~/.cursor.";

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args
        .iter()
        .any(|a| a == "--help" || a == "-h" || a == "help")
    {
        println!("{HELP}");
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("export") {
        headless_export(&args[1..]);
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("backup") {
        headless_backup(&args[1..]);
        return Ok(());
    }

    let mut port: u16 = 7070;
    let mut open_browser = true;
    let mut root_arg: Option<std::path::PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--port" => {
                port = args.get(i + 1).and_then(|p| p.parse().ok()).unwrap_or(7070);
                i += 2;
            }
            "--no-open" => {
                open_browser = false;
                i += 1;
            }
            other => {
                root_arg = Some(std::path::PathBuf::from(other));
                i += 1;
            }
        }
    }

    let root = root_arg
        .or_else(scanner::default_root)
        .expect("cannot determine ~/.cursor/projects; pass a path as argument");
    server::run(root, port, open_browser)
}

fn headless_export(args: &[String]) {
    let mut project: Option<String> = None;
    let mut out: Option<std::path::PathBuf> = None;
    let mut options = ExportOptions::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--project" => {
                project = args.get(i + 1).cloned();
                i += 2;
            }
            "--out" => {
                out = args.get(i + 1).map(std::path::PathBuf::from);
                i += 2;
            }
            "--all-formats" => {
                options.sft_chatml = true;
                options.sft_sharegpt = true;
                options.cpt_jsonl = true;
                options.cpt_txt = true;
                i += 1;
            }
            "--subagent-mode" => {
                match args.get(i + 1).map(String::as_str) {
                    Some("separate") => options.subagent_mode = SubagentMode::Separate,
                    Some("drop") => options.subagent_mode = SubagentMode::Drop,
                    Some("inline") => options.subagent_mode = SubagentMode::Inline,
                    other => {
                        eprintln!("--subagent-mode expects inline|separate|drop, got {other:?}");
                        std::process::exit(2);
                    }
                }
                i += 2;
            }
            "--include-subagents" => {
                // Explicit request to export subagents as records => Separate,
                // mirroring the web UI (avoids the inline+separate hybrid that
                // would export the same content twice).
                options.subagent_mode = SubagentMode::Separate;
                i += 1;
            }
            "--tool-calls" => {
                options.include_tool_calls = true;
                i += 1;
            }
            "--raw-user" => {
                options.user_content = UserContent::RawFull;
                i += 1;
            }
            "--no-clean" => {
                options.clean_assistant = false;
                i += 1;
            }
            "--thinking" => {
                options.thinking = match args.get(i + 1).map(String::as_str) {
                    Some("strip") => cursordump::export::ThinkingMode::Strip,
                    Some("verbatim") => cursordump::export::ThinkingMode::Verbatim,
                    Some("tagged") => cursordump::export::ThinkingMode::Tagged,
                    other => {
                        eprintln!("--thinking expects tagged|verbatim|strip, got {other:?}");
                        std::process::exit(2);
                    }
                };
                i += 2;
            }
            "--val" => {
                options.val_fraction =
                    args.get(i + 1)
                        .and_then(|v| v.parse().ok())
                        .unwrap_or_else(|| {
                            eprintln!("--val expects a fraction (e.g. 0.1)");
                            std::process::exit(2);
                        });
                i += 2;
            }
            "--min-turns" => {
                options.min_turns =
                    args.get(i + 1)
                        .and_then(|v| v.parse().ok())
                        .unwrap_or_else(|| {
                            eprintln!("--min-turns expects a number");
                            std::process::exit(2);
                        });
                i += 2;
            }
            "--no-media" => {
                options.copy_media = false;
                i += 1;
            }
            "--no-metadata" => {
                options.with_metadata = false;
                i += 1;
            }
            "--final-only" => {
                options.final_response_only = true;
                i += 1;
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }
    // Separate mode requires subagent transcripts in the record set.
    if options.subagent_mode == SubagentMode::Separate {
        options.include_subagent_sessions = true;
    }
    let (Some(project), Some(out)) = (project, out) else {
        eprintln!(
            "usage: cursordump export --project <slug> --out <dir>\n  \
             [--all-formats] [--subagent-mode inline|separate|drop] [--include-subagents]\n  \
             [--thinking tagged|verbatim|strip] [--val <fraction>] [--min-turns N]\n  \
             [--tool-calls] [--raw-user] [--no-clean] [--no-media] [--no-metadata] [--final-only]"
        );
        std::process::exit(2);
    };

    let root = scanner::default_root().expect("no home dir");
    let cursor_root = root
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| root.clone());
    let projects = scanner::scan_projects(&root);
    let Some(p) = projects.iter().find(|p| p.slug == project) else {
        eprintln!("project not found: {project}");
        eprintln!(
            "available: {}",
            projects
                .iter()
                .filter(|p| !p.sessions.is_empty())
                .map(|p| p.slug.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        std::process::exit(1);
    };

    let (tx, rx) = std::sync::mpsc::channel();
    run_export(p.sessions.clone(), out, options, cursor_root, tx, || {});
    while let Ok(ev) = rx.try_recv() {
        match ev {
            ExportEvent::Progress { .. } => {}
            ExportEvent::Done(s) => {
                println!(
                    "exported {} session(s) ({} skipped) -> {}",
                    s.sessions_exported,
                    s.sessions_skipped,
                    s.out_dir.display()
                );
                println!(
                    "sft records: {}, cpt records: {}, media: {}/{} copied",
                    s.sft_records, s.cpt_records, s.media_copied, s.media_referenced
                );
                for w in &s.warnings {
                    eprintln!("warning: {w}");
                }
            }
            ExportEvent::Failed(e) => {
                eprintln!("export failed: {e}");
                std::process::exit(1);
            }
        }
    }
}

fn headless_backup(args: &[String]) {
    use cursordump::backup::{run_backup, BackupEvent, BackupOptions};
    let mut out: Option<std::path::PathBuf> = None;
    let mut projects: Vec<String> = Vec::new();
    let mut options = BackupOptions::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                out = args.get(i + 1).map(std::path::PathBuf::from);
                i += 2;
            }
            "--project" => {
                if let Some(p) = args.get(i + 1) {
                    projects.push(p.clone());
                }
                i += 2;
            }
            "--skip-runtime" => {
                options.skip_runtime = true;
                i += 1;
            }
            "--no-verify" => {
                options.verify_transcripts = false;
                i += 1;
            }
            "--no-app" => {
                options.include_app = false;
                i += 1;
            }
            "--no-attachments" => {
                options.include_external_attachments = false;
                i += 1;
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }
    let Some(out) = out else {
        eprintln!("usage: cursordump backup --out <dir> [--project <slug>]... [--skip-runtime] [--no-verify] [--no-app] [--no-attachments]");
        std::process::exit(2);
    };
    if !projects.is_empty() {
        options.projects = Some(projects);
    }

    let root = scanner::default_root().expect("no home dir");
    let cursor_root = root
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| root.clone());
    let (tx, rx) = std::sync::mpsc::channel();
    run_backup(root, out, options, cursor_root, tx, || {});
    while let Ok(ev) = rx.try_recv() {
        match ev {
            BackupEvent::Progress { done, total, stage } => {
                eprintln!("[{done}/{total}] {stage}");
            }
            BackupEvent::Done(s) => {
                println!(
                    "backed up {} project(s): {} files copied, {} unchanged, {:.1} MB total -> {}",
                    s.projects,
                    s.files_copied,
                    s.files_unchanged,
                    s.bytes_total as f64 / 1_048_576.0,
                    s.out_dir.display()
                );
                for w in s.warnings.iter().take(10) {
                    eprintln!("warning: {w}");
                }
            }
            BackupEvent::Failed(e) => {
                eprintln!("backup failed: {e}");
                std::process::exit(1);
            }
        }
    }
}
