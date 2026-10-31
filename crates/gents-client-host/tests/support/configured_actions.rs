use anyhow::{ensure, Context, Result};
use gents::store::config_client::{
    apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents::store::graphql::escape_graphql_string;
use gents::{Collection, ConfigAccess};
use serde_json::{json, Value};

use super::Attachment;

pub async fn seed_editor_settings(
    access: &ConfigAccess,
    node_did: &str,
    agent_id: &str,
) -> Result<()> {
    let value = json!({"node_did":node_did,"task_id":"editor-settings","agent_id":agent_id,
        "display_name":"Editor settings fixture","prompt_template":"Editor fixture task"});
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Task,
        add: value.clone(),
        update: value,
    }])?;
    access
        .transact_write("test.editor.settings.seed", |txn| {
            let plan = &plan;
            Box::pin(async move {
                apply_desired_state_plan(txn, plan).await?;
                Ok(())
            })
        })
        .await
}

pub async fn verify_editor_settings(
    access: &ConfigAccess,
    node_did: &str,
    agent_id: &str,
) -> Result<()> {
    let response = access.execute(&format!(r#"{{ Task(filter: {{node_did: {{_eq: "{}"}}, task_id: {{_eq: "editor-settings"}}}}) {{ display_name prompt_template agent_id }} }}"#,
        escape_graphql_string(node_did))).await?;
    let rows = response["data"]["Task"]
        .as_array()
        .context("saved editor settings")?;
    ensure!(rows.len() == 1 && rows[0]["display_name"] == "Editor saved settings"
        && rows[0]["prompt_template"] == "Editor fixture task" && rows[0]["agent_id"] == agent_id,
        "the editor must save canonical fields while retaining the configured task owner: {response}");
    Ok(())
}

async fn call(client: &mut Attachment, id: &mut u32, command: &str, args: Value) -> Value {
    let result = client.call(*id, command, args).await;
    *id += 1;
    result
}

pub async fn exercise(
    client: &mut Attachment,
    id: &mut u32,
    access: &ConfigAccess,
    node_did: &str,
    agent_id: &str,
    client_did: &str,
) -> Result<()> {
    let plan = DesiredStateApplyPlan::new(vec![
        (Collection::Task, json!({"node_did":node_did,"task_id":"editor-task","agent_id":agent_id,
            "prompt_template":"Editor sequence {{ args.sequence }}","display_name":"Editor task"})),
        (Collection::Task, json!({"node_did":node_did,"task_id":"editor-disabled","agent_id":agent_id,
            "prompt_template":"Disabled","enabled":false})),
        (Collection::Task, json!({"node_did":node_did,"task_id":"editor-scheduled-task","agent_id":agent_id,
            "prompt_template":"Editor scheduled task"})),
        (Collection::Schedule, json!({"node_did":node_did,"schedule_id":"editor-schedule",
            "cadence":{"kind":"cron","expression":"0 0 1 1 *","timezone":"UTC"}})),
        (Collection::Trigger, json!({"node_did":node_did,"trigger_id":"editor-trigger","task_id":"editor-scheduled-task",
            "source":{"kind":"schedule","schedule_id":"editor-schedule"}})),
    ].into_iter().map(|(collection, value)| DesiredStateApplyDocument { collection, add: value.clone(), update: value }).collect())?;
    access
        .transact_write("test.editor.actions.seed", |txn| {
            let plan = &plan;
            Box::pin(async move {
                apply_desired_state_plan(txn, plan).await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await?;
    let catalog = call(
        client,
        id,
        "client_action_catalog",
        json!({"nodeDid":node_did}),
    )
    .await;
    let actions = catalog.as_array().context("configured action catalog")?;
    ensure!(actions
        .iter()
        .any(|action| action["kind"] == "task" && action["id"] == "editor-task"));
    ensure!(!actions
        .iter()
        .any(|action| action["id"] == "editor-disabled"));
    ensure!(actions
        .iter()
        .any(|action| action["kind"] == "schedule" && action["id"] == "editor-schedule"));
    for (command, args, expected) in [
        (
            "client_start_task",
            json!({"nodeDid":node_did,"taskId":"editor-task","argumentsJson":"{\"sequence\":9223372036854775807}"}),
            "Editor sequence 9223372036854775807",
        ),
        (
            "client_run_schedule",
            json!({"nodeDid":node_did,"scheduleId":"editor-schedule"}),
            "Editor scheduled task",
        ),
    ] {
        let receipt = call(client, id, command, args).await;
        ensure!(receipt["nodeDid"] == node_did && receipt["agentId"] == agent_id);
        let request_id = receipt["requestId"]
            .as_str()
            .context("accepted action request")?;
        let session_id = receipt["sessionId"]
            .as_str()
            .context("accepted action session")?;
        let query = format!(
            "{{ AgentRequest(filter: {{ request_id: {{_eq: \"{}\"}} }}) {{ {} }} }}",
            escape_graphql_string(request_id),
            gents::SIGNED_REQUEST_FIELDS
        );
        let response = access.execute(&query).await?;
        let rows = response["data"]["AgentRequest"]
            .as_array()
            .context("action request rows")?;
        ensure!(rows.len() == 1, "one request per configured action");
        let request: gents_protocol::row::AgentRequestRow =
            serde_json::from_value(rows[0].clone())?;
        gents::verify_request_receipt_signature(&request)?;
        ensure!(request.content.as_deref() == Some(expected));
        ensure!(request.session_id.as_deref() == Some(session_id));
        ensure!(request.admission_signer_did.as_deref() == Some(client_did));
    }
    let catalog = call(
        client,
        id,
        "client_config_catalog",
        json!({"nodeDid":node_did}),
    )
    .await;
    let backend = catalog
        .as_array()
        .and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry["collection"] == "InferenceBackend")
        })
        .context("native backend settings catalog")?;
    let backend_id = backend["id"].as_str().context("backend settings ID")?;
    let original = call(
        client,
        id,
        "client_config_read",
        json!({"nodeDid":node_did,"collection":"InferenceBackend","id":backend_id}),
    )
    .await;
    let mut content: Value = serde_json::from_str(
        original["content"]
            .as_str()
            .context("editable configuration")?,
    )?;
    ensure!(content.get("auth").is_none() && content.get("node_did").is_none());
    content["name"] = json!("Editor configured backend");
    let save_args = json!({"nodeDid":node_did,"collection":"InferenceBackend","id":backend_id,
        "revision":original["revision"],"content":serde_json::to_string(&content)?});
    let saved = call(client, id, "client_config_save", save_args.clone()).await;
    ensure!(saved["revision"]["documentId"] == original["revision"]["documentId"]);
    ensure!(saved["revision"]["compositeCid"] != original["revision"]["compositeCid"]);
    let stale = client.try_call(*id, "client_config_save", save_args).await;
    *id += 1;
    ensure!(
        stale.is_err(),
        "a stale editor must not overwrite newer configuration"
    );
    let auth_args = json!({"request":{"nodeDid":node_did,"backendId":backend_id}});
    let original_auth = call(
        client,
        id,
        "client_backend_authentication",
        auth_args.clone(),
    )
    .await;
    let save_auth = json!({"request":{"nodeDid":node_did,"backendId":backend_id,
        "revision":original_auth["revision"],"auth":{"kind":"api_key","key":"private-native-authentication-fixture"}}});
    call(
        client,
        id,
        "client_backend_authentication_save",
        save_auth.clone(),
    )
    .await;
    let observed_auth = call(client, id, "client_backend_authentication", auth_args).await;
    ensure!(observed_auth["authKind"] == "api_key");
    ensure!(!observed_auth
        .to_string()
        .contains("private-native-authentication-fixture"));
    let public = call(
        client,
        id,
        "client_config_read",
        json!({"nodeDid":node_did,"collection":"InferenceBackend","id":backend_id}),
    )
    .await;
    ensure!(!public
        .to_string()
        .contains("private-native-authentication-fixture"));
    let conflict = client
        .try_call(*id, "client_backend_authentication_save", save_auth)
        .await;
    *id += 1;
    ensure!(
        conflict.is_err(),
        "authentication changes must reject stale editor revisions"
    );
    Ok(())
}
