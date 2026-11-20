use anyhow::Result;
use gents_documents::apply_desired_state_plan;
use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
use gents_store::{collection::Collection, config_client::ConfigAccess};
use serde_json::json;

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
                apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await?;
                Ok(())
            })
        })
        .await
}
