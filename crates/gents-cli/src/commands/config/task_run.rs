use anyhow::{anyhow, Context, Result};
use gents::documents::template::{render_template, task_node_ctx, TemplateScope};
use gents::graphql::escape_graphql_string;
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
    pub(crate) agent_id: String,
    pub(crate) node_did: String,
    pub(crate) request_id: String,
    pub(crate) session_id: String,
    pub(crate) request_doc_id: String,
    pub(crate) input: RequestInput,
    pub(crate) status: &'static str,
    pub(crate) duplicate: bool,
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

    let node_did = crate::resolve_node_did(args.home.as_deref(), None)?;
    ensure_local_request_signer(args.home.as_deref(), &node_did)?;
    let (task_doc_id, task) = access
        .transact_write("cli.task_run.read", |txn| {
            let node_did = &node_did;
            let task_id = &task_id;
            Box::pin(async move {
                let (task_doc_id, value) = gents::config_client::read_desired_state_record_in_txn(
                    txn,
                    gents::Collection::Task,
                    node_did,
                    task_id,
                )
                .await?
                .context("task not found under the selected node")?;
                let task: gents::documents::Task = serde_json::from_value(value)?;
                anyhow::ensure!(task.enabled, "Task {} is disabled", task.task_id);
                let (_, value) = gents::config_client::read_desired_state_record_in_txn(
                    txn,
                    gents::Collection::Agent,
                    node_did,
                    &task.agent_id,
                )
                .await?
                .context("task agent not found under the selected node")?;
                let agent: gents::documents::Agent = serde_json::from_value(value)?;
                anyhow::ensure!(agent.enabled, "Agent {} is disabled", agent.agent_id);
                Ok((task_doc_id, task))
            })
        })
        .await?;
    let emit_outcome = task.emit_outcome;
    let agent_id = task.agent_id;
    let prompt_template = task.prompt_template;
    let goal_objective_template = task.goal_objective_template;
    let goal_token_budget = task.goal_token_budget;
    gents::goal::validate_task_goal_declaration(
        goal_objective_template.as_deref(),
        goal_token_budget,
    )?;

    let invocation_key = resolve_invocation_key(
        &task_id,
        goal_objective_template.is_some(),
        goal_objective_template
            .as_ref()
            .and(args.session_id.as_deref()),
    )?;
    let identity = gents_protocol::trigger_delivery::FireIdentity {
        owner_did: node_did.clone(),
        trigger_id: format!("manual:{task_id}:{invocation_key}"),
        source_collection: "Task".into(),
        source_doc_id: task_doc_id,
    };
    let fire_key = gents::lifecycle::task_fire_key(&identity);
    let request_id = format!("trigger-request:{fire_key}");
    anyhow::ensure!(goal_objective_template.is_some() || args.session_id.is_none() || args.continue_session.is_none(),
        "ordinary Tasks accept either --session-id for a session label or --continue-session for an existing session, not both");
    let session_id = match args.continue_session.as_deref() {
        Some(id) => {
            anyhow::ensure!(
                !id.trim().is_empty(),
                "--continue-session must not be empty"
            );
            id.to_owned()
        }
        None => args
            .session_id
            .as_ref()
            .filter(|id| goal_objective_template.is_none() && !id.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| format!("trigger-session:{fire_key}")),
    };
    let admitted = lookup_request_by_retry_key(&access, &node_did, &fire_key).await?;
    let prior_created_at = admitted
        .as_ref()
        .map(|request| {
            anyhow::ensure!(
                request.request_id == request_id,
                "Task fire identity conflicts with request ID"
            );
            request
                .created_at
                .clone()
                .context("Task fire request is missing created_at")
        })
        .transpose()?;
    let now = prior_created_at
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    let (node_scope, ctx_scope) = task_node_ctx(&node_did, &agent_id, &now);
    let scope = TemplateScope {
        session: Some(serde_json::json!({"session_id": session_id})),
        request: Some(serde_json::json!({"request_id": request_id})),
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

    let (content, input) = content_and_input_with_prompt_selected_skill_ids(None, &content);
    let input = prepare_task_run_input(
        &access,
        &node_did,
        &session_id,
        args.continue_session.is_some(),
        rendered_goal_objective.is_some(),
        admitted.as_ref(),
        input,
    )
    .await?;
    let goal_id = prepare_task_run_goal_id(
        &access,
        &node_did,
        &session_id,
        rendered_goal_objective.is_some(),
        admitted.as_ref(),
    )
    .await?;
    let admission =
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(&node_did);
    let create = gents::build_signed_request(
        gents::RequestSpec {
            input: input.clone(),
            trigger_lineage: gents::lifecycle::TriggerLineage {
                trigger_kind: Some("manual".to_string()),
                ..Default::default()
            },
            retry_key: Some(fire_key.clone()),
            ..gents::RequestSpec::new(
                gents_protocol::request_admission::RequestPurpose::Normal,
                gents::RequestIdentity {
                    request_id: request_id.clone(),
                    node_did: node_did.clone(),
                    requester_did: None,
                    agent_id: agent_id.clone(),
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
    let fire = gents_protocol::trigger_delivery::TriggerFire {
        fire_key,
        identity,
        task_id: task_id.clone(),
        request_id: request_id.clone(),
        session_id: session_id.clone(),
        goal_id,
        goal_objective: rendered_goal_objective,
        goal_token_budget,
        goal_assignment_applied: false,
        emit_outcome,
        queued_serial: false,
        source_handoff_id: Some(invocation_key),
        reply_session_id: None,
        shard_id: None,
        attempt: None,
        created_at: now,
    };
    let admitted = gents::lifecycle::write_task_delivery(
        &access,
        &fire,
        args.continue_session.is_some(),
        &create,
    )
    .await?;

    let (agent_id, input) = if admitted.duplicate {
        let persisted = lookup_request_by_retry_key(&access, &node_did, &fire.fire_key)
            .await?
            .context("duplicate Task fire is missing its admitted request")?;
        (
            persisted
                .agent_id
                .context("admitted Task request lacks agent_id")?,
            persisted.input.unwrap_or_default(),
        )
    } else {
        (agent_id, input)
    };
    Ok(TaskRunOutput {
        task_id,
        agent_id,
        node_did,
        request_id: admitted.request.request_id,
        session_id: admitted.request.session_id,
        request_doc_id: admitted.request.doc_id,
        input,
        status: if admitted.duplicate {
            "duplicate"
        } else {
            "pending"
        },
        duplicate: admitted.duplicate,
    })
}

fn resolve_invocation_key(
    task_id: &str,
    goal_backed: bool,
    stable_key: Option<&str>,
) -> Result<String> {
    match stable_key {
        Some(key) => {
            anyhow::ensure!(!key.trim().is_empty(), "--session-id must not be empty");
            Ok(key.to_owned())
        }
        None if goal_backed => anyhow::bail!(
            "Task {task_id} declares a durable goal; pass --session-id with a stable invocation identity so retries converge"
        ),
        None => Ok(uuid::Uuid::new_v4().to_string()),
    }
}

async fn prepare_task_run_input(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    continue_existing: bool,
    goal_backed: bool,
    admitted: Option<&AgentRequestRow>,
    input: RequestInput,
) -> Result<RequestInput> {
    if let Some(admitted) = admitted {
        anyhow::ensure!(
            admitted.node_did.as_deref() == Some(node_did)
                && admitted.requester_did.as_deref() == Some(node_did)
                && admitted.session_id.as_deref() == Some(session_id),
            "admitted Task input differs from its owner/session"
        );
        return Ok(admitted.input.clone().unwrap_or_default());
    }
    if goal_backed || !continue_existing {
        return Ok(input);
    }
    crate::request_helpers::prepare_cli_goal_input(access, node_did, session_id, input).await
}

/// Duplicate admission preserves the stored fire, so an untagged signed receipt
/// must not infer its original Task Goal identity from the current Goal row.
async fn prepare_task_run_goal_id(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    goal_backed: bool,
    admitted: Option<&AgentRequestRow>,
) -> Result<Option<String>> {
    if let Some(admitted) = admitted {
        return Ok(admitted
            .input
            .as_ref()
            .and_then(|input| input.goal_assignment.as_ref())
            .map(|reference| reference.goal_id.clone()));
    }
    if !goal_backed {
        return Ok(None);
    }
    access
        .transact_readonly("cli.task_run.goal_identity", |txn| {
            Box::pin(async move {
                let current =
                    gents::goal::load_canonical_goal_in_txn(txn, node_did, session_id).await?;
                Ok(Some(gents::goal::task_assignment_goal_id(
                    current.as_ref().map(|goal| goal.goal_id.as_str()),
                    node_did,
                    session_id,
                )))
            })
        })
        .await
}

async fn lookup_request_by_retry_key(
    access: &ConfigAccess,
    node_did: &str,
    retry_key: &str,
) -> Result<Option<AgentRequestRow>> {
    let query = format!(
        r#"query {{
            AgentRequest(filter: {{ node_did: {{ _eq: "{owner}" }}, requester_did: {{ _eq: "{owner}" }}, retry_key: {{ _eq: "{key}" }} }}, limit: 2) {{
                {signed_fields}
            }}
        }}"#,
        key = escape_graphql_string(retry_key),
        owner = escape_graphql_string(node_did),
        signed_fields = gents::SIGNED_REQUEST_FIELDS,
    );
    let response = access.execute(&query).await?;
    if let Some(errors) = response.get("errors").and_then(Value::as_array) {
        if !errors.is_empty() {
            anyhow::bail!("lookup Task fire request failed: {errors:?}");
        }
    }
    let rows = response
        .get("data")
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if rows.len() > 1 {
        anyhow::bail!("Task fire retry_key resolved to multiple AgentRequest rows");
    }
    rows.first()
        .cloned()
        .map(|row| {
            let row: AgentRequestRow = serde_json::from_value(row)
                .context("decoding Task fire canonical AgentRequest row")?;
            gents::verify_request_receipt_signature(&row)?;
            Ok(row)
        })
        .transpose()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request_helpers::goal_input_tests::{authored_input, fixture, reference};
    use gents::NodeIdentity;
    use serde_json::json;

    pub(super) async fn signed_task_receipt(
        identity: &gents::KeyIdentity,
        input: RequestInput,
    ) -> Result<Value> {
        let create = gents::build_signed_request(
            gents::RequestSpec {
                input,
                retry_key: Some("manual-fire".into()),
                ..gents::RequestSpec::new(
                    gents_protocol::request_admission::RequestPurpose::Normal,
                    gents::RequestIdentity {
                        request_id: "manual-task".into(),
                        node_did: identity.did().into(),
                        requester_did: None,
                        agent_id: "engineer".into(),
                        session_id: "chat".into(),
                        content: "Continue the Task".into(),
                        execution_origin: gents::lifecycle::ExecutionOrigin::Interactive,
                        created_at: "2026-10-21T00:00:00Z".into(),
                    },
                    gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
                        identity.did(),
                    ),
                )
            },
            gents::RequestSigner::Identity(identity),
        )
        .await?;
        let mut row = serde_json::to_value(&create)?;
        row["_docID"] = json!("admitted-physical-request");
        row["admission_kind"] = json!("local-self");
        row["admission_signer_did"] = json!(create.admission.signer_did);
        row["admission_signature"] = json!(bs58::encode(create.admission.signature).into_string());
        gents::verify_request_receipt_signature(&serde_json::from_value(row.clone())?)?;
        Ok(row)
    }

    #[tokio::test]
    async fn generated_manual_task_goal_input_preserves_signed_receipts() -> Result<()> {
        let snapshot: Value = gents_lean_contract::load_contract_snapshot()?;
        let cases = snapshot["goal_delivery_input_cases"].as_array().unwrap();
        let mut covered = 0;
        for case in cases
            .iter()
            .filter(|case| case["plugin"] == false && case["owned"] == true)
        {
            covered += 1;
            let (identity, graphql, source, server) = fixture(
                case["existing"].as_bool().unwrap(),
                true,
                reference(&case["observed"]),
                None,
            )
            .await?;
            let access = ConfigAccess::Graphql(graphql);
            let mut input = authored_input();
            input.goal_assignment = reference(&case["captured"]);
            let mut expected = input.clone();
            let admitted = if case["admitted"] == true {
                let mut original = authored_input();
                original.cwd = Some("/admitted-workspace".into());
                original.goal_assignment = reference(&case["admitted_reference"]);
                let row = signed_task_receipt(&identity, original.clone()).await?;
                *source.requests.lock().unwrap() = json!([row]);
                expected = original;
                lookup_request_by_retry_key(&access, identity.did(), "manual-fire").await?
            } else {
                None
            };
            let actual = prepare_task_run_input(
                &access,
                identity.did(),
                "chat",
                case["existing"].as_bool().unwrap(),
                case["captured"].is_object(),
                admitted.as_ref(),
                input,
            )
            .await?;
            server.abort();
            expected.goal_assignment = reference(&case["expected"]["reference"]);
            assert_eq!(actual, expected, "{case}");
            if admitted.is_some() || case["captured"].is_object() || case["existing"] == false {
                assert!(
                    !source
                        .queries
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|q| q.contains("AgentSession") || q.contains("GoalObservation")),
                    "{case}"
                );
            }
        }
        assert_eq!(covered, 7);
        Ok(())
    }

    #[tokio::test]
    async fn manual_task_goal_input_rejects_tampered_admitted_receipts() -> Result<()> {
        let (identity, graphql, source, server) = fixture(true, true, None, None).await?;
        let mut row = signed_task_receipt(&identity, authored_input()).await?;
        row["input"]["cwd"] = json!("/forged-workspace");
        *source.requests.lock().unwrap() = json!([row]);
        let error = lookup_request_by_retry_key(
            &ConfigAccess::Graphql(graphql),
            identity.did(),
            "manual-fire",
        )
        .await
        .unwrap_err();
        server.abort();
        assert!(format!("{error:#}").contains("signature"), "{error:#}");
        Ok(())
    }

    #[test]
    fn goal_task_requires_a_stable_invocation_key() {
        assert!(resolve_invocation_key("task", true, None).is_err());
        assert_eq!(
            resolve_invocation_key("task", true, Some("invocation-1")).unwrap(),
            "invocation-1"
        );
        assert!(resolve_invocation_key("task", false, None).is_ok());
        assert!(resolve_invocation_key("task", false, Some(" ")).is_err());
    }
}

#[cfg(test)]
mod goal_identity_tests;
