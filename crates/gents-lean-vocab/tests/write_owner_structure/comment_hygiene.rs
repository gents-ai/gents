use super::support::{all_rust_sources, repo_root};
use regex::Regex;
use std::collections::BTreeSet;

const GENERATED_SOURCE: &str = "crates/gents-lean-vocab/src/generated.rs";

struct Comment<'a> {
    line: usize,
    text: &'a str,
}

struct HygienePatterns {
    rules: Vec<(&'static str, Regex)>,
}

impl HygienePatterns {
    fn new() -> Self {
        let specs = [
            (
                "issue/PR number",
                r"(?i)(?:^|[^0-9A-Za-z_])#[0-9]{3,5}(?:[^0-9A-Za-z_]|$)",
            ),
            (
                "repo-qualified issue/PR number",
                r"(?i)\b(?:[a-z0-9_-]+/)?[a-z0-9_.-]+#[0-9]{2,5}(?:[^0-9A-Za-z_]|$)",
            ),
            (
                "issue/PR reference",
                r"(?i)\b(?:issue|pr|pull(?:\s+request)?)\s*#?[0-9]{2,5}(?:[^0-9A-Za-z_]|$)",
            ),
            (
                "GitHub issue/PR URL",
                r"(?i)https?://github\.com/[^/\s]+/[^/\s]+/(?:issues|pull)/[0-9]+",
            ),
            (
                "stale history prose",
                r#"(?i)(?:\badapted from\b|\b(?:on|decision)\s+20[0-9]{2}-[0-9]{2}-[0-9]{2}\b|"done when")"#,
            ),
        ];
        Self {
            rules: specs
                .into_iter()
                .map(|(label, pattern)| {
                    (
                        label,
                        Regex::new(pattern).expect("comment hygiene pattern compiles"),
                    )
                })
                .collect(),
        }
    }

    fn first_match(&self, text: &str) -> Option<&'static str> {
        self.rules
            .iter()
            .find_map(|(label, regex)| regex.is_match(text).then_some(*label))
    }
}

fn is_ident_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn raw_string_prefix_len(bytes: &[u8], start: usize) -> Option<(usize, usize)> {
    if start > 0 && is_ident_continue(bytes[start - 1]) {
        return None;
    }
    let after_prefix = match (bytes[start], bytes.get(start + 1).copied()) {
        (b'r', _) => start + 1,
        (b'b' | b'c', Some(b'r')) => start + 2,
        _ => return None,
    };
    let mut hashes = 0;
    while bytes.get(after_prefix + hashes) == Some(&b'#') {
        hashes += 1;
    }
    (bytes.get(after_prefix + hashes) == Some(&b'"')).then_some((after_prefix + hashes + 1, hashes))
}

fn skip_raw_string(bytes: &[u8], start: usize) -> Option<(usize, usize)> {
    let (mut index, hashes) = raw_string_prefix_len(bytes, start)?;
    let mut newlines = 0;
    while index < bytes.len() {
        if bytes[index] == b'\n' {
            newlines += 1;
            index += 1;
            continue;
        }
        if bytes[index] == b'"'
            && index + 1 + hashes <= bytes.len()
            && bytes[index + 1..index + 1 + hashes]
                .iter()
                .all(|byte| *byte == b'#')
        {
            return Some((index + 1 + hashes, newlines));
        }
        index += 1;
    }
    Some((bytes.len(), newlines))
}

fn skip_quoted_string(bytes: &[u8], start: usize) -> Option<(usize, usize)> {
    let mut index = match (bytes[start], bytes.get(start + 1).copied()) {
        (b'"', _) => start + 1,
        (b'b' | b'c', Some(b'"')) if start == 0 || !is_ident_continue(bytes[start - 1]) => {
            start + 2
        }
        _ => return None,
    };
    let mut newlines = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index = (index + 2).min(bytes.len()),
            b'"' => return Some((index + 1, newlines)),
            b'\n' => {
                newlines += 1;
                index += 1;
            }
            _ => index += 1,
        }
    }
    Some((bytes.len(), newlines))
}

fn skip_char_literal(bytes: &[u8], start: usize) -> Option<usize> {
    let quote = match (bytes[start], bytes.get(start + 1).copied()) {
        (b'\'', _) => start + 1,
        (b'b', Some(b'\'')) if start == 0 || !is_ident_continue(bytes[start - 1]) => start + 2,
        _ => return None,
    };
    if quote >= bytes.len() {
        return None;
    }
    if bytes[quote] == b'\\' {
        let mut index = quote + 2;
        while index < bytes.len() {
            if bytes[index] == b'\n' {
                return None;
            }
            if bytes[index] == b'\'' {
                return Some(index + 1);
            }
            index += 1;
        }
        return None;
    }
    let ch = std::str::from_utf8(&bytes[quote..]).ok()?.chars().next()?;
    let end = quote + ch.len_utf8();
    (bytes.get(end) == Some(&b'\'')).then_some(end + 1)
}

fn read_line_comment(bytes: &[u8], start: usize) -> usize {
    let mut index = start + 2;
    while index < bytes.len() && bytes[index] != b'\n' {
        index += 1;
    }
    index
}

fn read_block_comment(bytes: &[u8], start: usize) -> (usize, usize) {
    let mut index = start + 2;
    let mut depth = 1usize;
    let mut newlines = 0;
    while index < bytes.len() && depth > 0 {
        match (bytes[index], bytes.get(index + 1).copied()) {
            (b'\n', _) => {
                newlines += 1;
                index += 1;
            }
            (b'/', Some(b'*')) => {
                depth += 1;
                index += 2;
            }
            (b'*', Some(b'/')) => {
                depth -= 1;
                index += 2;
            }
            _ => index += 1,
        }
    }
    (index, newlines)
}

fn extract_comments(source: &str) -> Vec<Comment<'_>> {
    let bytes = source.as_bytes();
    let mut comments = Vec::new();
    let mut index = 0;
    let mut line = 1usize;
    while index < bytes.len() {
        if let Some((end, newlines)) = skip_raw_string(bytes, index) {
            index = end;
            line += newlines;
        } else if let Some((end, newlines)) = skip_quoted_string(bytes, index) {
            index = end;
            line += newlines;
        } else if let Some(end) = skip_char_literal(bytes, index) {
            index = end;
        } else if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'/') {
            let end = read_line_comment(bytes, index);
            comments.push(Comment {
                line,
                text: &source[index..end],
            });
            index = end;
        } else if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
            let (end, newlines) = read_block_comment(bytes, index);
            comments.push(Comment {
                line,
                text: &source[index..end],
            });
            index = end;
            line += newlines;
        } else {
            if bytes[index] == b'\n' {
                line += 1;
            }
            index += 1;
        }
    }
    comments
}

fn comment_violations(source: &str, patterns: &HygienePatterns) -> Vec<(usize, &'static str)> {
    let mut violations = Vec::new();
    for comment in extract_comments(source) {
        for (offset, line_text) in comment.text.lines().enumerate() {
            if let Some(label) = patterns.first_match(line_text) {
                violations.push((comment.line + offset, label));
            }
        }
    }
    violations
}

/// Comments express only constraints code and proofs cannot state (`SAFETY:`,
/// `Ordering:`, `External premise:`), never issue/PR references or stale
/// change history.
#[test]
fn comments_stay_free_of_issue_references_and_stale_history() {
    let root = repo_root();
    let patterns = HygienePatterns::new();
    let mut sources = Vec::new();
    for directory in ["crates", "apps"] {
        all_rust_sources(&root.join(directory), &mut sources);
    }
    let violations = sources
        .into_iter()
        .filter_map(|path| {
            let relative = path
                .strip_prefix(&root)
                .expect("source under repo root")
                .to_string_lossy()
                .replace('\\', "/");
            if relative == GENERATED_SOURCE {
                return None;
            }
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            let hits = comment_violations(&source, &patterns);
            (!hits.is_empty()).then(|| {
                hits.into_iter()
                    .map(|(line, label)| {
                        format!(
                            "{relative}:{line}: {label} in comment; replace with a concise \
                             constraint (`SAFETY:`, `Ordering:`, or `External premise:`) or delete"
                        )
                    })
                    .collect::<Vec<_>>()
            })
        })
        .flatten()
        .collect::<BTreeSet<_>>();
    assert!(
        violations.is_empty(),
        "comment hygiene rule failed:\n{}",
        violations.into_iter().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn comment_hygiene_fence_flags_issue_refs_and_history_without_false_positives() {
    let patterns = HygienePatterns::new();
    for (source, expected_count) in [
        ("// fix #4123", 1),
        ("/// PR #4200\n/// issue #3900", 2),
        ("//! See gents#3935 and defradb.rs#3006", 1),
        ("/* Adapted from an older #3264 test */", 1),
        ("/* outer /* inner #4123 */ outer */", 1),
        ("// Decision 2026-11-11, maintainer-approved", 1),
        ("/// #3330 \"Done when\": check behavior", 1),
        ("// https://github.com/gents-ai/gents/issues/4236", 1),
        ("/// gents-ai/packs#63", 1),
        ("#[doc = \"issue #4236\"]", 0),
        ("#[derive(Debug)]", 0),
        ("let s = \"// #4123 PR #4200\";", 0),
        ("let r = r#\"/* #4123 */\"#;", 0),
        ("let c = '\"'; let s = \"// #4123\";", 0),
        (
            "// hex colors #ffffff, #123456, #000000 and #[cfg(test)]",
            0,
        ),
        ("// UAX #29 breaks around controls", 0),
        ("// SAFETY: node_did scopes both scan and mutation.", 0),
        (
            "// Ordering: armed only after the queued row is durable.",
            0,
        ),
        (
            "/// External premise: DefraDB checks CollectionGet before writes.",
            0,
        ),
    ] {
        assert_eq!(
            comment_violations(source, &patterns).len(),
            expected_count,
            "{source}"
        );
    }
}
