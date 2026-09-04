use anyhow::{anyhow, Context, Result};
use gents::graphql::escape_graphql_string;
use gents::template::{render_template, task_node_ctx, TemplateScope};
use gents_protocol::row::AgentRequestRow;
use serde::Serialize;
use serde_json::Value;

use crate::cli::ConfigTaskRunArgs;
use crate::config_writes::ConfigAccess;
use crate::request_helpers::{
    content_and_metadata_with_prompt_selected_skill_ids, ensure_local_request_signer,
    wait_for_terminal_response,
};
use crate::{print_json, resolve_config_access, resolve_graphql_endpoint};

pub(crate) async fn config_task_run(args: ConfigTaskRunArgs) -> Result<()> {
    let output = enqueue_task_run(&args).await?;
    let mut value = serde_json::to_value(&output)?;
    if args.wait {
        let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
        let response = wait_for_terminal_response(
            &graphql,
            &output.request_id,
            args.timeout_secs,
            args.poll_secs,
        )
        .await?;
        if let Some(object) = value.as_object_mut() {
            object.insert(
                "wait".to_string(),
                serde_json::json!({
                    "timeout_secs": args.timeout_secs,
                    "poll_secs": args.poll_secs,
                    "response": response,
                }),
            );
        }
    }
    print_json(&value)?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct TaskRunOutput {
    pub(crate) task_id: String,
    pub(crate) behavior_id: String,
    pub(crate) agent_did: String,
    pub(crate) request_id: String,
    pub(crate) session_id: String,
    pub(crate) request_doc_id: String,
    pub(crate) metadata: Option<String>,
    pub(crate) status: &'static str,
}

pub(crate) async fn enqueue_task_run(args: &ConfigTaskRunArgs) -> Result<TaskRunOutput> {
    let task_id =
        resolve_task_id_for("run", args.task_id.as_deref(), args.task_id_flag.as_deref())?;

    let args_value: Value =
        serde_json::from_str(&args.args).map_err(|e| anyhow!("--args is not valid JSON: {e}"))?;
    if !args_value.is_object() {
        anyhow::bail!("--args must be a JSON object (got: {args_value})");
    }

    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;

    let task_query = format!(
        r#"query {{
            Task(filter: {{ task_id: {{ _eq: "{id}" }} }}, limit: 1) {{
                task_id
                behavior_id
                prompt_template
                goal_objective_template
                goal_token_budget
                enabled
            }}
        }}"#,
        id = escape_graphql_string(&task_id),
    );
    let task_response = access.execute(&task_query).await?;
    let task_row = task_response
        .get("data")
        .and_then(|d| d.get("Task"))
        .and_then(|arr| arr.as_array())
        .and_then(|arr| arr.first())
        .ok_or_else(|| anyhow!("no Task with task_id = {}", task_id))?;
    let behavior_id = task_row
        .get("behavior_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("Task {} has no behavior_id", task_id))?
        .to_string();
    let prompt_template = task_row
        .get("prompt_template")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let goal_objective_template = match task_row.get("goal_objective_template") {
        None | Some(Value::Null) => None,
        Some(Value::String(template)) if !template.trim().is_empty() => Some(template.clone()),
        Some(Value::String(_)) => {
            anyhow::bail!("Task {} has an empty goal_objective_template", task_id)
        }
        Some(value) => anyhow::bail!(
            "Task {} has a non-string goal_objective_template: {}",
            task_id,
            value
        ),
    };
    let goal_token_budget = match task_row.get("goal_token_budget") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_i64()
                .ok_or_else(|| anyhow!("Task {} has a non-integer goal_token_budget", task_id))?,
        ),
    };
    gents::goal::validate_task_goal_declaration(
        goal_objective_template.as_deref(),
        goal_token_budget,
    )?;
    let enabled = task_row
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !enabled {
        anyhow::bail!("Task {} is disabled; cannot run", task_id);
    }

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
                task_id
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
    ensure_local_request_signer(args.home.as_deref(), &agent_did)?;

    let goal_identity = resolve_task_goal_identity(
        &agent_did,
        &task_id,
        goal_objective_template.as_deref(),
        args.session_id.as_deref(),
    )?;
    let prior_created_at = if let Some(identity) = &goal_identity {
        match lookup_request_by_retry_key(&access, &identity.retry_key).await? {
            Some(request) => {
                anyhow::ensure!(
                    request.request_id == identity.request_id,
                    "goal-backed task retry_key conflicts with request_id {}",
                    request.request_id
                );
                Some(
                    request
                        .created_at
                        .context("goal-backed task request is missing created_at")?,
                )
            }
            None => None,
        }
    } else {
        None
    };
    let now = prior_created_at
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    let (node_scope, ctx_scope) = task_node_ctx(&agent_did, &behavior_id, &now);
    let scope = TemplateScope {
        event: serde_json::json!({
            "fired_at": now,
            "trigger_id": serde_json::Value::Null,
            "trigger_kind": "manual",
        }),
        doc: None,
        args: Some(args_value),
        group: None,
        node: node_scope,
        ctx: ctx_scope,
    };
    let content = render_template(&prompt_template, &scope)
        .map_err(|e| anyhow!("render manual template for task {}: {e}", task_id))?;
    let rendered_goal_objective = goal_objective_template
        .as_deref()
        .map(|template| {
            render_template(template, &scope)
                .map_err(|e| anyhow!("render goal template for task {}: {e}", task_id))
        })
        .transpose()?;
    if rendered_goal_objective
        .as_deref()
        .is_some_and(|objective| objective.trim().is_empty())
    {
        anyhow::bail!("Task {} rendered an empty goal objective", task_id);
    }

    let request_id = goal_identity
        .as_ref()
        .map(|identity| identity.request_id.clone())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let session_id = goal_identity
        .as_ref()
        .map(|identity| identity.session_id.clone())
        .or_else(|| {
            args.session_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let (content, metadata) = content_and_metadata_with_prompt_selected_skill_ids(None, &content);
    let admission =
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(&agent_did);
    let create = gents::build_signed_request(
        gents::RequestSpec {
            metadata: metadata.clone(),
            trigger_lineage: gents::lifecycle::TriggerLineage {
                trigger_kind: Some("manual".to_string()),
                ..Default::default()
            },
            retry_key: goal_identity
                .as_ref()
                .map(|identity| identity.retry_key.clone()),
            ..gents::RequestSpec::new(
                gents::RequestIdentity {
                    request_id: request_id.clone(),
                    agent_did: agent_did.clone(),
                    requester_did: None,
                    behavior_id: behavior_id.clone(),
                    session_id: session_id.clone(),
                    content: content.clone(),
                    execution_origin: gents::lifecycle::ExecutionOrigin::Interactive,
                    created_at: now.clone(),
                },
                admission,
            )
        },
        gents::RequestSigner::RegisteredTarget,
    )
    .await?;
    let doc_id = if let Some(objective) = rendered_goal_objective.as_deref() {
        gents::goal::submit_goal_backed_request(
            &access,
            &agent_did,
            &session_id,
            objective,
            goal_token_budget,
            &create,
        )
        .await?;
        lookup_request_by_retry_key(
            &access,
            goal_identity
                .as_ref()
                .expect("rendered goal has deterministic identity")
                .retry_key
                .as_str(),
        )
            .await?
            .map(|request| {
                request
                    .doc_id
                    .context("goal-backed task request is missing _docID")
            })
            .transpose()?
            .ok_or_else(|| {
                anyhow!(
                    "goal-backed AgentRequest for task {} committed but lookup by retry_key returned nothing",
                    task_id
                )
            })?
    } else {
        let mutation = create.graphql_mutation().map_err(anyhow::Error::msg)?;
        let response = access.execute(&mutation).await?;
        if let Some(errs) = response.get("errors").and_then(|v| v.as_array()) {
            if !errs.is_empty() {
                anyhow::bail!("create manual AgentRequest failed: {errs:?}");
            }
        }
        match extract_doc_id(&response) {
            Some(doc_id) => doc_id,
            None => lookup_doc_id_by_request_id(&access, &request_id)
                .await?
                .ok_or_else(|| {
                    anyhow!(
                        "manual AgentRequest for task {} persisted but _docID lookup by request_id returned nothing",
                        task_id
                    )
                })?,
        }
    };

    Ok(TaskRunOutput {
        task_id,
        behavior_id,
        agent_did,
        request_id,
        session_id,
        request_doc_id: doc_id,
        metadata,
        status: "pending",
    })
}

fn resolve_task_goal_identity(
    agent_did: &str,
    task_id: &str,
    goal_objective_template: Option<&str>,
    session_id: Option<&str>,
) -> Result<Option<gents::goal::TaskGoalFireIdentity>> {
    let Some(_) = goal_objective_template else {
        return Ok(None);
    };
    let session_id = session_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "Task {} declares a durable goal; pass --session-id with a stable invocation identity so retries converge",
                task_id
            )
    })?;
    Ok(Some(gents::goal::task_goal_fire_identity(
        agent_did,
        task_id,
        &format!("cli:{session_id}"),
    )))
}

async fn lookup_request_by_retry_key(
    access: &ConfigAccess,
    retry_key: &str,
) -> Result<Option<AgentRequestRow>> {
    let query = format!(
        r#"query {{
            AgentRequest(filter: {{ retry_key: {{ _eq: "{key}" }} }}, limit: 2) {{
                _docID
                request_id
                created_at
            }}
        }}"#,
        key = escape_graphql_string(retry_key),
    );
    let response = access.execute(&query).await?;
    if let Some(errors) = response.get("errors").and_then(Value::as_array) {
        if !errors.is_empty() {
            anyhow::bail!("lookup goal-backed task request failed: {errors:?}");
        }
    }
    let rows = response
        .get("data")
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if rows.len() > 1 {
        anyhow::bail!("goal-backed task retry_key resolved to multiple AgentRequest rows");
    }
    rows.first()
        .cloned()
        .map(|row| {
            serde_json::from_value(row)
                .context("decoding goal-backed task canonical AgentRequest row")
        })
        .transpose()
}

#[cfg(test)]
mod goal_identity_tests {
    use super::resolve_task_goal_identity;

    #[test]
    fn ordinary_task_run_needs_no_stable_session() {
        assert!(
            resolve_task_goal_identity("did:test:one", "task", None, None)
                .expect("ordinary identity")
                .is_none()
        );
    }

    #[test]
    fn durable_task_run_requires_stable_session() {
        let error = resolve_task_goal_identity("did:test:one", "task", Some("objective"), None)
            .expect_err("durable task must require session");
        assert!(error.to_string().contains("--session-id"));
    }

    #[test]
    fn durable_task_retry_identity_is_deterministic() {
        let first =
            resolve_task_goal_identity("did:test:one", "task", Some("objective"), Some("run-42"))
                .expect("first identity")
                .expect("goal identity");
        let retry =
            resolve_task_goal_identity("did:test:one", "task", Some("objective"), Some("run-42"))
                .expect("retry identity")
                .expect("goal identity");
        assert_eq!(first, retry);
        assert_ne!(first.session_id, "run-42");
        assert!(first.session_id.contains("task-goal-session"));
    }
}

pub(crate) fn resolve_task_id_for(
    command: &str,
    positional: Option<&str>,
    flag: Option<&str>,
) -> Result<String> {
    let positional = positional.map(str::trim).filter(|value| !value.is_empty());
    let flag = flag.map(str::trim).filter(|value| !value.is_empty());
    match (positional, flag) {
        (Some(positional), Some(flag)) if positional != flag => {
            anyhow::bail!(
                "conflicting task ids provided: positional={} and --task-id={}\nNext:\n  1. Pass the task id once: `gents task {command} TASK_ID`\n  2. Or use `--task-id TASK_ID`, but not both",
                positional,
                flag
            );
        }
        (Some(task_id), _) | (_, Some(task_id)) => Ok(task_id.to_string()),
        (None, None) => anyhow::bail!(
            "missing task id\nNext:\n  1. Pass it positionally: `gents task {command} TASK_ID`\n  2. Or use `--task-id TASK_ID`"
        ),
    }
}

async fn lookup_doc_id_by_request_id(
    access: &ConfigAccess,
    request_id: &str,
) -> Result<Option<String>> {
    let query = format!(
        r#"query {{
            AgentRequest(filter: {{ request_id: {{ _eq: "{id}" }} }}, limit: 2) {{
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
    let rows = response
        .get("data")
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|arr| arr.as_array())
        .cloned()
        .unwrap_or_default();
    if rows.len() > 1 {
        anyhow::bail!(
            "lookup AgentRequest by request_id {request_id} is ambiguous across {} documents",
            rows.len()
        );
    }
    Ok(rows
        .first()
        .and_then(|row| row.get("_docID"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string()))
}

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

    fn test_manual_mutation(metadata: Option<&str>) -> String {
        let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
            "req-1",
            "did:test:test",
            "did:test:test",
            "behavior-1",
            "sess-1",
            "hello Amy",
            "interactive",
            "2026-04-21T00:00:00Z",
            gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
                "did:test:test",
            ),
        );
        create.admission.signature = vec![0; 64];
        create.metadata = metadata.map(ToOwned::to_owned);
        create.caused_by_trigger_kind = Some("manual".to_string());
        create.graphql_mutation().unwrap()
    }

    #[test]
    fn build_mutation_uses_signed_local_self_and_omits_trigger_id() {
        let mutation = test_manual_mutation(None);
        assert!(mutation.contains("admission_kind: \"local-self\""));
        assert!(mutation.contains("caused_by_trigger_kind: \"manual\""));
        assert!(
            !mutation.contains("caused_by_trigger_id:"),
            "caused_by_trigger_id must be omitted so it stays null for manual runs"
        );
        assert!(mutation.contains("execution_origin: \"interactive\""));
        assert!(mutation.contains("lifecycle_state: \"pending\""));
        assert!(mutation.contains("content: \"hello Amy\""));
    }

    #[test]
    fn build_mutation_includes_selected_skill_metadata_when_present() {
        let mutation = test_manual_mutation(Some(r#"{"selected_skill_ids":["vuln-scan"]}"#));

        assert!(mutation.contains("metadata:"));
        assert!(mutation.contains(r#"\"selected_skill_ids\":[\"vuln-scan\"]"#));
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
        let add_object = serde_json::json!({
            "data": { "add_AgentRequest": { "_docID": "doc-3" } }
        });
        assert_eq!(extract_doc_id(&add_object), Some("doc-3".to_string()));

        let add_array = serde_json::json!({
            "data": { "add_AgentRequest": [ { "_docID": "doc-4" } ] }
        });
        assert_eq!(extract_doc_id(&add_array), Some("doc-4".to_string()));
    }

    #[test]
    fn resolve_task_id_accepts_positional_or_flag_and_rejects_conflict() {
        assert_eq!(
            resolve_task_id_for("run", Some("host-check"), None).unwrap(),
            "host-check"
        );
        assert_eq!(
            resolve_task_id_for("run", None, Some("host-check")).unwrap(),
            "host-check"
        );
        assert_eq!(
            resolve_task_id_for("run", Some("host-check"), Some("host-check")).unwrap(),
            "host-check"
        );
        assert!(resolve_task_id_for("run", Some("host-check"), Some("other")).is_err());
        assert!(resolve_task_id_for("run", None, None).is_err());
    }
}
