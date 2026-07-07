//! Secret detection and optional redaction for exported text.
//!
//! Transcripts routinely contain shell output and pasted config that can carry
//! live credentials (API tokens, keys, bearer tokens). Training on them would
//! memorize secrets and shipping a dataset would leak them. We detect common
//! secret shapes so the export can (a) always REPORT how many remain in the
//! written files and (b) optionally REDACT them.

use regex::Regex;
use std::sync::OnceLock;

/// (label, pattern) for common secret formats. Patterns are conservative to
/// avoid false positives on ordinary prose/code.
fn patterns() -> &'static [(&'static str, Regex)] {
    static P: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    P.get_or_init(|| {
        let raw: &[(&str, &str)] = &[
            ("huggingface", r"\bhf_[A-Za-z0-9]{20,}"),
            ("openai", r"\bsk-[A-Za-z0-9_-]{20,}"),
            ("github_pat", r"\bghp_[A-Za-z0-9]{30,}"),
            ("github_pat", r"\bgithub_pat_[A-Za-z0-9_]{30,}"),
            ("github_oauth", r"\bgho_[A-Za-z0-9]{30,}"),
            ("aws_access_key", r"\bAKIA[0-9A-Z]{16}\b"),
            ("google_api", r"\bAIza[0-9A-Za-z_-]{35}"),
            ("slack", r"\bxox[baprs]-[A-Za-z0-9-]{10,}"),
            ("bearer", r"[Bb]earer\s+[A-Za-z0-9._~+/-]{20,}=*"),
            (
                "private_key",
                r"-----BEGIN (?:RSA |EC |OPENSSH |PGP )?PRIVATE KEY-----",
            ),
        ];
        raw.iter()
            .map(|(l, p)| (*l, Regex::new(p).expect("valid secret regex")))
            .collect()
    })
}

/// Count secret matches in `text`, tallied by label.
pub fn scan(text: &str, tally: &mut std::collections::BTreeMap<String, usize>) {
    for (label, re) in patterns() {
        let n = re.find_iter(text).count();
        if n > 0 {
            *tally.entry((*label).to_string()).or_insert(0) += n;
        }
    }
}

/// Replace every detected secret with `[REDACTED_<LABEL>]`.
pub fn redact(text: &str) -> String {
    let mut out = text.to_string();
    for (label, re) in patterns() {
        let marker = format!("[REDACTED_{}]", label.to_uppercase());
        out = re.replace_all(&out, marker.as_str()).into_owned();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_and_redacts_common_secrets() {
        let text = "run hf download --token hf_abcdefghijklmnopqrstuvwxyz and \
                    curl -H 'Authorization: Bearer agora_bbc17435141917189301d9f028650981a53c0622'";
        let mut tally = std::collections::BTreeMap::new();
        scan(text, &mut tally);
        assert!(tally.get("huggingface").copied().unwrap_or(0) >= 1);
        assert!(tally.get("bearer").copied().unwrap_or(0) >= 1);
        let red = redact(text);
        assert!(!red.contains("hf_abcdefghijklmnopqrstuvwxyz"));
        assert!(red.contains("[REDACTED_HUGGINGFACE]"));
    }

    #[test]
    fn no_false_positive_on_plain_prose() {
        let mut tally = std::collections::BTreeMap::new();
        scan(
            "The bearer of this message should ski more often.",
            &mut tally,
        );
        // "bearer" word followed by short prose should not match the token shape.
        assert!(tally.is_empty(), "unexpected: {tally:?}");
    }
}
