//! End-to-end export against real Cursor data when available.
//!
//! Reads from ~/.cursor/projects (READ ONLY) and writes to a temp dir.
//! Skips silently on machines without Cursor data.

use std::sync::mpsc::channel;

use cursordump::export::{run_export, ExportEvent, ExportOptions};
use cursordump::scanner;

/// An export must serialize EVERY attachment the user referenced and that
/// still exists — both files inside the projects root and external
/// workspace `@file`s — into `media/`, with manifest entries.
#[test]
fn export_copies_internal_and_external_attachments() {
    let base = std::env::temp_dir().join("cursordump-export-attach-test");
    let _ = std::fs::remove_dir_all(&base);
    let cursor_root = base.join("fake-cursor");
    let root = cursor_root.join("projects");

    // Internal attachment (inside the projects root) + external workspace file.
    let internal = root.join("proj/assets/shot.png");
    let external = base.join("workspace/diagram.png");
    let gone = base.join("workspace/deleted.png"); // referenced but missing
    std::fs::create_dir_all(internal.parent().unwrap()).unwrap();
    std::fs::create_dir_all(external.parent().unwrap()).unwrap();
    std::fs::write(&internal, "PNG-INTERNAL").unwrap();
    std::fs::write(&external, "PNG-EXTERNAL").unwrap();

    let transcript = root.join("proj/agent-transcripts/s1/s1.jsonl");
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    let user = serde_json::json!({"role":"user","message":{"content":[{"type":"text","text":
        format!("<user_query>see {} and {} and {}</user_query>",
            internal.display(), external.display(), gone.display())}]}});
    let asst = serde_json::json!({"role":"assistant","message":{"content":[{"type":"text","text":"Looked at both images; the diagram matches the screenshot."}]}});
    std::fs::write(&transcript, format!("{user}\n{asst}\n")).unwrap();

    let meta = cursordump::model::SessionMeta {
        id: "s1".into(),
        project_slug: "proj".into(),
        path: transcript,
        title: "t".into(),
        modified: None,
        size_bytes: 0,
        is_subagent: false,
        parent_id: None,
    };
    let out = base.join("dump");
    let (tx, rx) = channel();
    run_export(
        vec![meta],
        out.clone(),
        ExportOptions::default(),
        cursor_root,
        tx,
        || {},
    );
    let mut summary = None;
    while let Ok(ev) = rx.try_recv() {
        match ev {
            ExportEvent::Done(s) => summary = Some(s),
            ExportEvent::Failed(e) => panic!("export failed: {e}"),
            ExportEvent::Progress { .. } => {}
        }
    }
    let summary = summary.expect("export completes");
    assert_eq!(summary.media_referenced, 3);
    assert_eq!(
        summary.media_copied, 2,
        "both existing attachments serialized"
    );

    // Both files exist under media/ with their content intact.
    let media: Vec<_> = std::fs::read_dir(out.join("media"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    assert_eq!(media.len(), 2);
    let contents: Vec<String> = media
        .iter()
        .map(|p| std::fs::read_to_string(p).unwrap())
        .collect();
    assert!(contents.iter().any(|c| c == "PNG-INTERNAL"));
    assert!(contents.iter().any(|c| c == "PNG-EXTERNAL"));

    // Manifest records origin and copy status per reference.
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("manifest.json")).unwrap()).unwrap();
    let entries = manifest["media"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    let by_path = |needle: &str| {
        entries
            .iter()
            .find(|e| e["original_path"].as_str().unwrap().contains(needle))
            .unwrap()
    };
    assert_eq!(by_path("shot.png")["origin"], "cursor");
    assert!(by_path("shot.png")["sha256"].is_string());
    assert_eq!(by_path("diagram.png")["origin"], "external");
    assert!(by_path("diagram.png")["copied_to"].is_string());
    assert_eq!(by_path("deleted.png")["exists"], false);
    assert!(by_path("deleted.png")["copied_to"].is_null());

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn full_export_roundtrip_on_real_data() {
    let Some(root) = scanner::default_root().filter(|r| r.is_dir()) else {
        eprintln!("no ~/.cursor/projects — skipping");
        return;
    };
    let projects = scanner::scan_projects(&root);
    let sessions: Vec<_> = projects
        .iter()
        .flat_map(|p| p.sessions.iter().cloned())
        .filter(|s| !s.is_subagent)
        .take(10)
        .collect();
    if sessions.is_empty() {
        eprintln!("no sessions found — skipping");
        return;
    }

    let out = std::env::temp_dir().join("cursordump-e2e");
    let _ = std::fs::remove_dir_all(&out);
    let options = ExportOptions {
        sft_chatml: true,
        sft_sharegpt: true,
        cpt_jsonl: true,
        cpt_txt: true,
        val_fraction: 0.2,
        ..Default::default()
    };
    let cursor_root = root.parent().unwrap().to_path_buf();
    let (tx, rx) = channel();
    run_export(sessions, out.clone(), options, cursor_root, tx, || {});

    let mut summary = None;
    while let Ok(ev) = rx.try_recv() {
        match ev {
            ExportEvent::Done(s) => summary = Some(s),
            ExportEvent::Failed(e) => panic!("export failed: {e}"),
            ExportEvent::Progress { .. } => {}
        }
    }
    let summary = summary.expect("export must complete");
    assert!(summary.sessions_exported > 0, "no sessions exported");

    // Structural validation of outputs.
    for sub in ["sft_chatml", "sft_sharegpt", "cpt"] {
        let train = out.join(sub).join("train.jsonl");
        assert!(train.is_file(), "{sub}/train.jsonl missing");
        let content = std::fs::read_to_string(&train).unwrap();
        let mut keys: Option<Vec<String>> = None;
        for line in content.lines() {
            let v: serde_json::Value = serde_json::from_str(line).expect("valid json line");
            let obj = v.as_object().expect("object record");
            let mut k: Vec<String> = obj.keys().cloned().collect();
            k.sort();
            match &keys {
                None => keys = Some(k),
                Some(prev) => assert_eq!(prev, &k, "inconsistent schema in {sub}"),
            }
            match sub {
                "sft_chatml" => {
                    let msgs = v["messages"].as_array().unwrap();
                    assert!(!msgs.is_empty());
                    for (i, m) in msgs.iter().enumerate() {
                        let expected = if i % 2 == 0 { "user" } else { "assistant" };
                        assert_eq!(m["role"], expected, "roles must alternate");
                        assert!(!m["content"].as_str().unwrap().trim().is_empty());
                    }
                }
                "sft_sharegpt" => {
                    let conv = v["conversations"].as_array().unwrap();
                    for (i, m) in conv.iter().enumerate() {
                        let expected = if i % 2 == 0 { "human" } else { "gpt" };
                        assert_eq!(m["from"], expected);
                    }
                }
                "cpt" => {
                    assert!(!v["text"].as_str().unwrap().trim().is_empty());
                }
                _ => unreachable!(),
            }
        }
    }

    // Manifest written last, with matching line counts.
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("manifest.json")).unwrap()).unwrap();
    let counts = manifest["file_line_counts"].as_object().unwrap();
    for (file, expected) in counts {
        let actual = std::fs::read_to_string(out.join(file))
            .unwrap()
            .lines()
            .count();
        assert_eq!(
            actual as u64,
            expected.as_u64().unwrap(),
            "count mismatch {file}"
        );
    }
    assert!(out.join("README.md").is_file());

    let _ = std::fs::remove_dir_all(&out);
}
