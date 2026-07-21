use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

const PROVIDER_FIXTURE_ROOT: &str = "tests/fixtures/providers";

pub fn provider_wire_fixtures_do_not_contain_credentials() -> Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(PROVIDER_FIXTURE_ROOT);
    if !root.exists() {
        return Ok(());
    }

    let mut files = Vec::new();
    collect_fixture_files(&root, &mut files)?;
    for path in files {
        let text = fs::read_to_string(&path)
            .with_context(|| format!("reading provider fixture {}", path.display()))?;
        assert_no_secret_patterns(&path, &text);
    }
    Ok(())
}

fn collect_fixture_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry.with_context(|| format!("reading entry in {}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_fixture_files(&path, out)?;
        } else if is_fixture_file(&path) {
            out.push(path);
        }
    }
    Ok(())
}

fn is_fixture_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some("json" | "jsonl" | "http" | "sse" | "txt")
    )
}

fn assert_no_secret_patterns(path: &Path, text: &str) {
    let lower = text.to_ascii_lowercase();
    let forbidden_substrings = [
        "authorization: bearer ",
        "\"authorization\":\"bearer ",
        "\"authorization\": \"bearer ",
        "sk-proj-",
        "sk-",
        "xoxb-",
        "xoxp-",
        "ghp_",
        "github_pat_",
        "acct_",
        "refresh_token",
        "access_token",
        "id_token",
    ];

    for pattern in forbidden_substrings {
        assert!(
            !lower.contains(pattern),
            "provider fixture {} contains unredacted credential-looking pattern `{pattern}`",
            path.display()
        );
    }

    for key in [
        "key",
        "api_key",
        "access_token",
        "refresh_token",
        "id_token",
    ] {
        assert!(
            !contains_unredacted_query_param(&lower, key),
            "provider fixture {} contains unredacted `{key}` query parameter",
            path.display()
        );
    }
}

fn contains_unredacted_query_param(text: &str, key: &str) -> bool {
    let query_key = format!("{key}=");
    let mut rest = text;
    while let Some(index) = rest.find(&query_key) {
        let value_start = index + query_key.len();
        let value = &rest[value_start..];
        let value = value
            .split(&['&', ' ', '\n', '\r', '"', '\''][..])
            .next()
            .unwrap_or_default();
        if !matches!(value, "" | "<redacted>" | "redacted" | "[redacted]") {
            return true;
        }
        rest = &rest[value_start..];
    }
    false
}
