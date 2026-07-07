//! Turn-semantics regression tests for the adversarial-review fixes:
//! injected user records must not split turns nor leak into clean exports.

use std::path::PathBuf;

use cursordump::export::{render_turns, ExportOptions};
use cursordump::model::SessionMeta;
use cursordump::parser::parse_content;
use serde_json::json;

fn meta() -> SessionMeta {
    SessionMeta {
        id: "t".into(),
        project_slug: "proj".into(),
        path: PathBuf::from("/dev/null"),
        title: "t".into(),
        modified: None,
        size_bytes: 0,
        is_subagent: false,
        parent_id: None,
    }
}

fn user(text: &str) -> String {
    json!({"role":"user","message":{"content":[{"type":"text","text":text}]}}).to_string()
}

fn asst(text: &str) -> String {
    json!({"role":"assistant","message":{"content":[{"type":"text","text":text}]}}).to_string()
}

const SUBAGENT_NOTE: &str = "<user_query>The beginning of the above subagent result is already visible to the user. Perform any follow-up actions (if needed). DO NOT regurgitate or reiterate its result unless asked.</user_query>";

#[test]
fn subagent_notification_does_not_split_turn() {
    // real query -> work -> injected notification -> more work
    // Must be ONE turn whose assistant side contains all assistant text.
    let content = [
        user("<user_query>audit the repo</user_query>"),
        asst("Starting the audit."),
        user(SUBAGENT_NOTE),
        asst("The audit found 3 issues; here is the summary."),
    ]
    .join("\n");
    let session = parse_content(meta(), &content);
    assert_eq!(
        session.turns.len(),
        1,
        "injected record must not split the turn"
    );

    let rendered = render_turns(&session, &ExportOptions::default(), None);
    assert_eq!(rendered.len(), 1);
    assert_eq!(rendered[0].user, "audit the repo");
    assert!(rendered[0].answer.contains("Starting the audit."));
    assert!(rendered[0].answer.contains("3 issues"));
    assert!(
        !rendered[0].user.contains("subagent"),
        "boilerplate must not leak into user content"
    );
}

#[test]
fn context_only_user_record_does_not_split_turn() {
    // A user record with no <user_query> (attached context) mid-conversation.
    let content = [
        user("<user_query>first question</user_query>"),
        asst("first answer"),
        user("<system_notification>background shell finished</system_notification>"),
        asst("follow-up work after notification"),
        user("<user_query>second question</user_query>"),
        asst("second answer"),
    ]
    .join("\n");
    let session = parse_content(meta(), &content);
    assert_eq!(session.turns.len(), 2);

    let rendered = render_turns(&session, &ExportOptions::default(), None);
    assert_eq!(rendered.len(), 2);
    assert!(rendered[0].answer.contains("first answer"));
    assert!(
        rendered[0].answer.contains("follow-up work"),
        "assistant work after injected record belongs to the same turn"
    );
    assert_eq!(rendered[1].user, "second question");
}

#[test]
fn clean_assistant_strips_thinking_and_links() {
    let content = [
        user("<user_query>what changed?</user_query>"),
        asst("**Reviewing the changes**\nI need to check the diff first. I'll look at the files involved and see what changed.\n\nThe change renames `foo` to `bar` across 3 files. See [prior chat](11111111-2222-3333-4444-555555555555) for context."),
    ]
    .join("\n");
    let session = parse_content(meta(), &content);
    let rendered = render_turns(&session, &ExportOptions::default(), None);
    assert_eq!(rendered.len(), 1);
    let answer = &rendered[0].answer;
    let thinking = &rendered[0].thinking;
    // The thinking header/deliberation moves OUT of the answer, INTO thinking.
    assert!(
        !answer.contains("Reviewing the changes"),
        "header out of answer"
    );
    assert!(
        !answer.contains("I need to check"),
        "deliberation out of answer"
    );
    assert!(
        thinking.contains("I need to check"),
        "deliberation captured as thinking"
    );
    assert!(
        answer.contains("renames `foo` to `bar`"),
        "real answer kept"
    );
    assert!(answer.contains("prior chat"), "link label kept");
    assert!(!answer.contains("11111111-2222"), "chat uuid removed");

    // With cleaning disabled the chat uuid is preserved in the native text.
    let raw = render_turns(
        &session,
        &ExportOptions {
            clean_assistant: false,
            ..Default::default()
        },
        None,
    );
    assert!(raw[0].native.contains("11111111-2222"));
}

#[test]
fn final_response_only_keeps_last_assistant_text() {
    let content = [
        user("<user_query>fix the tests</user_query>"),
        asst("Looking into the failures now."),
        asst("Two assertions were stale after the API change."),
        asst("All 12 tests pass now; the fix touched only the fixtures."),
    ]
    .join("\n");
    let session = parse_content(meta(), &content);
    let rendered = render_turns(
        &session,
        &ExportOptions {
            final_response_only: true,
            ..Default::default()
        },
        None,
    );
    assert_eq!(rendered.len(), 1);
    assert!(rendered[0].answer.contains("All 12 tests pass"));
    assert!(!rendered[0].answer.contains("Looking into the failures"));
}
