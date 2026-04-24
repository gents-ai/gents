//! `config task run --task-id <id> --args <JSON>` — fire a configured Task
//! once against a running agent.
//!
//! This path intentionally duplicates the mutation shape produced by
//! `defra_agent::write_manual_agent_request` instead of calling it: that
//! helper takes `&EmbeddedNode`, and the CLI generally runs against a remote
//! agent over GraphQL-over-HTTP via [`ConfigAccess::Graphql`]. Both paths
//! produce the same `(caused_by_trigger_id = null,
//! caused_by_trigger_kind = "manual")` lineage tuple and the same
//! `execution_origin = "interactive"`, so observers can treat them as one
//! origin.
//!
//! When running against a local embedded node via [`ConfigAccess::Local`],
//! the same mutation runs through the node's GraphQL layer — there is no
//! reason to branch.

use anyhow::{anyhow, Result};
use defra_agent::graphql::escape_graphql_string;
use defra_agent::template::{render_template, TemplateScope};
use serde_json::Value;

use crate::cli::ConfigTaskRunArgs;
use crate::config_writes::ConfigAccess;
use crate::{print_json, resolve_config_access};

/// Must match `lifecycle::DEFAULT_REQUEST_MAX_RETRIES` and the value written by
/// `write_manual_agent_request`. Kept inline here so the CLI mutation is a
/// self-contained record of the shape — the defra-agent constant is not
/// re-exported.
const DEFAULT_REQUEST_MAX_RETRIES: u32 = 3;

pub(super) async fn config_task_run(args: ConfigTaskRunArgs) -> Result<()> {
    // 1. Parse --args as a JSON object.
    let args_value: Value =
        serde_json::from_str(&args.args).map_err(|e| anyhow!("--args is not valid JSON: {e}"))?;
    if !args_value.is_object() {
        anyhow::bail!("--args must be a JSON object (got: {args_value})");
    }

    // 2. Resolve GraphQL / local access the same way every other config
    //    subcommand does. Ensures local schemas if we fell back to an
    //    embedded node.
    let (access, _) = resolve_config_access(
        args.home.as_deref(),
        args.graphql.as_deref(),
        /* ensure_local_schemas */ true,
    )
    .await?;

    // 3. Fetch the Task doc.
    let task_query = format!(
        r#"query {{
            Task(filter: {{ task_id: {{ _eq: "{id}" }} }}, limit: 1) {{
                task_id
                behavior_id
                prompt_template
                enabled
            }}
        }}"#,
        id = escape_graphql_string(&args.task_id),
    );
    let task_response = access.execute(&task_query).await?;
    let task_row = task_response
        .get("data")
        .and_then(|d| d.get("Task"))
        .and_then(|arr| arr.as_array())
        .and_then(|arr| arr.first())
        .ok_or_else(|| anyhow!("no Task with task_id = {}", args.task_id))?;
    let behavior_id = task_row
        .get("behavior_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("Task {} has no behavior_id", args.task_id))?
        .to_string();
    let prompt_template = task_row
        .get("prompt_template")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let enabled = task_row
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !enabled {
        anyhow::bail!("Task {} is disabled; cannot run", args.task_id);
    }

    // 4. Fetch the AgentBehavior to get agent_did and confirm it's enabled.
    let behavior_query = format!(
        r#"query {{
            AgentBehavior(filter: {{ behavior_id: {{ _eq: "{id}" }} }}, limit: 1) {{
                agent_did
                enabled
            }}
        }}"#,
        id = escape_graphql_string(&behavior_id),
    );
    let behavior_response = access.execute(&behavior_query).await?;
    let behavior_row = behavior_response
        .get("data")
        .and_then(|d| d.get("AgentBehavior"))
        .and_then(|arr| arr.as_array())
        .and_then(|arr| arr.first())
        .ok_or_else(|| {
            anyhow!(
                "no AgentBehavior with behavior_id = {} (referenced by task {})",
                behavior_id,
                args.task_id
            )
        })?;
    let agent_did = behavior_row
        .get("agent_did")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("AgentBehavior {} has no agent_did", behavior_id))?
        .to_string();
    if !behavior_row
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        anyhow::bail!("AgentBehavior {} is disabled", behavior_id);
    }

    // 5. Render the prompt template. Matches the `TemplateScope` shape used
    //    by `write_manual_agent_request`.
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let scope = TemplateScope {
        event: serde_json::json!({
            "fired_at": now,
            "trigger_id": serde_json::Value::Null,
            "trigger_kind": "manual",
        }),
        doc: None,
        args: Some(args_value),
    };
    let content = render_template(&prompt_template, &scope)
        .map_err(|e| anyhow!("render manual template for task {}: {e}", args.task_id))?;

    // 6. Issue the same create_AgentRequest mutation as
    //    `write_manual_agent_request`: same field set, same lineage.
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let mutation = build_create_manual_request_mutation(CreateManualRequestInput {
        request_id: &request_id,
        session_id: &session_id,
        agent_did: &agent_did,
        behavior_id: &behavior_id,
        content: &content,
        created_at: &now,
    });
    let response = access.execute(&mutation).await?;
    if let Some(errs) = response.get("errors").and_then(|v| v.as_array()) {
        if !errs.is_empty() {
            anyhow::bail!("create manual AgentRequest failed: {errs:?}");
        }
    }
    // The mutation succeeded (no `errors` array). `create_AgentRequest` may
    // return the `_docID` inline, or it may omit it entirely — mirroring the
    // quirk documented in `defra_agent::lifecycle::manual`. When inline
    // extraction yields nothing, fall back to a follow-up query filtered by
    // the just-written `request_id`. The row exists either way; treating the
    // missing-inline shape as a hard failure would make the operator retry
    // and double-fire the task.
    let doc_id = match extract_doc_id(&response) {
        Some(doc_id) => doc_id,
        None => lookup_doc_id_by_request_id(&access, &request_id)
            .await?
            .ok_or_else(|| {
                anyhow!(
                    "manual AgentRequest for task {} persisted but _docID lookup by request_id returned nothing",
                    args.task_id
                )
            })?,
    };

    // 7. Print the structured result.
    print_json(&serde_json::json!({
        "task_id": args.task_id,
        "behavior_id": behavior_id,
        "agent_did": agent_did,
        "request_id": request_id,
        "session_id": session_id,
        "request_doc_id": doc_id,
        "status": "pending",
    }))?;
    Ok(())
}

struct CreateManualRequestInput<'a> {
    request_id: &'a str,
    session_id: &'a str,
    agent_did: &'a str,
    behavior_id: &'a str,
    content: &'a str,
    created_at: &'a str,
}

fn build_create_manual_request_mutation(input: CreateManualRequestInput<'_>) -> String {
    // `caused_by_trigger_id` is intentionally omitted so it stays null in the
    // persisted document — manual runs have no trigger id to reference.
    format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{request_id}",
                agent_did: "{agent_did}",
                behavior_id: "{behavior_id}",
                session_id: "{session_id}",
                retry_parent_request: "",
                retry_root_request: "{request_id}",
                superseded_by_request: "",
                content: "{content}",
                status: "pending",
                lifecycle_state: "pending",
                backend_id: "",
                execution_origin: "interactive",
                caused_by_trigger_kind: "manual",
                failure_reason: "",
                created_at: "{created_at}",
                retry_count: 0,
                max_retries: {max_retries}
            }}) {{ _docID }}
        }}"#,
        request_id = escape_graphql_string(input.request_id),
        agent_did = escape_graphql_string(input.agent_did),
        behavior_id = escape_graphql_string(input.behavior_id),
        session_id = escape_graphql_string(input.session_id),
        content = escape_graphql_string(input.content),
        created_at = escape_graphql_string(input.created_at),
        max_retries = DEFAULT_REQUEST_MAX_RETRIES,
    )
}

/// Fallback for when `create_AgentRequest` succeeded (no `errors` array) but
/// the response did not echo the `_docID` inline. The row exists; fetch it
/// by `request_id` so we can report a stable `request_doc_id` without
/// resorting to a retry that would double-fire the task.
async fn lookup_doc_id_by_request_id(
    access: &ConfigAccess,
    request_id: &str,
) -> Result<Option<String>> {
    let query = format!(
        r#"query {{
            AgentRequest(filter: {{ request_id: {{ _eq: "{id}" }} }}, limit: 1) {{
                _docID
            }}
        }}"#,
        id = escape_graphql_string(request_id),
    );
    let response = access.execute(&query).await?;
    if let Some(errs) = response.get("errors").and_then(|v| v.as_array()) {
        if !errs.is_empty() {
            anyhow::bail!("lookup AgentRequest by request_id {request_id} failed: {errs:?}");
        }
    }
    Ok(response
        .get("data")
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|arr| arr.as_array())
        .and_then(|arr| arr.first())
        .and_then(|row| row.get("_docID"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string()))
}

/// Pull the `_docID` out of a create-AgentRequest response.
///
/// DefraDB accepts both `create_<Type>` and `add_<Type>` mutation forms, and
/// returns the result under whichever *response* field name it chose — which
/// can differ from the requested alias. In practice `create_AgentRequest`
/// shows up in the response as `add_AgentRequest`. On top of that, the value
/// may be a single object or an array. Handle every shape.
fn extract_doc_id(response: &Value) -> Option<String> {
    let data = response.get("data")?;
    let candidates = [
        data.get("create_AgentRequest"),
        data.get("add_AgentRequest"),
    ];
    for value in candidates.into_iter().flatten() {
        if let Some(doc_id) = value.get("_docID").and_then(|v| v.as_str()) {
            return Some(doc_id.to_string());
        }
        if let Some(doc_id) = value
            .as_array()
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("_docID"))
            .and_then(|v| v.as_str())
        {
            return Some(doc_id.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_mutation_includes_manual_lineage_and_omits_trigger_id() {
        let mutation = build_create_manual_request_mutation(CreateManualRequestInput {
            request_id: "req-1",
            session_id: "sess-1",
            agent_did: "did:defra-agent:test",
            behavior_id: "behavior-1",
            content: "hello Amy",
            created_at: "2026-04-21T00:00:00Z",
        });
        assert!(mutation.contains("caused_by_trigger_kind: \"manual\""));
        assert!(
            !mutation.contains("caused_by_trigger_id:"),
            "caused_by_trigger_id must be omitted so it stays null for manual runs"
        );
        assert!(mutation.contains("execution_origin: \"interactive\""));
        assert!(mutation.contains("lifecycle_state: \"pending\""));
        assert!(mutation.contains("status: \"pending\""));
        assert!(mutation.contains("content: \"hello Amy\""));
    }

    #[test]
    fn extract_doc_id_handles_object_and_array_shapes() {
        let object_shape = serde_json::json!({
            "data": { "create_AgentRequest": { "_docID": "doc-1" } }
        });
        assert_eq!(extract_doc_id(&object_shape), Some("doc-1".to_string()));

        let array_shape = serde_json::json!({
            "data": { "create_AgentRequest": [ { "_docID": "doc-2" } ] }
        });
        assert_eq!(extract_doc_id(&array_shape), Some("doc-2".to_string()));

        let empty = serde_json::json!({
            "data": { "create_AgentRequest": [] }
        });
        assert_eq!(extract_doc_id(&empty), None);
    }

    #[test]
    fn extract_doc_id_returns_none_when_response_omits_doc_id_entirely() {
        // Pins the quirk: a `create_AgentRequest` mutation can succeed (no
        // `errors` array) while its inline payload carries no `_docID`.
        // The caller must treat this as "fall back to a request_id lookup",
        // not as "mutation failed"; a retry would double-fire the task.
        let object_without_doc_id = serde_json::json!({
            "data": { "create_AgentRequest": {} }
        });
        assert_eq!(extract_doc_id(&object_without_doc_id), None);

        let array_without_doc_id = serde_json::json!({
            "data": { "create_AgentRequest": [ {} ] }
        });
        assert_eq!(extract_doc_id(&array_without_doc_id), None);

        let missing_field = serde_json::json!({ "data": {} });
        assert_eq!(extract_doc_id(&missing_field), None);
    }

    #[test]
    fn extract_doc_id_handles_add_alias_response() {
        // DefraDB returns `add_AgentRequest` when we ask for
        // `create_AgentRequest` — both shapes need to work.
        let add_object = serde_json::json!({
            "data": { "add_AgentRequest": { "_docID": "doc-3" } }
        });
        assert_eq!(extract_doc_id(&add_object), Some("doc-3".to_string()));

        let add_array = serde_json::json!({
            "data": { "add_AgentRequest": [ { "_docID": "doc-4" } ] }
        });
        assert_eq!(extract_doc_id(&add_array), Some("doc-4".to_string()));
    }
}
