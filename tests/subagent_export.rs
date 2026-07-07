//! Subagent linkage + inline/separate export behavior, using a synthetic
//! master+subagent fixture written to a temp dir (never touches ~/.cursor).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;

use cursordump::export::{run_export, ExportEvent, ExportOptions, ExportSummary, SubagentMode};
use cursordump::model::SessionMeta;
use serde_json::json;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("cursordump-sub-{name}"));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// Build a master transcript that spawns one foreground + one background Task,
/// plus the matching subagent transcripts under `subagents/`.
fn build_fixture(root: &Path) -> SessionMeta {
    let master_id = "11111111-1111-1111-1111-111111111111";
    let tdir = root.join("agent-transcripts").join(master_id);
    fs::create_dir_all(tdir.join("subagents")).unwrap();

    let fg_prompt = "Explore the parser module and report findings.";
    let bg_prompt = "Generate images in the background.";

    let master = [
        json!({"role":"user","message":{"content":[{"type":"text","text":"<user_query>Audit the parser and fix bugs.</user_query>"}]}}).to_string(),
        json!({"role":"assistant","message":{"content":[
            {"type":"text","text":"I'll delegate exploration first."},
            {"type":"tool_use","name":"Task","input":{"subagent_type":"explore","description":"Explore parser","prompt":fg_prompt,"run_in_background":false}},
            {"type":"tool_use","name":"Task","input":{"subagent_type":"shell","description":"Make images","prompt":bg_prompt,"run_in_background":true}}
        ]}}).to_string(),
        json!({"role":"user","message":{"content":[{"type":"text","text":"<user_query>The beginning of the above subagent result is already visible to the user. Perform any follow-up actions.</user_query>"}]}}).to_string(),
        json!({"role":"assistant","message":{"content":[{"type":"text","text":"Based on the exploration, the off-by-one is in chunk_turns; I fixed it and tests pass."}]}}).to_string(),
    ].join("\n");
    fs::write(tdir.join(format!("{master_id}.jsonl")), master).unwrap();

    // Foreground subagent: first user_query == fg_prompt.
    let fg = [
        json!({"role":"user","message":{"content":[{"type":"text","text":format!("<user_query>{fg_prompt}</user_query>")}]}}).to_string(),
        json!({"role":"assistant","message":{"content":[{"type":"text","text":"The parser splits turns on real user records. The chunker has an off-by-one at the boundary."}]}}).to_string(),
    ].join("\n");
    fs::write(
        tdir.join("subagents")
            .join("22222222-2222-2222-2222-222222222222.jsonl"),
        fg,
    )
    .unwrap();

    // Background subagent.
    let bg = [
        json!({"role":"user","message":{"content":[{"type":"text","text":format!("<user_query>{bg_prompt}</user_query>")}]}}).to_string(),
        json!({"role":"assistant","message":{"content":[{"type":"text","text":"Generated 5 images into assets/."}]}}).to_string(),
    ].join("\n");
    fs::write(
        tdir.join("subagents")
            .join("33333333-3333-3333-3333-333333333333.jsonl"),
        bg,
    )
    .unwrap();

    SessionMeta {
        id: master_id.into(),
        project_slug: "proj".into(),
        path: tdir.join(format!("{master_id}.jsonl")),
        title: "Audit the parser".into(),
        modified: None,
        size_bytes: 0,
        is_subagent: false,
        parent_id: None,
    }
}

fn do_export(master: SessionMeta, out: &Path, options: ExportOptions) -> ExportSummary {
    let (tx, rx) = channel();
    // cursor_root is a scratch parent that is NOT ~/.cursor.
    let cursor_root = out.join("fake-cursor-root");
    run_export(
        vec![master],
        out.to_path_buf(),
        options,
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
    summary.expect("done")
}

#[test]
fn inline_mode_splices_foreground_result_not_background() {
    let root = scratch("inline");
    let master = build_fixture(&root);
    let out = root.join("out");
    let options = ExportOptions {
        sft_chatml: true,
        include_tool_calls: true,
        subagent_mode: SubagentMode::Inline,
        ..Default::default()
    };
    let summary = do_export(master, &out, options);
    assert_eq!(summary.sessions_exported, 1, "only the master is a record");

    let content = fs::read_to_string(out.join("sft_chatml").join("train.jsonl")).unwrap();
    // Foreground result inlined.
    assert!(
        content.contains("off-by-one at the boundary"),
        "fg result inlined"
    );
    assert!(content.contains("tool_result"), "result rendered");
    // Background result NOT inlined; marked spawned_in_background.
    assert!(
        content.contains("spawned_in_background"),
        "bg marked, not inlined"
    );
    assert!(
        !content.contains("Generated 5 images"),
        "background result must not be spliced in"
    );
    // Metadata records the linkage.
    let rec: serde_json::Value = serde_json::from_str(content.lines().next().unwrap()).unwrap();
    let calls = rec["metadata"]["task_calls"].as_array().unwrap();
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().any(|c| c["match"] == "exact"));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn separate_mode_exports_subagents_as_records() {
    let root = scratch("separate");
    let master = build_fixture(&root);
    let out = root.join("out");
    let options = ExportOptions {
        sft_chatml: true,
        subagent_mode: SubagentMode::Separate,
        ..Default::default()
    };
    // Provide the subagent metas too (the GUI would pass all basket sessions).
    let sub_dir = master.path.parent().unwrap().join("subagents");
    let subs: Vec<SessionMeta> = fs::read_dir(&sub_dir)
        .unwrap()
        .flatten()
        .map(|e| SessionMeta {
            id: e.path().file_stem().unwrap().to_string_lossy().to_string(),
            project_slug: "proj".into(),
            path: e.path(),
            title: "sub".into(),
            modified: None,
            size_bytes: 0,
            is_subagent: true,
            parent_id: Some(master.id.clone()),
        })
        .collect();
    let mut all = vec![master];
    all.extend(subs);

    let (tx, rx) = channel();
    let cursor_root = out.join("fake-cursor-root");
    run_export(all, out.clone(), options, cursor_root, tx, || {});
    let mut summary = None;
    while let Ok(ev) = rx.try_recv() {
        if let ExportEvent::Done(s) = ev {
            summary = Some(s);
        }
    }
    let summary = summary.unwrap();
    assert_eq!(summary.sessions_exported, 3, "master + 2 subagents");

    let content = fs::read_to_string(out.join("sft_chatml").join("train.jsonl")).unwrap();
    // Subagent records are tagged as agent briefs.
    assert!(content.contains("\"user_kind\":\"agent_brief\""));
    let _ = fs::remove_dir_all(&root);
}
