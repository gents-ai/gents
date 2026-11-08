use anyhow::{ensure, Context, Result};
use gents::documents::apply_desired_state_plan;
use gents::store::config_client::{DesiredStateApplyDocument, DesiredStateApplyPlan};
use gents::store::graphql::escape_graphql_string;
use gents::{Collection, ConfigAccess};
use serde_json::{json, Value};

use super::Attachment;

pub async fn seed_editor_settings(
    access: &ConfigAccess,
    node_did: &str,
    agent_did: &str,
) -> Result<()> {
    let value = json!({"node_did":node_did,"task_id":"editor-settings","agent_did":agent_did,
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
    agent_did: &str,
) -> Result<()> {
    let response = access.execute(&format!(r#"{{ Task(filter: {{node_did: {{_eq: "{}"}}, task_id: {{_eq: "editor-settings"}}}}) {{ display_name prompt_template agent_did }} }}"#,
        escape_graphql_string(node_did))).await?;
    let rows = response["data"]["Task"]
        .as_array()
        .context("saved editor settings")?;
    ensure!(rows.len() == 1 && rows[0]["display_name"] == "Editor saved settings"
        && rows[0]["prompt_template"] == "Editor fixture task" && rows[0]["agent_did"] == agent_did,
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
) -> Result<()> {
    let backends = access
        .execute(&format!(
            r#"{{ InferenceBackend(filter: {{node_did: {{_eq: "{}"}}}}) {{ backend_id }} }}"#,
            escape_graphql_string(node_did)
        ))
        .await?;
    let backend_id = backends["data"]["InferenceBackend"]
        .as_array()
        .and_then(|rows| rows.first())
        .and_then(|row| row["backend_id"].as_str())
        .context("configured inference backend")?
        .to_owned();
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
