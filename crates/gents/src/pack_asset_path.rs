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
}
