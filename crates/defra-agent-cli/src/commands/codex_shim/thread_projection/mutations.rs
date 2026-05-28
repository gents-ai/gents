use anyhow::{Context, Result};
use codex_app_server_protocol as codex;
use defra_agent::graphql::escape_graphql_string;
use serde_json::Value;

use crate::commands::codex_shim::protocol::absolute_path;
use crate::commands::codex_shim::store::query_node_json;
use crate::commands::codex_shim::ShimState;

use super::{load_codex_thread, CodexThreadRecord};

pub(in crate::commands::codex_shim) async fn loaded_codex_thread_ids(
    state: &ShimState,
) -> Result<Vec<String>> {
    let response = query_node_json(
        &state.node,
        r#"{
            CodexThreadProjection(
                filter: { loaded: { _eq: true }, archived: { _eq: false } },
                order: { updated_at: DESC }
            ) { session_id }
        }"#,
    )
    .await?;
    Ok(response
        .pointer("/data/CodexThreadProjection")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| {
            row.get("session_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .collect())
}

pub(in crate::commands::codex_shim) async fn set_codex_thread_loaded(
    state: &ShimState,
    thread_id: &str,
    loaded: bool,
) -> Result<()> {
    let escaped_thread_id = escape_graphql_string(thread_id);
    let mutation = format!(
        r#"mutation {{
            update_CodexThreadProjection(
                filter: {{ session_id: {{ _eq: "{escaped_thread_id}" }} }},
                input: {{ loaded: {loaded}, updated_at: "{now}" }}
            ) {{ _docID }}
        }}"#,
        now = chrono::Utc::now().to_rfc3339(),
    );
    query_node_json(&state.node, &mutation).await?;
    Ok(())
}

pub(in crate::commands::codex_shim) async fn set_codex_thread_archived(
    state: &ShimState,
    thread_id: &str,
    archived: bool,
) -> Result<()> {
    let escaped_thread_id = escape_graphql_string(thread_id);
    let mutation = format!(
        r#"mutation {{
            update_CodexThreadProjection(
                filter: {{ session_id: {{ _eq: "{escaped_thread_id}" }} }},
                input: {{ archived: {archived}, loaded: false, updated_at: "{now}" }}
            ) {{ _docID }}
        }}"#,
        now = chrono::Utc::now().to_rfc3339(),
    );
    query_node_json(&state.node, &mutation).await?;
    Ok(())
}

pub(in crate::commands::codex_shim) async fn set_codex_thread_name(
    state: &ShimState,
    thread_id: &str,
    name: &str,
) -> Result<()> {
    let escaped_thread_id = escape_graphql_string(thread_id);
    let escaped_name = escape_graphql_string(name.trim());
    let mutation = format!(
        r#"mutation {{
            update_CodexThreadProjection(
                filter: {{ session_id: {{ _eq: "{escaped_thread_id}" }} }},
                input: {{ name: "{escaped_name}", updated_at: "{now}" }}
            ) {{ _docID }}
        }}"#,
        now = chrono::Utc::now().to_rfc3339(),
    );
    query_node_json(&state.node, &mutation).await?;
    Ok(())
}

pub(in crate::commands::codex_shim) async fn set_codex_thread_memory_mode(
    state: &ShimState,
    thread_id: &str,
    mode: codex::ThreadMemoryMode,
) -> Result<()> {
    let escaped_thread_id = escape_graphql_string(thread_id);
    let escaped_mode = escape_graphql_string(mode.as_str());
    let mutation = format!(
        r#"mutation {{
            update_CodexThreadProjection(
                filter: {{ session_id: {{ _eq: "{escaped_thread_id}" }} }},
                input: {{ memory_mode: "{escaped_mode}", updated_at: "{now}" }}
            ) {{ _docID }}
        }}"#,
        now = chrono::Utc::now().to_rfc3339(),
    );
    query_node_json(&state.node, &mutation).await?;
    Ok(())
}

pub(in crate::commands::codex_shim) async fn set_codex_thread_settings(
    state: &ShimState,
    thread_id: &str,
    settings: &codex::ThreadSettingsUpdateParams,
) -> Result<()> {
    let escaped_thread_id = escape_graphql_string(thread_id);
    let settings_json =
        serde_json::to_string(settings).context("encoding Codex thread settings")?;
    let escaped_settings = escape_graphql_string(&settings_json);
    let cwd_update = settings
        .cwd
        .as_deref()
        .map(|cwd| {
            let cwd = if cwd.is_absolute() {
                cwd.to_path_buf()
            } else {
                state.cwd.join(cwd)
            };
            format!(
                r#", cwd: "{}""#,
                escape_graphql_string(&absolute_path(&cwd))
            )
        })
        .unwrap_or_default();
    let mutation = format!(
        r#"mutation {{
            update_CodexThreadProjection(
                filter: {{ session_id: {{ _eq: "{escaped_thread_id}" }} }},
                input: {{ settings_json: "{escaped_settings}"{cwd_update}, updated_at: "{now}" }}
            ) {{ _docID }}
        }}"#,
        now = chrono::Utc::now().to_rfc3339(),
    );
    query_node_json(&state.node, &mutation).await?;
    Ok(())
}

pub(in crate::commands::codex_shim) async fn set_codex_thread_git_info(
    state: &ShimState,
    thread_id: &str,
    git_info: &Option<codex::ThreadMetadataGitInfoUpdateParams>,
) -> Result<Option<CodexThreadRecord>> {
    let git_info_json = serde_json::to_string(git_info).context("encoding Codex git metadata")?;
    let escaped_thread_id = escape_graphql_string(thread_id);
    let escaped_git_info = escape_graphql_string(&git_info_json);
    let mutation = format!(
        r#"mutation {{
            update_CodexThreadProjection(
                filter: {{ session_id: {{ _eq: "{escaped_thread_id}" }} }},
                input: {{ git_info_json: "{escaped_git_info}", updated_at: "{now}" }}
            ) {{ _docID }}
        }}"#,
        now = chrono::Utc::now().to_rfc3339(),
    );
    query_node_json(&state.node, &mutation).await?;
    load_codex_thread(state, thread_id).await
}
