//! Regex matcher engine core: walk a tree, apply the matcher registry to
//! each admitted file, and collect per-file candidate matches.
//!
//! `scan_root` is the entry point used by the pack CLI commands (Tasks 2–4);
//! `match_content` is the pure per-file core those and the tests below drive
//! directly.
use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

pub(crate) mod matchers;

pub(crate) use matchers::{registry, NoiseTier};

#[derive(Debug, Clone)]
pub(crate) struct CandidateMatch {
    pub slug: &'static str,
    pub tier: NoiseTier,
    pub line: usize,     // 1-based
    pub excerpt: String, // matched line, trimmed, max 160 chars
}

#[derive(Debug, Clone)]
pub(crate) struct FileCandidates {
    pub path: String, // relative to scan root, forward slashes
    pub matches: Vec<CandidateMatch>,
}

/// Every registry pattern compiled once, tagged with the registry index it
/// came from so a hit can be traced back to its `Matcher`.
fn compiled_patterns() -> &'static [(usize, regex::Regex)] {
    static PATTERNS: OnceLock<Vec<(usize, regex::Regex)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        registry()
            .iter()
            .enumerate()
            .flat_map(|(idx, matcher)| {
                matcher.patterns.iter().map(move |pattern| {
                    let regex = regex::Regex::new(pattern)
                        .unwrap_or_else(|err| panic!("invalid secscan pattern {pattern:?}: {err}"));
                    (idx, regex)
                })
            })
            .collect()
    })
}

/// Line number (1-based) and trimmed text of the line containing `start`.
fn line_at(content: &str, start: usize) -> (usize, &str) {
    let line_start = content[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line_end = content[start..]
        .find('\n')
        .map(|i| start + i)
        .unwrap_or(content.len());
    let line_number = content[..start].bytes().filter(|b| *b == b'\n').count() + 1;
    (line_number, &content[line_start..line_end])
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

pub(crate) fn match_content(content: &str, path: &str) -> Vec<CandidateMatch> {
    let extension = Path::new(path).extension().and_then(|e| e.to_str());
    let registry = registry();
    let mut seen = HashSet::new();
    let mut matches = Vec::new();

    for (idx, regex) in compiled_patterns() {
        let matcher = &registry[*idx];
        if !matcher.extensions.is_empty() {
            match extension {
                Some(ext) if matcher.extensions.contains(&ext) => {}
                _ => continue,
            }
        }

        for found in regex.find_iter(content) {
            let (line, line_text) = line_at(content, found.start());
            if !seen.insert((matcher.slug, line)) {
                continue;
            }
            matches.push(CandidateMatch {
                slug: matcher.slug,
                tier: matcher.tier,
                line,
                excerpt: truncate_chars(line_text.trim(), 160),
            });
        }
    }

    matches
}

const MAX_FILE_BYTES: u64 = 1024 * 1024;

pub(crate) fn scan_root(root: &Path) -> anyhow::Result<Vec<FileCandidates>> {
    let mut files = Vec::new();

    // `require_git` defaults to true, which silently disables `.gitignore`
    // parsing outside an actual git repository (e.g. a bare tempdir in
    // tests, or a pack root that isn't its own git checkout).
    for entry in ignore::WalkBuilder::new(root)
        .hidden(true)
        .require_git(false)
        .build()
    {
        let entry = entry?;
        let is_file = entry.file_type().map(|ft| ft.is_file()).unwrap_or(false);
        if !is_file {
            continue;
        }

        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.len() > MAX_FILE_BYTES {
            continue;
        }

        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let Ok(content) = std::str::from_utf8(&bytes) else {
            continue;
        };
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let relative = relative.to_string_lossy().replace('\\', "/");

        let matches = match_content(content, &relative);
        if matches.is_empty() {
            continue;
        }

        files.push(FileCandidates {
            path: relative,
            matches,
        });
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_content_maps_offsets_to_lines() {
        let content = "fn ok() {}\nlet api_key = \"sk_live_ABCDEF1234567890\";\n";
        let matches = match_content(content, "src/config.rs");
        assert!(
            matches
                .iter()
                .any(|m| m.slug == "secrets-exposure" && m.line == 2),
            "expected secrets-exposure on line 2, got {matches:?}"
        );
    }

    #[test]
    fn extension_gate_excludes_non_matching_files() {
        // graphql-injection is gated to .rs; the same text in a .md must not fire it.
        let content =
            "format!(\"mutation {{ create_Job(input: {{ run_id: \\\"{run_id}\\\" }}) }}\")";
        let rs = match_content(content, "src/a.rs");
        let md = match_content(content, "docs/a.md");
        assert!(rs.iter().any(|m| m.slug == "graphql-injection"));
        assert!(!md.iter().any(|m| m.slug == "graphql-injection"));
    }

    #[test]
    fn scan_root_respects_gitignore_and_returns_relative_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
        std::fs::write(
            dir.path().join("src/leak.rs"),
            "let api_key = \"sk_live_ABCDEF1234567890\";\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("target/leak.rs"),
            "let api_key = \"sk_live_ABCDEF1234567890\";\n",
        )
        .unwrap();
        let files = scan_root(dir.path()).expect("scan");
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["src/leak.rs"]);
    }
}
