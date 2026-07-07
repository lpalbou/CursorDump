//! Content-cleaning passes applied when rendering assistant text for export.
//!
//! Cursor transcripts interleave the assistant's user-facing prose with
//! summarized thinking narration (bold pseudo-headers followed by
//! first-person deliberation) and IDE-only artifacts (chat links). These are
//! valuable for exploration but poison training targets, so exports strip
//! them by default (toggle in `ExportOptions`).

use regex::Regex;
use std::sync::OnceLock;

/// Split Cursor assistant text into (thinking, answer).
///
/// "Thinking" = runs of `**Title Case Header**` followed by first-person
/// deliberation prose (verified shape across real transcripts). The thinking
/// segments are concatenated in order (headers dropped) as the reasoning
/// trace; everything else is the user-facing answer. Reordering all thinking
/// ahead of the answer matches the reasoning-model convention
/// (`<think>…</think>` precedes the response).
///
/// Detection is conservative: a header alone is not enough; the following
/// prose must read as first-person deliberation. This keeps legitimate bold
/// section headings ("**Results**") in the answer.
pub fn split_thinking(text: &str) -> (String, String) {
    static HEADER: OnceLock<Regex> = OnceLock::new();
    let header = HEADER
        .get_or_init(|| Regex::new(r"^\*\*[A-Z][^*\n]{2,70}\*\*$").expect("valid header regex"));

    // Thinking headers are frequently glued to the preceding sentence by a
    // single newline ("...done.\n**Planning next**\n\nI need to..."), which
    // would hide them from paragraph splitting. Normalize so every standalone
    // header line begins a paragraph.
    let normalized = normalize_header_breaks(text, header);

    // Split into paragraphs (blank-line separated). Real transcripts use BOTH:
    //   (a) "**Header**\nbody..." — header and body in one paragraph
    //   (b) "**Header**\n\nbody..." — header paragraph, then body paragraph
    let paragraphs: Vec<&str> = normalized.split("\n\n").collect();
    let mut is_thinking = vec![false; paragraphs.len()];

    for i in 0..paragraphs.len() {
        let para = paragraphs[i];
        let mut lines = para.lines();
        let Some(first) = lines.next() else { continue };
        if !header.is_match(first.trim()) {
            continue;
        }
        let body: String = lines.collect::<Vec<_>>().join(" ");
        if !body.trim().is_empty() {
            if is_deliberation(&body) {
                is_thinking[i] = true;
            }
        } else if let Some(next) = paragraphs.get(i + 1) {
            let next_first = next.lines().next().unwrap_or("");
            if !header.is_match(next_first.trim()) && is_deliberation(next) {
                is_thinking[i] = true;
                is_thinking[i + 1] = true;
            }
        }
    }

    // Second pass: HEADERLESS thinking. Some models' summaries have no bold
    // headers at all — just first-person planning paragraphs ("Now I'm
    // creating an end-to-end example that…"). Require a STRONG deliberation
    // opener plus the usual marker density, so ordinary answer prose that
    // merely mentions "I" mid-sentence is not swept in.
    for (i, para) in paragraphs.iter().enumerate() {
        if is_thinking[i] {
            continue;
        }
        let first_line = para.lines().next().unwrap_or("").trim();
        if header.is_match(first_line) {
            continue; // header paragraphs were already judged in pass one
        }
        if is_headerless_deliberation(para) {
            is_thinking[i] = true;
        }
    }

    let mut thinking = Vec::new();
    let mut answer = Vec::new();
    for (para, &think) in paragraphs.iter().zip(&is_thinking) {
        if think {
            // Keep the whole paragraph INCLUDING the bold header: mis-tagged
            // content stays human-auditable and recoverable inside <think>.
            let p = para.trim();
            if !p.is_empty() {
                thinking.push(p.to_string());
            }
        } else {
            answer.push((*para).to_string());
        }
    }

    (
        thinking.join("\n\n").trim().to_string(),
        answer.join("\n\n").trim().to_string(),
    )
}

/// Strip thinking, returning only the user-facing answer.
pub fn strip_thinking_blocks(text: &str) -> String {
    split_thinking(text).1
}

/// Ensure every standalone bold-header line is separated from the preceding
/// line by a blank line, so paragraph splitting treats it as a boundary.
fn normalize_header_breaks(text: &str, header: &Regex) -> String {
    let mut out = String::with_capacity(text.len() + 64);
    let mut prev_blank = true; // start-of-text counts as a boundary
    for (i, line) in text.split('\n').enumerate() {
        let is_header = header.is_match(line.trim());
        if i > 0 {
            out.push('\n');
        }
        if is_header && !prev_blank {
            out.push('\n'); // insert the missing blank line before the header
        }
        out.push_str(line);
        prev_blank = line.trim().is_empty();
    }
    out
}

/// Normalize typographic apostrophes/quotes to ASCII for MATCHING only.
/// GPT-family transcripts use U+2019 ("I’ll", "I’m"); without this every
/// deliberation heuristic silently fails on them, leaving reasoning in the
/// answer. The emitted text keeps the original characters — this is only used
/// inside the predicates.
fn norm_apos(s: &str) -> String {
    s.replace(['\u{2019}', '\u{2018}', '\u{02bc}'], "'")
}

/// STRONG planning openers: unambiguous first-person planning phrases. A
/// paragraph opening with one of these (and free of deliverable structure) is
/// deliberation on its own — no extra marker density needed.
const STRONG_OPENERS: &[&str] = &[
    "Now I'm ",
    "Now I'll ",
    "Now I ",
    "Now let me ",
    "Now, I",
    "Let me ",
    "Let's ",
    "I'll ",
    "I will ",
    "I need to ",
    "I need ",
    "I'm going ",
    "I am going ",
    "I'm planning",
    "I'm setting",
    "I'm mapping",
    "I'm looking",
    "I'm creating",
    "I'm working",
    "I'm checking",
    "I'm gonna ",
    "I plan ",
    "I should ",
    "First, I",
    "First I",
    "Next, I",
    "Next I",
    "Then I'll",
    "My plan",
    "My next",
    "My first",
    "Time to ",
];

/// WEAK openers: first-person but ambiguous ("I'm confident…" is an answer).
/// These require full deliberation density before counting as thinking.
const DELIBERATION_OPENERS: &[&str] = &[
    "I'm ",
    "I am ",
    "I want ",
    "I see ",
    "I think ",
    "I notice",
    "I've ",
    "I have ",
    "I checked",
    "So I ",
    "The user ",
];

/// Deliverables have STRUCTURE (code fences, tables, bullet/numbered lists,
/// headings); deliberation is plain prose. Structured paragraphs are never
/// thinking — a false positive would hide real answer content in `<think>`.
fn has_deliverable_structure(body: &str) -> bool {
    if body.contains("```") {
        return true;
    }
    body.lines().any(|line| {
        let l = line.trim_start();
        l.starts_with("- ")
            || l.starts_with("* ")
            || l.starts_with("| ")
            || l.starts_with('#')
            || (l.len() > 2 && l.as_bytes()[0].is_ascii_digit() && l[1..].starts_with(". "))
    })
}

/// Headerless deliberation: a plain paragraph opening with first-person
/// planning. Strong openers qualify alone; weak openers need marker density.
fn is_headerless_deliberation(para: &str) -> bool {
    let p = para.trim();
    if p.len() < 60 || has_deliverable_structure(p) {
        return false;
    }
    let pn = norm_apos(p);
    if STRONG_OPENERS.iter().any(|o| pn.starts_with(o)) {
        return true;
    }
    if DELIBERATION_OPENERS.iter().any(|o| pn.starts_with(o)) {
        return is_deliberation(p);
    }
    false
}

/// First-person deliberation heuristic for a paragraph body.
///
/// Hard vetoes come first: deliverables have STRUCTURE (code fences, tables,
/// bullet lists, code citations), deliberation is plain prose. Now that
/// thinking is captured rather than stripped, a false positive would hide
/// real answer content inside `<think>`, so structured paragraphs are never
/// classified as thinking.
fn is_deliberation(body: &str) -> bool {
    let raw = body.trim();
    if raw.len() < 40 {
        return false;
    }
    // Structural vetoes: never treat structured content as thinking.
    if raw.contains("```") {
        return false;
    }
    for line in raw.lines() {
        let l = line.trim_start();
        if l.starts_with("- ")
            || l.starts_with("* ")
            || l.starts_with("| ")
            || l.starts_with('#')
            || (l.len() > 2 && l.as_bytes()[0].is_ascii_digit() && l[1..].starts_with(". "))
        {
            return false;
        }
    }
    // Match on an apostrophe-normalized copy (handles "I’ll" / "I’m").
    let body = norm_apos(raw);
    const MARKERS: &[&str] = &[
        "I need to",
        "I'll ",
        "I will ",
        "I'm ",
        "I am ",
        "I want ",
        "I should ",
        "I see ",
        "I think ",
        "I notice",
        "I plan ",
        "I can ",
        "I could ",
        "Let me ",
        "Let's ",
        "We need",
        "We'll ",
        "We should",
        "My first",
        "I have ",
        "I've ",
        "It's important",
        "It looks like",
        "It seems",
        "I wonder",
        "I consider",
        "I'm considering",
        "Maybe I",
        "So I",
        "the user's ",
        "I checked",
        "I did ",
        "I don't",
        "I won't",
        "I must ",
        "I may ",
        "I might ",
    ];
    // First-person planning signals: strong evidence of deliberation.
    const PLANNING: &[&str] = &[
        "I need",
        "I'll",
        "I will",
        "Let me",
        "Let's",
        "I'm going",
        "I should",
        "I want",
        "I plan",
        "I have to",
        "I must",
        "I'm considering",
        "My plan",
    ];
    let hits = MARKERS.iter().filter(|m| body.contains(*m)).count();
    let has_planning = PLANNING.iter().any(|m| body.contains(m));
    hits >= 2
        || body.starts_with("I ")
        || body.starts_with("I'")
        // "The user …" prose is deliberation ONLY with a planning co-signal,
        // so answer prose that merely addresses the user is not swept in.
        || (body.starts_with("The user") && has_planning)
}

/// Rewrite Cursor chat links `[label](<uuid>)` to plain `label`: the UUIDs
/// only resolve inside the IDE and teach a model to emit dead links.
pub fn strip_chat_links(text: &str) -> String {
    static LINK: OnceLock<Regex> = OnceLock::new();
    let link = LINK.get_or_init(|| {
        Regex::new(
            r"\[([^\]]+)\]\((?:[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})\)",
        )
        .expect("valid chat link regex")
    });
    link.replace_all(text, "$1").into_owned()
}

/// Drop trailing "unfulfilled intent" content: when tool calls are stripped,
/// merged assistant text often ends with "…now I'll run the tests." with
/// nothing following. Training on these teaches announcing-then-stopping.
///
/// Strategy: repeatedly trim intent SENTENCES from the very end of the text
/// (a final paragraph can mix substance and a trailing announcement), then
/// drop the final paragraph entirely if it became empty.
pub fn strip_trailing_intent(text: &str) -> String {
    let mut out = text.trim_end().to_string();
    // Trim at most a handful of sentences to stay conservative.
    for _ in 0..4 {
        let Some(cut) = trailing_intent_sentence_start(&out) else {
            break;
        };
        out.truncate(cut);
        let t = out.trim_end().trim_end_matches("\n\n").len();
        out.truncate(t);
    }
    out.trim_end().to_string()
}

/// If the last sentence of `text` announces impending action, return the byte
/// index where that sentence starts (i.e. where to cut). Otherwise None.
fn trailing_intent_sentence_start(text: &str) -> Option<usize> {
    let trimmed = text.trim_end();
    if trimmed.is_empty() {
        return None;
    }
    // Never touch code blocks or list content.
    if trimmed.ends_with('`') || trimmed.ends_with("```") {
        return None;
    }
    // Find the start of the last sentence: after the previous '.', '!', '?',
    // ':' followed by whitespace, or after the last blank line.
    let last_para = trimmed.rfind("\n\n").map(|i| i + 2).unwrap_or(0);
    let tail = &trimmed[last_para..];
    let mut sentence_start = last_para;
    let bytes = tail.as_bytes();
    let mut i = tail.len().saturating_sub(1);
    while i > 0 {
        let c = bytes[i - 1];
        if (c == b'.' || c == b'!' || c == b'?')
            && bytes
                .get(i)
                .map(|b| b.is_ascii_whitespace())
                .unwrap_or(false)
        {
            sentence_start = last_para + i + 1;
            break;
        }
        i -= 1;
    }
    let sentence = trimmed[sentence_start..].trim_start();
    const INTENT_PATTERNS: &[&str] = &[
        "Let me ",
        "Now I'll",
        "Now I will",
        "Now let me",
        "I'll now",
        "I will now",
        "Next, I'll",
        "Next I'll",
        "Next up, I",
        "Time to ",
        "Then I'll",
        "then I'll",
        "I'm going to ",
        "I am going to ",
        "I'll start",
        "I'll begin",
        "I'll run",
        "I'll check",
        "I'll look",
        "I'll read",
        "I'll open",
        "I'll fix",
        "I'll update",
        "I'll create",
        "I'll stage",
        "I'll write",
        "I'll add",
        "I'll apply",
    ];
    let is_intent = sentence.len() < 300
        && !sentence.contains('\n')
        && INTENT_PATTERNS
            .iter()
            .any(|p| sentence.starts_with(p) || sentence.contains(p))
        && (sentence.ends_with('.') || sentence.ends_with('!') || sentence.ends_with('…'));
    if is_intent {
        Some(sentence_start)
    } else {
        None
    }
}

/// Full cleaning pipeline for one assistant text segment.
pub fn clean_assistant_text(text: &str) -> String {
    strip_chat_links(&strip_thinking_blocks(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_thinking_run_keeps_answer() {
        let text = "Here is the fix for your bug.\n\n\
                    **Investigating the lock issue**\n\
                    I need to check whether the lock file is stale. I'll look at the terminals folder to see if anything holds it.\n\n\
                    **Planning the commit**\n\
                    Let me stage the files. I should follow the guidelines and avoid reading with cat.\n\n\
                    The lock file was stale; I removed it and committed.";
        let cleaned = strip_thinking_blocks(text);
        assert!(cleaned.contains("Here is the fix"));
        assert!(cleaned.contains("The lock file was stale"));
        assert!(!cleaned.contains("Investigating the lock issue"));
        assert!(!cleaned.contains("avoid reading with cat"));
    }

    #[test]
    fn strips_thinking_header_glued_to_previous_line() {
        // Header preceded by only a single newline (real transcript layout).
        let text = "Ran git add . to stage the files.\n\
                    **Finalizing the git commit process**\n\n\
                    The user previously tried git add . and it failed. I need to check whether the index lock is held before retrying.\n\n\
                    Committed successfully as abc123.";
        let cleaned = strip_thinking_blocks(text);
        assert!(cleaned.contains("Ran git add ."));
        assert!(cleaned.contains("Committed successfully"));
        assert!(!cleaned.contains("Finalizing the git commit process"));
        assert!(!cleaned.contains("index lock is held"));
    }

    #[test]
    fn strips_thinking_with_standalone_header_paragraphs() {
        // Real transcript layout: header is its own paragraph, deliberation
        // body follows as a separate paragraph.
        let text = "Got it, cleaning up the file now.\n\n\
                    **Planning the formatting approach**\n\n\
                    So, for this task, I want to begin with 2-3 paragraphs to reason through the formatting. I think it's crucial to stick to commentary for updates.\n\n\
                    **Reviewing and planning the task**\n\n\
                    I see the instructions say to first review the scratchpad and clear any old tasks if necessary. It's important to read the file since it includes information.\n\n\
                    The file is now clean markdown with proper nesting.";
        let cleaned = strip_thinking_blocks(text);
        assert!(cleaned.contains("Got it, cleaning up"));
        assert!(cleaned.contains("The file is now clean markdown"));
        assert!(!cleaned.contains("Planning the formatting approach"));
        assert!(!cleaned.contains("I want to begin with 2-3 paragraphs"));
        assert!(!cleaned.contains("Reviewing and planning the task"));
    }

    #[test]
    fn detects_headerless_thinking_paragraphs() {
        // Real pattern from transcripts: no bold headers at all, just
        // first-person planning prose interleaved with the answer.
        let text = "Now the self-dogfood — a scenario exercising the whole attention model end-to-end.\n\n\
                    Now I'm creating an end-to-end example that exercises the attention model with a worker agent triaging messages. I need to include noise like small FYIs that can be inlined.\n\n\
                    The script demonstrates how the worker handles open questions requiring replies.";
        let (thinking, answer) = split_thinking(text);
        assert!(
            thinking.contains("Now I'm creating"),
            "headerless thinking captured"
        );
        assert!(answer.contains("self-dogfood"), "headline stays in answer");
        assert!(
            answer.contains("The script demonstrates"),
            "descriptive prose stays"
        );
    }

    #[test]
    fn detects_thinking_with_curly_apostrophes() {
        // GPT-family transcripts use U+2019; matching must still fire, but the
        // EMITTED text must keep the original curly apostrophes.
        let text = "I\u{2019}m planning to simplify the Mermaid diagrams by using three code fences. I\u{2019}ll rewrite them next.\n\nHere is the cleaned diagram.";
        let (thinking, answer) = split_thinking(text);
        assert!(
            thinking.contains("I\u{2019}m planning"),
            "curly-apos thinking captured, original kept"
        );
        assert!(answer.contains("Here is the cleaned diagram"));

        // Header + curly-apostrophe body.
        let text2 = "**Clarifying Mermaid syntax issues**\n\nI\u{2019}m looking at some syntax details. It seems the parser needs a fix.\n\nThe fix is applied.";
        let (thinking2, answer2) = split_thinking(text2);
        assert!(
            thinking2.contains("Clarifying Mermaid"),
            "header thinking captured"
        );
        assert!(answer2.contains("The fix is applied"));
        assert!(!answer2.contains("looking at some syntax"));
    }

    #[test]
    fn headerless_detection_spares_normal_answers() {
        // Answer prose that mentions "I" mid-paragraph must not be swept in.
        let text = "The fix changes three files. I kept the public API stable, so no callers need updates.";
        let (thinking, answer) = split_thinking(text);
        assert!(thinking.is_empty());
        assert_eq!(answer, text);
        // Structured content with a first-person opener is also spared.
        let text2 = "I'll summarize the results:\n- test A passes\n- test B passes";
        let (thinking2, answer2) = split_thinking(text2);
        assert!(thinking2.is_empty());
        assert!(answer2.contains("test A"));
    }

    #[test]
    fn keeps_legitimate_bold_headings() {
        let text =
            "**Results**\n\nThe benchmark shows 3x speedup across all configurations tested.";
        let cleaned = strip_thinking_blocks(text);
        assert_eq!(cleaned, text);
    }

    #[test]
    fn strips_chat_links_only() {
        let text = "See the [Nature audit](c285f6d0-ec2a-4d3e-ac8d-e759fe6057bc) and [docs](https://example.com).";
        let cleaned = strip_chat_links(text);
        assert!(cleaned.contains("See the Nature audit and"));
        assert!(cleaned.contains("[docs](https://example.com)"));
    }

    #[test]
    fn strips_trailing_intent_paragraph() {
        let text = "The parser bug is in segment_turns.\n\nNow I'll run the tests to confirm.";
        assert_eq!(
            strip_trailing_intent(text),
            "The parser bug is in segment_turns."
        );
    }

    #[test]
    fn strips_trailing_intent_sentence_within_paragraph() {
        let text = "The lock file is stale and no process owns it. Once removed, then I'll stage everything and create the commit.";
        assert_eq!(
            strip_trailing_intent(text),
            "The lock file is stale and no process owns it."
        );
    }

    #[test]
    fn keeps_substantive_final_paragraph() {
        let text = "First part.\n\nThe final fix changes three files and all tests pass now.";
        assert_eq!(strip_trailing_intent(text), text);
    }
}
