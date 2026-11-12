use anyhow::Result;
use gents_protocol::operations::task::TaskOperationError;
use gents_tools::task_run::TaskRunOptions;

use super::{ensure_peer_chat_ready, observe_submitted_request, ClientCore};
use crate::client::mutations::SubmittedRequest;

impl ClientCore {
    pub async fn fire_task_now_for_node(
        &self,
        node_did: &str,
        task_id: &str,
        args: serde_json::Value,
    ) -> Result<SubmittedRequest> {
        let peer_record = self.peer_record_for_chat_write(node_did)?;
        ensure_peer_chat_ready(node_did, peer_record.as_ref())?;
        let access = self.operator_access(node_did)?;
        let fired = async {
            let task =
                gents_automation::operations::task::runnable(&access, node_did, task_id).await?;
            let session_id = task
                .task
                .goal_objective_template
                .is_some()
                .then(|| uuid::Uuid::new_v4().to_string());
            gents_automation::operations::task::fire(
                &access,
                node_did,
                &self.node_identity,
                task,
                &TaskRunOptions {
                    args,
                    session_id,
                    continue_session: None,
                },
            )
            .await
        };
        let output = fired
            .await
            .map_err(|error| self.record_mutation_error("fire task", failure(error)))?;
        let submitted = SubmittedRequest {
            request_id: output.request_id,
            session_id: output.session_id,
            node_did: output.node_did,
            agent_did: Some(output.agent_did),
        };
        let node = submitted.node_did.clone();
        let submitted =
            observe_submitted_request(&self.store, submitted, self.refresh_written_node(&node))
                .await;
        self.clear_mutation_error();
        tracing::info!(
            target: "gents_desktop_core::writes",
            doc_type = "manual_run",
            node_did,
            task_id,
            request_id = %submitted.request_id,
            "desktop manual task run submitted through the task operation"
        );
        Ok(submitted)
    }

    pub async fn fire_schedule_now_for_node(
        &self,
        node_did: &str,
        schedule_id: &str,
    ) -> Result<SubmittedRequest> {
        let access = self.operator_access(node_did)?;
        let task_id =
            gents_automation::operations::task::scheduled_task(&access, node_did, schedule_id)
                .await
                .map_err(|error| self.record_mutation_error("resolve schedule", failure(error)))?;
        self.fire_task_now_for_node(node_did, &task_id, serde_json::json!({}))
            .await
    }
}

/// A refused Task operation as the message the UI shows: what happened, then
/// the next step in desktop terms.
fn failure(error: TaskOperationError) -> anyhow::Error {
    use gents_protocol::operations::NextCall;
    let next = match error.recovery() {
        NextCall::List { resource: "task" } => "Reload the task list and choose a task.",
        NextCall::Inspect { resource: "task" } => "Enable the task, then run it again.",
        NextCall::Inspect { .. } => "Enable the task's agent, then run it again.",
        NextCall::List { resource: "schedule" } => "Reload the schedules and choose one.",
        NextCall::List { .. } => "Give the schedule exactly one enabled trigger, or run its task.",
        NextCall::VerbHelp { .. }
        | NextCall::ResourceHelp { .. }
        | NextCall::Preview { .. }
        | NextCall::Verb { .. } => {
            "Check the task's arguments and templates, then run it again."
        }
    };
    let message = format!(
        "{}. {next}",
        gents_protocol::error_chain::ErrorChain(&error)
            .to_string()
            .trim_end_matches('.')
    );
    anyhow::Error::new(error).context(message)
}
