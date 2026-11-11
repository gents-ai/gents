use std::path::Path;

use anyhow::{anyhow, Context, Result};
pub(crate) use gents::request::task_run::TaskRunOutput;
use gents::request::task_run::{
    enqueue_task_run as enqueue_task_run_owner, read_runnable_task, TaskRunOptions,
};
#[cfg(test)]
use gents::request::task_run::{
    lookup_request_by_retry_key, prepare_task_run_goal_id, prepare_task_run_input,
    resolve_invocation_key,
};
#[cfg(test)]
use gents::store::graphql::escape_graphql_string;
#[cfg(test)]
use gents_protocol::request_input::RequestInput;
#[cfg(test)]
use gents_protocol::row::AgentRequestRow;
use serde_json::Value;

use crate::cli::ConfigTaskRunArgs;
use crate::config_writes::ConfigAccess;
use crate::request_helpers::wait_for_terminal_response;
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

pub(crate) async fn enqueue_task_run(args: &ConfigTaskRunArgs) -> Result<TaskRunOutput> {
    let task_id =
        resolve_task_id_for("run", args.task_id.as_deref(), args.task_id_flag.as_deref())?;

    let args_value: Value =
        serde_json::from_str(&args.args).map_err(|e| anyhow!("--args is not valid JSON: {e}"))?;
    if !args_value.is_object() {
        anyhow::bail!("--args must be a JSON object (got: {args_value})");
    }

    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    enqueue_task_run_on(
        &access,
        args.home.as_deref(),
        &task_id,
        args_value,
        args.session_id.as_deref(),
        args.continue_session.as_deref(),
    )
    .await
}

pub(crate) async fn enqueue_task_run_on(
    access: &ConfigAccess,
    home: Option<&Path>,
    task_id: &str,
    args: Value,
    session_id: Option<&str>,
    continue_session: Option<&str>,
) -> Result<TaskRunOutput> {
    let node_did = crate::resolve_node_did(home, None)?;
    let actor_did = access
        .actor_did()
        .context("Task invocation requires an authenticated client identity")?;
    let signer = gents::store::identity::RegisteredIdentity::from_registered_did(&actor_did, None)?;
    let task = read_runnable_task(access, &node_did, task_id).await?;
    enqueue_task_run_owner(
        access,
        &node_did,
        &signer,
        task,
        &TaskRunOptions {
            args,
            session_id: session_id.map(str::to_owned),
            continue_session: continue_session.map(str::to_owned),
        },
    )
    .await
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
                "conflicting task ids provided: positional={} and --task-id={}\nNext:\n  1. Pass the task id once: `gents automation task {command} TASK_ID`\n  2. Or use `--task-id TASK_ID`, but not both",
                positional,
                flag
            );
        }
        (Some(task_id), _) | (_, Some(task_id)) => Ok(task_id.to_string()),
        (None, None) => anyhow::bail!(
            "missing task id\nNext:\n  1. Pass it positionally: `gents automation task {command} TASK_ID`\n  2. Or use `--task-id TASK_ID`"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request_helpers::goal_input_tests::{authored_input, fixture, reference};
    use gents::NodeIdentity;
    use serde_json::json;
    use std::sync::Arc;

    /// A signed client receipt as a Task invocation's request row decodes it;
    /// replay reads only its signed fields.
    pub(super) async fn signed_task_receipt(
        source: &crate::request_helpers::goal_input_tests::Source,
        input: RequestInput,
    ) -> Result<Value> {
        let user = source.user();
        let create = gents::request::lifecycle::build_signed_client_request(
            &source.home.access,
            gents::RequestSpec {
                input,
                retry_key: Some("manual-fire".into()),
                ..gents::RequestSpec::new(
                    gents_protocol::request_admission::RequestPurpose::Normal,
                    gents::RequestIdentity {
                        request_id: "manual-task".into(),
                        node_did: source.node_did.clone(),
                        requester_did: Some(source.agent_did.clone()),
                        agent_did: source.agent_did.clone(),
                        session_id: "chat".into(),
                        content: "Continue the Task".into(),
                        execution_origin: gents::request::lifecycle::ExecutionOrigin::Interactive,
                        created_at: "2026-10-21T00:00:00Z".into(),
                    },
                    gents_protocol::request_admission::AgentRequestAdmissionRecord::client(
                        user.did(),
                    ),
                )
            },
            gents::RequestSigner::Identity(user),
        )
        .await?;
        let mut row = serde_json::to_value(&create)?;
        row["_docID"] = json!("admitted-physical-request");
        row["admission_kind"] = json!("client");
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
            let source = fixture(
                case["existing"].as_bool().unwrap(),
                true,
                reference(&case["observed"]),
                None,
            )
            .await?;
            let access = ConfigAccess::Graphql(source.graphql.clone());
            let mut input = authored_input();
            input.goal_assignment = reference(&case["captured"]);
            let mut expected = input.clone();
            let admitted = if case["admitted"] == true {
                let mut original = authored_input();
                original.cwd = Some("/admitted-workspace".into());
                original.goal_assignment = reference(&case["admitted_reference"]);
                source.admit(original.clone()).await?;
                expected = original;
                lookup_request_by_retry_key(&access, &source.node_did, "manual-fire").await?
            } else {
                None
            };
            let before = source.exchanges();
            let actual = prepare_task_run_input(
                &access,
                &gents::store::identity::RegisteredIdentity::from_registered_did(
                    access.actor_did().context("test actor")?,
                    None,
                )?,
                &source.node_did,
                "chat",
                case["existing"].as_bool().unwrap(),
                case["captured"].is_object(),
                admitted.as_ref(),
                input,
            )
            .await?;
            expected.goal_assignment = reference(&case["expected"]["reference"]);
            assert_eq!(actual, expected, "{case}");
            if admitted.is_some() || case["captured"].is_object() || case["existing"] == false {
                assert_eq!(source.exchanges(), before, "{case}");
            }
        }
        assert_eq!(covered, 7);
        Ok(())
    }

    #[tokio::test]
    async fn manual_task_goal_input_rejects_tampered_admitted_receipts() -> Result<()> {
        use crate::shared::test_support::{ForwardingProxy, ProxyDecision};
        let source = fixture(true, true, None, None).await?;
        source.admit(authored_input()).await?;
        let access = ConfigAccess::Graphql(source.graphql.clone());
        let mut row = serde_json::to_value(
            lookup_request_by_retry_key(&access, &source.node_did, "manual-fire")
                .await?
                .context("admitted Task request")?,
        )?;
        // The durable row is genuine; a reader that alters its signed input in
        // transit must not be believed.
        row["input"]["cwd"] = json!("/forged-workspace");
        let forged = json!({"data": {"AgentRequest": [row]}});
        let hook = Arc::new(move |_path: String, body: axum::body::Bytes| {
            let forged = forged.clone();
            Box::pin(async move {
                if String::from_utf8_lossy(&body).contains("retry_key") {
                    ProxyDecision::ForwardThenRespond(axum::response::IntoResponse::into_response(
                        axum::Json(forged),
                    ))
                } else {
                    ProxyDecision::Forward
                }
            }) as futures_util::future::BoxFuture<'static, ProxyDecision>
        });
        let proxy = ForwardingProxy::spawn(source.home.graphql(), hook).await;
        let tampering = ConfigAccess::Graphql(crate::shared::test_support::user_endpoint(
            &proxy.graphql,
            source.user(),
        ));
        let error = lookup_request_by_retry_key(&tampering, &source.node_did, "manual-fire")
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("signature"), "{error:#}");
        Ok(())
    }

    /// The shell runs a task through its own access: with no runtime recorded in the home,
    /// `resolve_config_access` would open the home's store instead.
    #[tokio::test]
    async fn run_now_uses_the_access_it_is_given() -> Result<()> {
        let home = tempfile::tempdir()?;
        let did = crate::caused_sessions::fixtures::FIXTURE_DID.to_string();
        crate::home_state::write_init_config(
            home.path(),
            &crate::shared::StoredInitConfig {
                home: home.path().to_string_lossy().into_owned(),
                node_name: "fixture".into(),
                node_did: did.clone(),
                key_path: None,
                identity_backend: None,
                keychain_label: None,
                secure_enclave_label: None,
                tool_package: None,
                tool_ceiling: crate::cli::args::ToolCeilingArg::MetaOnly,
                tool_root: None,
                store_encryption: None,
            },
        )?;
        let served = crate::shared::test_support::served_config(json!({
            "node": { "node_did": did, "default_agent_did": "engineer" },
            "inference_backends": [{
                "node_did": did,
                "backend_id": "backend",
                "name": "Backend",
                "provider_kind": "OpenAiCompatible",
                "endpoint": "http://127.0.0.1:1/v1",
                "auth": { "kind": "unauthenticated" },
            }],
            "inference_profiles": [{
                "node_did": did,
                "profile_id": "profile",
                "backend_id": "backend",
                "model_name": "model",
            }],
            "agents": [{ "node_did": did, "agent_did": "engineer", "inference_profile_id": "profile" }],
            "tasks": [{
                "node_did": did,
                "task_id": "digest",
                "agent_did": "engineer",
                "prompt_template": "digest the day",
            }],
        }))
        .await;

        let output = enqueue_task_run_on(
            &ConfigAccess::Graphql(served.user.clone()),
            Some(home.path()),
            "digest",
            json!({}),
            None,
            None,
        )
        .await?;

        let written = ConfigAccess::Graphql(served.user.clone())
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
                escape_graphql_string(&output.request_id)
            ))
            .await?;
        assert_eq!(
            written["data"]["AgentRequest"],
            json!([{ "_docID": output.request_doc_id }])
        );
        assert_eq!(output.status, "pending");
        assert!(
            !crate::home_state::default_data_dir(home.path()).exists(),
            "the home's store is never opened"
        );
        served.node.shutdown().await.expect("node shutdown");
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
