//! Shared by build-time embedding and runtime pack resolution.
pub(crate) fn is_distributable_asset(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && std::path::Path::new(path)
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
        && !path.split('/').any(|part| {
            part.is_empty()
                || part.starts_with('.')
                || matches!(part, "runs" | "target" | "node_modules" | "__pycache__")
        })
}

/// Canonical spelling for a pack directory or a per-document directory handle.
/// Keep this next to asset admission so build-time embedding and runtime
/// resolution cannot disagree about what may be distributed.
pub(crate) fn is_snake_case_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

pub(crate) fn has_canonical_asset_spelling(path: &str) -> bool {
    let mut parts = path.split('/').peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_some() {
            if !is_snake_case_name(part) {
                return false;
            }
        } else if !matches!(part, "README.md" | "Cargo.toml" | "Cargo.lock")
            && (!part.split('.').all(|segment| {
                !segment.is_empty()
                    && segment.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                    })
            }))
        {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distribution_excludes_private_paths_and_build_artifacts() {
        for path in [
            "",
            "/tmp/key",
            "../key",
            "a/../key",
            "a/./key",
            "a//key",
            "a\\key",
            ".env",
            "a/.key",
            "runs/log.json",
            "target/a",
            "node_modules/a",
            "__pycache__/a",
        ] {
            assert!(!is_distributable_asset(path), "{path}");
        }
        for path in ["README.md", "manifest.json", "tasks/review/object.json"] {
            assert!(is_distributable_asset(path), "{path}");
        }
    }

    #[test]
    fn canonical_pack_and_asset_names_are_unambiguous() {
        for name in ["code_review", "a1", "web_deep_research"] {
            assert!(is_snake_case_name(name), "{name}");
        }
        for name in ["", "_private", "1pack", "code-review", "CodeReview"] {
            assert!(!is_snake_case_name(name), "{name}");
        }
        for path in [
            "README.md",
            "schemas/review_job.graphql",
            "tasks/review_scan_task/object.json",
        ] {
            assert!(has_canonical_asset_spelling(path), "{path}");
        }
        for path in [
            "Tasks/review/object.json",
            "tasks/review-task/object.json",
            "tasks/_review/object.json",
            "tasks/review/Prompt.md",
        ] {
            assert!(!has_canonical_asset_spelling(path), "{path}");
        }
    }
}
