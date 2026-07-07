//! Adversarial robustness tests: hostile inputs against parser, media
//! extraction, export and scanner.
//!
//! Every test asserts the CURRENT observed behavior so the suite stays green;
//! assertions that document a known gap (wrong-but-tolerated result) are
//! marked with `GAP:` comments. See the accompanying robustness report.
//!
//! Fixtures are generated at runtime under the system temp dir — nothing is
//! ever written beneath ~/.cursor.

use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::time::Instant;

use serde_json::json;

use cursordump::export::{
    run_export, turn_is_trainable, validate_out_dir, ExportEvent, ExportOptions, ExportSummary,
};
use cursordump::media::extract_media_refs;
use cursordump::model::{Block, ParsedSession, SessionMeta};
use cursordump::parser::{extract_user_query, parse_content, parse_session};
use cursordump::scanner;

// ---------------------------------------------------------------- helpers

fn meta_at(path: PathBuf) -> SessionMeta {
    SessionMeta {
        id: "adv".into(),
        project_slug: "advproj".into(),
        path,
        title: "adversarial".into(),
        modified: None,
        size_bytes: 0,
        is_subagent: false,
        parent_id: None,
    }
}

fn parse(content: &str) -> ParsedSession {
    parse_content(meta_at(PathBuf::from("/dev/null")), content)
}

/// Build a syntactically valid user record line (escaping handled by serde).
fn user_line(text: &str) -> String {
    json!({"role":"user","message":{"content":[{"type":"text","text":text}]}}).to_string()
}

fn asst_line(text: &str) -> String {
    json!({"role":"assistant","message":{"content":[{"type":"text","text":text}]}}).to_string()
}

/// Unique scratch dir under the system temp dir (never ~/.cursor).
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cursordump-adv-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn drain_export(rx: &std::sync::mpsc::Receiver<ExportEvent>) -> ExportSummary {
    let mut summary = None;
    while let Ok(ev) = rx.try_recv() {
        match ev {
            ExportEvent::Done(s) => summary = Some(s),
            ExportEvent::Failed(e) => panic!("export failed: {e}"),
            ExportEvent::Progress { .. } => {}
        }
    }
    summary.expect("export must send Done")
}

// ------------------------------------------------------ 1. hostile shapes

#[test]
fn parser_survives_hostile_scalar_shapes() {
    // content as string / null / number / object; blocks missing "type";
    // text null / number; tool_use missing name and input.
    let lines = [
        r#"{"role":"user","message":{"content":"plain string content"}}"#.to_string(),
        r#"{"role":"user","message":{"content":null}}"#.to_string(),
        r#"{"role":"user","message":{"content":42}}"#.to_string(),
        r#"{"role":"user","message":{"content":{"foo":"bar"}}}"#.to_string(),
        r#"{"role":"assistant","message":{"content":[{"text":"no type field"}]}}"#.to_string(),
        r#"{"role":"assistant","message":{"content":[{"type":"text","text":null}]}}"#.to_string(),
        r#"{"role":"assistant","message":{"content":[{"type":"text","text":123}]}}"#.to_string(),
        r#"{"role":"assistant","message":{"content":[{"type":"tool_use"}]}}"#.to_string(),
        r#"{"role":"assistant","message":{"content":[{"type":"tool_use","input":"str-input"}]}}"#
            .to_string(),
    ];
    let parsed = parse(&lines.join("\n"));
    assert_eq!(parsed.messages.len(), 9, "all records accepted, no panic");
    assert_eq!(parsed.skipped_lines, 0);

    // String content becomes one text block; null/number/object become none.
    assert!(
        matches!(&parsed.messages[0].blocks[..], [Block::Text(t)] if t == "plain string content")
    );
    for i in 1..4 {
        assert!(
            parsed.messages[i].blocks.is_empty(),
            "record {i} has no blocks"
        );
    }
    // Missing "type" is preserved as Other("").
    assert!(matches!(&parsed.messages[4].blocks[..], [Block::Other(t)] if t.is_empty()));
    // Null/number text degrade to empty text blocks (content silently lost).
    assert!(matches!(&parsed.messages[5].blocks[..], [Block::Text(t)] if t.is_empty()));
    assert!(matches!(&parsed.messages[6].blocks[..], [Block::Text(t)] if t.is_empty()));
    // tool_use without name/input gets placeholders.
    assert!(matches!(&parsed.messages[7].blocks[..],
        [Block::ToolUse { name, input }] if name == "(unnamed tool)" && input.is_null()));
    assert!(matches!(&parsed.messages[8].blocks[..],
        [Block::ToolUse { name, .. }] if name == "(unnamed tool)"));
}

#[test]
fn parser_torn_json_lines_are_skipped() {
    let content = concat!(
        r#"{"role":"user","message":{"content":[{"type":"te"#,
        "\n", // torn mid-key
        r#"{"role":"assistant","#,
        "\n", // torn early
        r#"{"role":"user","message":{}}{"trailing":"garbage"}"#,
        "\n", // trailing junk
        "not json at all\n",
        "\n",       // empty line: ignored, not counted
        "   \t \n", // whitespace-only: ignored, not counted
    );
    let parsed = parse(content);
    assert_eq!(parsed.messages.len(), 0);
    assert_eq!(
        parsed.skipped_lines, 4,
        "torn/garbage counted, blank lines not"
    );
}

#[test]
fn parser_crlf_and_blank_lines_are_fine() {
    let content = format!(
        "{}\r\n\r\n{}\r\n",
        user_line("<user_query>crlf query</user_query>"),
        asst_line("answer")
    );
    let parsed = parse(&content);
    assert_eq!(parsed.messages.len(), 2);
    assert_eq!(parsed.skipped_lines, 0);
    assert_eq!(parsed.messages[0].user_query.as_deref(), Some("crlf query"));
}

#[test]
fn parser_bom_is_stripped_first_record_survives() {
    // FIXED GAP: a UTF-8 BOM before the first record is now trimmed, so the
    // first record — typically the first user query — parses normally.
    let content = format!(
        "\u{FEFF}{}\n{}\n",
        user_line("<user_query>survives BOM</user_query>"),
        asst_line("survives")
    );
    let parsed = parse(&content);
    assert_eq!(parsed.skipped_lines, 0);
    assert_eq!(parsed.messages.len(), 2);
    assert_eq!(
        parsed.messages[0].user_query.as_deref(),
        Some("survives BOM")
    );
}

#[test]
fn parser_deep_nesting_rejected_not_overflowing() {
    // serde_json's default recursion limit (128) rejects the line instead of
    // overflowing the stack — the whole record is lost but nothing crashes.
    let deep = format!(
        r#"{{"role":"user","message":{{"content":{}{}}}}}"#,
        "[".repeat(1000),
        "]".repeat(1000)
    );
    let bare = format!("{}{}", "[".repeat(1000), "]".repeat(1000));
    let content = format!(
        "{deep}\n{bare}\n{}",
        user_line("<user_query>ok</user_query>")
    );
    let parsed = parse(&content);
    assert_eq!(
        parsed.skipped_lines, 2,
        "deeply nested lines rejected, not crashed"
    );
    assert_eq!(parsed.messages.len(), 1);

    // Moderate nesting (100 < 128) parses fine inside tool_use input.
    let nested_input = format!("{}{}", "[".repeat(100), "]".repeat(100));
    let line = format!(
        r#"{{"role":"assistant","message":{{"content":[{{"type":"tool_use","name":"t","input":{nested_input}}}]}}}}"#
    );
    let parsed = parse(&line);
    assert_eq!(parsed.messages.len(), 1);
    assert_eq!(parsed.messages[0].tool_calls().count(), 1);
}

#[test]
fn parser_multi_mb_text_block() {
    let big = "a".repeat(8 * 1024 * 1024);
    let content = format!(
        "{}\n{}",
        user_line(&format!("<user_query>big {big}</user_query>")),
        asst_line("ok")
    );
    let start = Instant::now();
    let parsed = parse(&content);
    let elapsed = start.elapsed();
    assert_eq!(parsed.messages.len(), 2);
    let q = parsed.messages[0].user_query.as_ref().unwrap();
    assert_eq!(q.len(), "big ".len() + big.len());
    assert!(elapsed.as_secs() < 10, "8MB block parsed in {elapsed:?}");
}

// ------------------------------------------------- 3. user_query tag abuse

#[test]
fn user_query_nested_open_tag_is_kept_verbatim() {
    // GAP (cosmetic): a literal nested open tag is kept inside the extracted
    // query and the trailing "c" after the first close tag is dropped.
    let text = "<user_query>a<user_query>b</user_query>c</user_query>";
    assert_eq!(extract_user_query(text).as_deref(), Some("a<user_query>b"));
}

#[test]
fn user_query_unclosed_at_eof_takes_rest() {
    let text = "prefix<user_query>torn write, no close";
    assert_eq!(
        extract_user_query(text).as_deref(),
        Some("torn write, no close")
    );
}

#[test]
fn user_query_spanning_two_text_blocks_is_found() {
    // Tag opened in one text block and closed in the next: blocks_text joins
    // with '\n', so extraction sees the full tag. Not missed.
    let line = json!({"role":"user","message":{"content":[
        {"type":"text","text":"<user_query>first half"},
        {"type":"text","text":"second half</user_query>"}
    ]}})
    .to_string();
    let parsed = parse(&line);
    assert_eq!(
        parsed.messages[0].user_query.as_deref(),
        Some("first half\nsecond half")
    );
}

#[test]
fn user_query_empty_tag_yields_no_query_and_untrainable_turn() {
    // FIXED GAP: an all-empty tag now yields user_query == None (and the
    // record is treated as injected). Export drops the turn.
    let content = format!(
        "{}\n{}",
        user_line("<user_query>   </user_query>"),
        asst_line("hello")
    );
    let parsed = parse(&content);
    assert!(!parsed.messages[0].has_user_query);
    assert_eq!(parsed.messages[0].user_query, None);
    assert!(parsed.messages[0].is_injected);
    assert_eq!(parsed.turns.len(), 1);
    assert!(!turn_is_trainable(
        &parsed.turns[0],
        &ExportOptions::default()
    ));
}

#[test]
fn user_query_literal_close_tag_in_code_fence_truncates() {
    // GAP: extraction is not fence-aware. A literal </user_query> inside a
    // code fence ends extraction early; the remainder of the real query is
    // dropped from the extracted text.
    let text = "<user_query>please print ```\n</user_query>\n``` in your answer</user_query>";
    assert_eq!(
        extract_user_query(text).as_deref(),
        Some("please print ```")
    );
}

// ------------------------------------------------------- 4. invalid UTF-8

#[test]
fn parse_session_invalid_utf8_is_recovered_lossily() {
    // FIXED GAP: invalid UTF-8 (e.g. a torn multi-byte write from a live
    // session) no longer discards the whole session; valid records survive
    // and the mangled line is counted as skipped.
    let dir = scratch("utf8");
    let path = dir.join("bad.jsonl");
    let mut bytes = user_line("<user_query>recovered</user_query>").into_bytes();
    bytes.push(b'\n');
    bytes.extend_from_slice(&[0xFF, 0xFE, 0xFF, b'\n']);
    fs::write(&path, &bytes).unwrap();

    let parsed = parse_session(&meta_at(path));
    assert_eq!(parsed.messages.len(), 1, "valid record recovered");
    assert_eq!(parsed.messages[0].user_query.as_deref(), Some("recovered"));
    assert_eq!(parsed.skipped_lines, 1, "mangled line counted");
    let _ = fs::remove_dir_all(&dir);
}

// ----------------------------------------------------- 5. media.rs attacks

fn session_with_user_text(text: &str) -> ParsedSession {
    parse(&user_line(text))
}

#[test]
fn media_regex_misses_paths_with_spaces_and_truncates() {
    // Real Cursor asset/upload names are underscore-sanitized (verified on
    // live data: assets/Screenshot_2026-07-03_at_11.47.48_PM-<uuid>.png), so
    // in-cursor copies are safe. But arbitrary user paths with spaces:
    // GAP: fully space-separated paths are missed entirely, and a space in a
    // parent dir yields a truncated bogus match recorded in the manifest.
    let session =
        session_with_user_text("see /Users/x/My Screens/shot final.png and /Users/x/di r/file.pdf");
    let refs = extract_media_refs(&session, &PathBuf::from("/nonexistent-root"));
    let paths: Vec<String> = refs.iter().map(|r| r.path.display().to_string()).collect();
    assert_eq!(
        paths,
        vec!["/file.pdf"],
        "first path missed, second truncated"
    );

    // Sanitized real-world asset naming is matched in full.
    let real =
        "/Users/x/.cursor/projects/p/assets/Screenshot_2026-05-27_at_1.42.14_AM-931a80a6.png";
    let session = session_with_user_text(&format!("image: {real}"));
    let refs = extract_media_refs(&session, &PathBuf::from("/nonexistent-root"));
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].path.display().to_string(), real);
}

#[test]
fn media_regex_unicode_huge_paths_and_redos_attempt() {
    // Unicode path segments are matched.
    let session = session_with_user_text("voir /tmp/скриншот-图片-café.png ici");
    let refs = extract_media_refs(&session, &PathBuf::from("/nonexistent-root"));
    assert_eq!(refs.len(), 1);
    assert_eq!(
        refs[0].path.display().to_string(),
        "/tmp/скриншот-图片-café.png"
    );

    // Enormous synthetic path: matched, classified, exists=false; no blowup.
    let huge = format!("/{}.png", "a".repeat(1_000_000));
    let session = session_with_user_text(&huge);
    let refs = extract_media_refs(&session, &PathBuf::from("/nonexistent-root"));
    assert_eq!(refs.len(), 1);
    assert!(!refs[0].exists);

    // ReDoS attempt: rust's regex crate is linear-time, so dot-heavy
    // near-miss input must complete quickly.
    let adversarial = format!("/{}", "a.".repeat(500_000)); // no valid extension
    let session = session_with_user_text(&adversarial);
    let start = Instant::now();
    let refs = extract_media_refs(&session, &PathBuf::from("/nonexistent-root"));
    let elapsed = start.elapsed();
    assert!(refs.is_empty());
    assert!(elapsed.as_secs() < 5, "linear scan took {elapsed:?}");
}

// --------------------------------------------------------- 6. export abuse

#[test]
fn export_with_all_turns_untrainable_completes_cleanly() {
    let dir = scratch("untrainable");
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    // Assistant side is tool_use only; with include_tool_calls=false (default)
    // every turn renders empty on the assistant side -> untrainable.
    let content = format!(
        "{}\n{}\n",
        user_line("<user_query>do something</user_query>"),
        json!({"role":"assistant","message":{"content":[{"type":"tool_use","name":"Shell","input":{"command":"ls"}}]}})
    );
    let jsonl = src.join("adv.jsonl");
    fs::write(&jsonl, content).unwrap();

    let out = dir.join("out");
    let (tx, rx) = channel();
    run_export(
        vec![meta_at(jsonl)],
        out.clone(),
        ExportOptions::default(),
        dir.join("fake-cursor-root"), // never touches real ~/.cursor
        tx,
        || {},
    );
    let summary = drain_export(&rx);
    assert_eq!(summary.sessions_exported, 0);
    assert_eq!(summary.sessions_skipped, 1);
    assert!(
        out.join("manifest.json").is_file(),
        "manifest still written"
    );
    assert!(out.join("README.md").is_file());
    assert!(!out.join("sft_chatml").join("train.jsonl").exists());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn export_100k_turns_completes_in_reasonable_time() {
    let dir = scratch("100k");
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    let mut content = String::with_capacity(40_000_000);
    for i in 0..100_000 {
        content.push_str(&user_line(&format!(
            "<user_query>question {i}</user_query>"
        )));
        content.push('\n');
        content.push_str(&asst_line(&format!("answer {i}")));
        content.push('\n');
    }
    let jsonl = src.join("big.jsonl");
    fs::write(&jsonl, &content).unwrap();

    let out = dir.join("out");
    let (tx, rx) = channel();
    let start = Instant::now();
    let options = ExportOptions {
        max_record_chars: 0, // exercise the single-record path at scale
        ..Default::default()
    };
    run_export(
        vec![meta_at(jsonl)],
        out.clone(),
        options,
        dir.join("fake-cursor-root"),
        tx,
        || {},
    );
    let elapsed = start.elapsed();
    let summary = drain_export(&rx);
    assert_eq!(summary.sessions_exported, 1);
    assert_eq!(summary.sft_records, 1);
    let train = fs::read_to_string(out.join("sft_chatml").join("train.jsonl")).unwrap();
    let record: serde_json::Value = serde_json::from_str(train.trim()).unwrap();
    assert_eq!(record["messages"].as_array().unwrap().len(), 200_000);
    // Note: each turn is rendered several times (trainability check + each
    // writer); still linear overall. Generous bound to avoid CI flakiness.
    assert!(elapsed.as_secs() < 60, "100k-turn export took {elapsed:?}");
    eprintln!("100k-turn export: {elapsed:?}");
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn validate_out_dir_rejects_symlink_into_cursor() {
    // A symlink under /tmp pointing at ~/.cursor must be caught after
    // canonicalization. Creating the symlink writes only in /tmp.
    let Some(home) = dirs::home_dir() else { return };
    let cursor_root = home.join(".cursor");
    if !cursor_root.is_dir() {
        eprintln!("no ~/.cursor — skipping symlink test");
        return;
    }
    let dir = scratch("symlink");
    let link = dir.join("innocent-looking");
    std::os::unix::fs::symlink(&cursor_root, &link).unwrap();

    let err = validate_out_dir(&link.join("dump"), &cursor_root);
    assert!(
        err.is_err(),
        "symlinked out_dir into ~/.cursor must be rejected"
    );

    // `..` traversal that lands back inside ~/.cursor is also rejected.
    let sneaky = cursor_root.join("projects").join("..").join("sneaky-dump");
    assert!(validate_out_dir(&sneaky, &cursor_root).is_err());

    // And a genuinely safe temp dir passes.
    assert!(validate_out_dir(&dir.join("ok"), &cursor_root).is_ok());
    let _ = fs::remove_dir_all(&dir);
}

// -------------------------------------------------------- 7. scanner abuse

#[test]
fn scanner_ignores_junk_and_handles_empty_sessions() {
    let root = scratch("scanroot");

    // Junk file at root level: ignored (not a dir).
    fs::write(root.join("stray.txt"), "junk").unwrap();
    // Project without agent-transcripts: included with zero sessions.
    fs::create_dir_all(root.join("proj-empty")).unwrap();

    let transcripts = root.join("proj1").join("agent-transcripts");
    fs::create_dir_all(&transcripts).unwrap();
    // Non-dir junk inside agent-transcripts: ignored.
    fs::write(transcripts.join("junk.log"), "noise").unwrap();
    // Directory named like a uuid but empty: no <id>.jsonl -> no session.
    fs::create_dir_all(transcripts.join("11111111-1111-1111-1111-111111111111")).unwrap();
    // Session dir whose jsonl name does not match the dir id: ignored.
    let mismatched = transcripts.join("22222222-2222-2222-2222-222222222222");
    fs::create_dir_all(&mismatched).unwrap();
    fs::write(mismatched.join("other-name.jsonl"), user_line("x")).unwrap();
    // 0-byte transcript: session listed, untitled.
    let zero = transcripts.join("33333333-3333-3333-3333-333333333333");
    fs::create_dir_all(&zero).unwrap();
    fs::write(zero.join("33333333-3333-3333-3333-333333333333.jsonl"), "").unwrap();
    // Invalid-UTF-8 transcript: session listed, untitled (title read fails).
    let bad = transcripts.join("44444444-4444-4444-4444-444444444444");
    fs::create_dir_all(&bad).unwrap();
    fs::write(
        bad.join("44444444-4444-4444-4444-444444444444.jsonl"),
        [0xFF, 0xFE, b'\n'],
    )
    .unwrap();
    // Valid transcript with subagent junk next to it.
    let good = transcripts.join("55555555-5555-5555-5555-555555555555");
    fs::create_dir_all(good.join("subagents")).unwrap();
    fs::write(
        good.join("55555555-5555-5555-5555-555555555555.jsonl"),
        format!("{}\n", user_line("<user_query>real title</user_query>")),
    )
    .unwrap();
    fs::write(good.join("subagents").join("not-a-transcript.txt"), "x").unwrap();

    let projects = scanner::scan_projects(&root);
    assert_eq!(
        projects.len(),
        2,
        "junk file at root not treated as project"
    );

    let proj1 = projects.iter().find(|p| p.slug == "proj1").unwrap();
    let ids: Vec<&str> = proj1.sessions.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        proj1.sessions.len(),
        3,
        "zero-byte, bad-utf8 and valid sessions"
    );
    assert!(ids.contains(&"33333333-3333-3333-3333-333333333333"));
    assert!(ids.contains(&"44444444-4444-4444-4444-444444444444"));
    assert!(ids.contains(&"55555555-5555-5555-5555-555555555555"));
    for s in &proj1.sessions {
        match s.id.as_str() {
            "55555555-5555-5555-5555-555555555555" => assert_eq!(s.title, "real title"),
            _ => assert_eq!(s.title, "(untitled session)"),
        }
        assert!(!s.is_subagent);
    }

    let empty = projects.iter().find(|p| p.slug == "proj-empty").unwrap();
    assert!(empty.sessions.is_empty());
    let _ = fs::remove_dir_all(&root);
}
