//! The minimal task description classification may send off the machine.
//!
//! Only the current user message is ever an input: never history, memory,
//! tool output, persona, attachments or another profile's data. From it this
//! module removes what is most likely to be private — code blocks, URLs,
//! e-mail addresses, file paths, credential-shaped tokens, values that follow
//! credential words, and long digit runs — then bounds the result. Redaction is
//! a minimization step, not an authorization: the profile must still be
//! permitted, and a message that cannot be reduced safely is not sent at all.

/// A message longer than this is most likely a pasted document; it is not
/// summarized for an external service.
pub(crate) const MAX_SOURCE_CHARS: usize = 8_000;

/// Words whose following value is treated as a credential.
const SECRET_WORDS: &[&str] = &[
    "password",
    "passwd",
    "passphrase",
    "secret",
    "token",
    "apikey",
    "api_key",
    "api-key",
    "bearer",
    "otp",
];

/// Words that may sit between a credential word and its value.
const FILLER_WORDS: &[&str] = &["is", "was", "are", "be", "to", "of", "=", ":", "->"];

/// Prefixes of well-known credential formats.
const SECRET_PREFIXES: &[&str] = &[
    "sk-",
    "sk_",
    "pk_",
    "rk_",
    "ghp_",
    "gho_",
    "ghs_",
    "ghu_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "akia",
    "asia",
    "aiza",
    "eyj",
    "hf_",
];

/// A sanitized task description.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TaskSummary {
    pub text: String,
}

/// Why no description could be produced. Classification is then skipped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// Nothing classifiable remains after redaction.
    Empty,
    /// The message is too long to be a task description.
    TooLong,
    /// The message contains key or certificate material.
    KeyMaterial,
}

impl Refusal {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Empty => "empty_after_sanitization",
            Self::TooLong => "input_too_long",
            Self::KeyMaterial => "key_material",
        }
    }
}

/// Reduce `message` to at most `max_chars` characters of sanitized text.
pub(crate) fn summarize(message: &str, max_chars: usize) -> Result<TaskSummary, Refusal> {
    if message.chars().count() > MAX_SOURCE_CHARS {
        return Err(Refusal::TooLong);
    }
    let lowered = message.to_ascii_lowercase();
    if lowered.contains("-----begin") || lowered.contains("private key-----") {
        return Err(Refusal::KeyMaterial);
    }
    let mut words: Vec<String> = Vec::new();
    let mut in_code = false;
    let mut after_secret_word = false;
    for line in message.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            if !in_code {
                words.push("[code]".to_owned());
            }
            in_code = !in_code;
            after_secret_word = false;
            continue;
        }
        if in_code {
            continue;
        }
        for token in line.split_whitespace() {
            // "my password is X": the value follows the filler, not the word.
            if after_secret_word && FILLER_WORDS.contains(&token.to_ascii_lowercase().as_str()) {
                words.push(token.to_owned());
                continue;
            }
            let placeholder = if after_secret_word {
                Some("[secret]")
            } else {
                placeholder(token)
            };
            after_secret_word = is_secret_word(token);
            match placeholder {
                Some(placeholder) => {
                    // Adjacent identical placeholders say nothing more.
                    if words.last().map(String::as_str) != Some(placeholder) {
                        words.push(placeholder.to_owned());
                    }
                }
                None => words.push(token.to_owned()),
            }
        }
    }
    let joined = words.join(" ");
    let text = truncate(&joined, max_chars);
    let substantive = text
        .split_whitespace()
        .filter(|word| !word.starts_with('['))
        .flat_map(str::chars)
        .filter(|c| c.is_alphabetic())
        .count();
    if substantive < 3 {
        return Err(Refusal::Empty);
    }
    Ok(TaskSummary { text })
}

fn is_secret_word(token: &str) -> bool {
    let word = token
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_ascii_lowercase();
    SECRET_WORDS.contains(&word.as_str())
}

/// The placeholder replacing `token`, if it must not be sent.
fn placeholder(token: &str) -> Option<&'static str> {
    let core = token.trim_matches(|c: char| {
        matches!(
            c,
            '"' | '\''
                | '`'
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '<'
                | '>'
                | ','
                | ';'
                | '!'
                | '?'
        )
    });
    let core = core.trim_end_matches(['.', ':']);
    if core.is_empty() {
        return None;
    }
    let lowered = core.to_ascii_lowercase();
    // `password=hunter2`, `token:abc`, `API_KEY=...`
    if let Some((key, value)) = lowered.split_once(['=', ':'])
        && !value.is_empty()
        && SECRET_WORDS
            .iter()
            .any(|word| key.trim_matches('-').ends_with(word))
    {
        return Some("[secret]");
    }
    if lowered.contains("://") || lowered.starts_with("www.") {
        return Some("[url]");
    }
    if let Some((local, domain)) = core.split_once('@')
        && !local.is_empty()
        && domain.contains('.')
    {
        return Some("[email]");
    }
    let bytes = core.as_bytes();
    if (core.starts_with('/') && core.len() > 1)
        || core.starts_with("~/")
        || core.starts_with("./")
        || core.starts_with("../")
        || core.contains('\\')
        || (bytes.len() > 2
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'/')
    {
        return Some("[path]");
    }
    if is_credential_shaped(core, &lowered) {
        return Some("[secret]");
    }
    if core.chars().filter(char::is_ascii_digit).count() >= 6 {
        return Some("[number]");
    }
    None
}

fn is_credential_shaped(core: &str, lowered: &str) -> bool {
    if core.len() >= 12
        && SECRET_PREFIXES
            .iter()
            .any(|prefix| lowered.starts_with(prefix))
    {
        return true;
    }
    let token_chars = core
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '/' | '='));
    let digits = core.chars().filter(char::is_ascii_digit).count();
    let letters = core.chars().filter(char::is_ascii_alphabetic).count();
    let hex = core.len() >= 32 && core.chars().all(|c| c.is_ascii_hexdigit());
    hex || (token_chars && core.len() >= 24 && digits >= 3 && letters >= 3)
}

/// Cut at a character boundary, preferring the last word boundary.
fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let cut: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    let cut = match cut.rfind(' ') {
        Some(space) if space > 0 => cut[..space].to_owned(),
        _ => cut,
    };
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_task_and_removes_private_spans() {
        let message = "Please fix the login bug in /srv/alice/project/src/auth.rs, \
            email bob@example.com the result, see https://internal.example/ticket?id=42 \
            and use token ghp_aBcDeFgHiJkLmNoPqRsTuVwXyZ012345. My password is hunter2 \
            and my phone is 555-123-4567. API_KEY=abc123";
        let summary = summarize(message, 400).unwrap();
        for leaked in [
            "alice",
            "bob@example.com",
            "internal.example",
            "ghp_",
            "hunter2",
            "555",
            "abc123",
        ] {
            assert!(
                !summary.text.contains(leaked),
                "{leaked} leaked: {}",
                summary.text
            );
        }
        for kept in [
            "fix", "login", "bug", "[path]", "[email]", "[url]", "[secret]", "[number]",
        ] {
            assert!(
                summary.text.contains(kept),
                "{kept} missing: {}",
                summary.text
            );
        }
        assert!(!summary.text.ends_with('…'));
    }

    #[test]
    fn code_blocks_are_replaced_entirely() {
        let message = "Why does this fail?\n```rust\nlet key = \"private-value\";\nconnect(\"db.internal\");\n```\nIt panics on start.";
        let summary = summarize(message, 400).unwrap();
        assert_eq!(
            summary.text,
            "Why does this fail? [code] It panics on start."
        );
    }

    #[test]
    fn windows_paths_and_hex_digests_are_redacted() {
        let summary = summarize(
            r"Open C:\Users\carol\notes.txt and compare 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08 please",
            400,
        )
        .unwrap();
        assert_eq!(summary.text, "Open [path] and compare [secret] please");
    }

    #[test]
    fn output_is_bounded_without_splitting_characters() {
        let message = "résumé ".repeat(200);
        let summary = summarize(&message, 50).unwrap();
        assert!(summary.text.chars().count() <= 50);
        assert!(summary.text.ends_with('…'));
    }

    #[test]
    fn unsafe_or_empty_messages_are_refused() {
        assert_eq!(
            summarize("-----BEGIN OPENSSH PRIVATE KEY-----\nabc", 400),
            Err(Refusal::KeyMaterial)
        );
        assert_eq!(
            summarize(&"word ".repeat(MAX_SOURCE_CHARS), 400),
            Err(Refusal::TooLong)
        );
        assert_eq!(
            summarize("https://a.example/x 123456789", 400),
            Err(Refusal::Empty)
        );
        assert_eq!(summarize("   ", 400), Err(Refusal::Empty));
    }

    #[test]
    fn ordinary_words_survive() {
        let summary = summarize(
            "Summarize the latest research on battery chemistry and compare two approaches.",
            400,
        )
        .unwrap();
        assert_eq!(
            summary.text,
            "Summarize the latest research on battery chemistry and compare two approaches."
        );
    }
}
