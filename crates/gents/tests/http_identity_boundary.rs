use std::path::{Path, PathBuf};

fn rust_sources(root: &Path, output: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_sources(&path, output);
        } else if path.extension().and_then(|value| value.to_str()) == Some("rs") {
            output.push(path);
        }
    }
}

fn production_prefix(source: &str) -> &str {
    source
        .split_once("\n#[cfg(test)]\nmod tests")
        .or_else(|| source.split_once("\n#[cfg(test)]\nmod tx_tests"))
        .map(|(production, _)| production)
        .unwrap_or(source)
}

#[test]
fn production_document_http_has_no_anonymous_escape_hatch() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap();
    let roots = [
        "crates/gents/src",
        "crates/gents-cli/src",
        "crates/gents-desktop-core/src",
        "crates/gents-desktop-bridge/src",
    ];
    let mut violations = Vec::new();

    for relative_root in roots {
        let mut sources = Vec::new();
        rust_sources(&workspace.join(relative_root), &mut sources);
        for path in sources {
            let filename = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("");
            if filename.contains("test") {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            let production = production_prefix(&source);
            let is_central_boundary = path.ends_with("crates/gents/src/config_client/http.rs");
            for banned in [
                "execute_graphql_async(",
                "execute_graphql_async_with_tx(",
                "execute_graphql_blocking(",
                ".post(graphql)",
                ".post(&graphql)",
                ".post(state.graphql)",
            ] {
                if production.contains(banned) {
                    violations.push(format!("{} contains {banned:?}", path.display()));
                }
            }
            if !is_central_boundary {
                for banned in [
                    "execute_graphql_async_authenticated(",
                    "execute_graphql_async_authenticated_with_tx(",
                ] {
                    if production.contains(banned) {
                        violations.push(format!("{} contains {banned:?}", path.display()));
                    }
                }
            }

            let compact = production
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect::<String>();
            for raw_graphql_json in [
                ".json(&json!({\"query\":",
                ".json(&serde_json::json!({\"query\":",
            ] {
                if compact.contains(raw_graphql_json) {
                    violations.push(format!(
                        "{} constructs a raw GraphQL HTTP body {raw_graphql_json:?}",
                        path.display()
                    ));
                }
            }
        }
    }

    let protocol =
        std::fs::read_to_string(workspace.join("crates/gents-protocol/src/graphql.rs")).unwrap();
    for anonymous_api in [
        "pub async fn graphql_endpoint_available(",
        "pub async fn execute_graphql_async(",
        "pub async fn execute_graphql_async_with_tx(",
        "pub fn execute_graphql_blocking(",
    ] {
        if protocol.contains(anonymous_api) {
            violations.push(format!(
                "gents-protocol publicly exposes anonymous API {anonymous_api:?}"
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "all DefraDB document HTTP must flow through AuthenticatedGraphql; the only intentionally unauthenticated probe is GET /api/v0/node/identity (non-document bootstrap/health metadata):\n{}",
        violations.join("\n")
    );
}
