use std::path::{Component, Path, PathBuf};

use anyhow::Result;
use gents::graphql::escape_graphql_string;
use serde_json::{json, Value};

use crate::cli::output_format::OutputFormat;
use crate::cli::*;
use crate::config_writes::ConfigAccess;
use crate::request_helpers::resolve_dual_id;
use crate::{extract_mutation_doc_id, print_json, resolve_config_access};

// WorkspaceRoot is local-only, operator-managed config (see
// crates/gents-schemas/schemas/agent/workspace_root.graphql): it has no
// agent_did and nothing else references it, so it does not participate in
// the per-agent desired-state manifest / apply / prune machinery that backs
// `crud::config_rm`. list/show reuse the generic crud.rs query path (which
// has no such dependency); set/rm write and delete directly through
// `ConfigAccess::execute`, the same shared write path `behavior_set` drives
// via `write_agent_behavior_document`.

/// Reject relative paths and lexically normalize `.`/`..` components without
/// touching the filesystem. The operator may pre-register a root before it
/// exists, so this cannot use `std::fs::canonicalize` (which requires the
/// path to exist and additionally resolves symlinks).
fn canonicalize_workspace_root_path(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        anyhow::bail!(
            "--path must be an absolute path, got {}; the workspace root does not need to \
             exist yet, but its path must be absolute",
            path.display()
        );
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    Ok(normalized)
}

pub(super) async fn workspace_root_set(args: WorkspaceRootUpsertArgs) -> Result<()> {
    let path = canonicalize_workspace_root_path(&args.path)?;
    let root_path = path.to_string_lossy().into_owned();
    let enabled = !args.disabled;
    let updated_at = chrono::Utc::now().to_rfc3339();

    let access = ConfigAccess::Graphql(args.graphql.clone());
    let root_path_escaped = escape_graphql_string(&root_path);
    let display_name = match args.display_name.as_deref() {
        Some(value) => format!(r#""{}""#, escape_graphql_string(value)),
        None => "null".to_string(),
    };
    let mutable = format!(
        r#"display_name: {display_name}, enabled: {enabled}, updated_at: "{}""#,
        escape_graphql_string(&updated_at)
    );
    let mutation = format!(
        r#"mutation {{
            upsert_WorkspaceRoot(
                filter: {{ root_path: {{ _eq: "{root_path_escaped}" }} }},
                add: {{
                    root_path: "{root_path_escaped}",
                    {mutable}
                }},
                update: {{ {mutable} }}
            ) {{ _docID }}
        }}"#
    );
    let response = access.execute(&mutation).await?;
    let doc_id = extract_mutation_doc_id(&response, "WorkspaceRoot")?;
    print_json(&json!({
        "doc_id": doc_id,
        "root_path": root_path,
        "display_name": args.display_name,
        "enabled": enabled,
    }))?;
    Ok(())
}

pub(super) async fn workspace_root_rm(args: ConfigShowArgs) -> Result<()> {
    let root_path = resolve_dual_id(
        "workspace root",
        "--id",
        args.id.as_deref(),
        args.id_flag.as_deref(),
    )?;
    args.output
        .ensure_supported("config workspace-root rm", &[OutputFormat::Json])?;
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let root_path_escaped = escape_graphql_string(&root_path);
    let mutation = format!(
        r#"mutation {{ delete_WorkspaceRoot(filter: {{ root_path: {{ _eq: "{root_path_escaped}" }} }}) {{ _docID }} }}"#
    );
    let response = access.execute(&mutation).await?;
    let deleted = response
        .get("data")
        .and_then(|data| data.get("delete_WorkspaceRoot"))
        .and_then(Value::as_array)
        .map(|rows| rows.len())
        .unwrap_or(0);
    if deleted == 0 {
        anyhow::bail!("no WorkspaceRoot document with root_path {:?}", root_path);
    }
    print_json(&json!({ "deleted": deleted, "root_path": root_path }))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalize_rejects_relative_paths() {
        let err = canonicalize_workspace_root_path(Path::new("relative/path")).unwrap_err();
        assert!(err.to_string().contains("must be an absolute path"));
    }

    #[test]
    fn canonicalize_normalizes_dot_and_dotdot_lexically() {
        let normalized =
            canonicalize_workspace_root_path(Path::new("/a/./b/../c")).expect("absolute path");
        assert_eq!(normalized, PathBuf::from("/a/c"));
    }

    #[test]
    fn canonicalize_does_not_require_existence() {
        let normalized = canonicalize_workspace_root_path(Path::new("/definitely/not/a/real/path"))
            .expect("absolute path need not exist");
        assert_eq!(normalized, PathBuf::from("/definitely/not/a/real/path"));
    }
}
