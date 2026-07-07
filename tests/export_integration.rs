//! End-to-end export against real Cursor data when available.
//!
//! Reads from ~/.cursor/projects (READ ONLY) and writes to a temp dir.
//! Skips silently on machines without Cursor data.

use std::sync::mpsc::channel;

use cursordump::export::{run_export, ExportEvent, ExportOptions};
use cursordump::scanner;

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
