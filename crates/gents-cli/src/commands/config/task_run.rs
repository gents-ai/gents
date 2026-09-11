use anyhow::{anyhow, Context, Result};
use gents::graphql::escape_graphql_string;
use gents::template::{render_template, task_node_ctx, TemplateScope};
use gents_protocol::request_input::RequestInput;
use gents_protocol::row::AgentRequestRow;
use serde::Serialize;
use serde_json::Value;

use crate::cli::ConfigTaskRunArgs;
use crate::config_writes::ConfigAccess;
use crate::request_helpers::{
    content_and_input_with_prompt_selected_skill_ids, ensure_local_request_signer,
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
    pub(crate) input: RequestInput,
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

    let agent_did = crate::resolve_agent_did(args.home.as_deref(), None)?;
    ensure_local_request_signer(args.home.as_deref(), &agent_did)?;
    let task = access
        .transact("cli.task_run.read", |txn| {
            let agent_did = &agent_did;
            let task_id = &task_id;
            Box::pin(async move {
                let (_, value) = gents::config_client::read_desired_state_record_in_txn(
                    txn,
                    gents::Collection::Task,
                    agent_did,
                    task_id,
                )
                .await?
                .context("task not found under the selected principal")?;
                let task: gents::document_config::Task = serde_json::from_value(value)?;
                anyhow::ensure!(task.enabled, "Task {} is disabled", task.task_id);
                let (_, value) = gents::config_client::read_desired_state_record_in_txn(
                    txn,
                    gents::Collection::AgentBehavior,
                    agent_did,
                    &task.behavior_id,
                )
                .await?
                .context("task behavior not found under the selected principal")?;
                let behavior: gents::document_config::AgentBehavior =
                    serde_json::from_value(value)?;
                anyhow::ensure!(
                    behavior.enabled,
                    "AgentBehavior {} is disabled",
                    behavior.behavior_id
                );
                Ok(task)
            })
        })
        .await?;
    let behavior_id = task.behavior_id;
    let prompt_template = task.prompt_template;
    let goal_objective_template = task.goal_objective_template;
    let goal_token_budget = task.goal_token_budget;
    gents::goal::validate_task_goal_declaration(
        goal_objective_template.as_deref(),
        goal_token_budget,
    )?;

    let goal_identity = resolve_task_goal_identity(
        &agent_did,
        &task_id,
        goal_objective_template.as_deref(),
        args.session_id.as_deref(),
    )?;
    let prior_created_at = if let Some(identity) = &goal_identity {
        match lookup_request_by_retry_key(&access, &agent_did, &identity.retry_key).await? {
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
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let (content, input) = content_and_input_with_prompt_selected_skill_ids(None, &content);
    let admission =
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(&agent_did);
    let create = gents::build_signed_request(
        gents::RequestSpec {
            input: input.clone(),
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
            &agent_did,
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
        let response = access.write("cli.task_run.request", &mutation).await?;
        if let Some(errs) = response.get("errors").and_then(|v| v.as_array()) {
            if !errs.is_empty() {
                anyhow::bail!("create manual AgentRequest failed: {errs:?}");
            }
        }
        match extract_doc_id(&response) {
            Some(doc_id) => doc_id,
            None => lookup_doc_id_by_request_id(&access, &agent_did, &request_id)
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
        input,
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
        .filter(|value| !value.trim().is_empty())
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
    agent_did: &str,
    retry_key: &str,
) -> Result<Option<AgentRequestRow>> {
    let query = format!(
        r#"query {{
            AgentRequest(filter: {{ agent_did: {{ _eq: "{owner}" }}, requester_did: {{ _eq: "{owner}" }}, retry_key: {{ _eq: "{key}" }} }}, limit: 2) {{
                _docID
                request_id
                created_at
            }}
        }}"#,
        key = escape_graphql_string(retry_key),
        owner = escape_graphql_string(agent_did),
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
    let positional = positional.filter(|value| !value.trim().is_empty());
    let flag = flag.filter(|value| !value.trim().is_empty());
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
    agent_did: &str,
    request_id: &str,
) -> Result<Option<String>> {
    let query = format!(
        r#"query {{
            AgentRequest(filter: {{ agent_did: {{ _eq: "{owner}" }}, requester_did: {{ _eq: "{owner}" }}, request_id: {{ _eq: "{id}" }} }}, limit: 2) {{
                _docID
            }}
        }}"#,
        id = escape_graphql_string(request_id),
        owner = escape_graphql_string(agent_did),
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

    fn test_manual_mutation(input: RequestInput) -> String {
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
        create.input = input;
        create.caused_by_trigger_kind = Some("manual".to_string());
        create.graphql_mutation().unwrap()
    }

    #[test]
    fn build_mutation_uses_signed_local_self_and_omits_trigger_id() {
        let mutation = test_manual_mutation(RequestInput::default());
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
    fn build_mutation_includes_selected_skill_input_when_present() {
        let mutation = test_manual_mutation(RequestInput {
            selected_skill_ids: vec!["vuln-scan".into()],
            ..Default::default()
        });

        assert!(mutation.contains("input:"));
        assert!(mutation.contains(r#"selected_skill_ids: ["vuln-scan"]"#));
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
        assert_eq!(
            resolve_task_id_for("run", Some(" task "), None).unwrap(),
            " task "
        );
        assert!(resolve_task_id_for("run", Some(" task "), Some("task")).is_err());
    }
}
