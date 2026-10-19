use std::sync::Arc;

use gents::background_tools::ProcessControlScope;
use gents::config_client::ConfigAccess;
use gents::plugin_job::control::control;
use gents::plugin_job::view::{load_job_view, JobKey};
use gents_desktop_core::client::ClientCore;
use gents_protocol::job::{JobControlOutcome, JobReadError};

use crate::types::DesktopJobControlRequest;

fn control_error(error: JobReadError) -> String {
    match error {
        JobReadError::NotYetObservable => format!("not found: {error}"),
        JobReadError::NotAJob | JobReadError::NotAuthorized => {
            "workflow_control_unknown_run".into()
        }
        other => other.to_string(),
    }
}

/// Stops or pauses the job row at the request's address as the node's operator.
pub async fn job_control(
    core: &Arc<ClientCore>,
    request: &DesktopJobControlRequest,
) -> Result<JobControlOutcome, String> {
    let access = ConfigAccess::local(core.node_arc());
    let view = load_job_view(
        &access,
        None,
        JobKey::Address {
            session_id: request.session_id.clone(),
            tool_call_id: request.tool_call_id.clone(),
        },
    )
    .await
    .map_err(control_error)?;
    let scope = ProcessControlScope {
        request_id: String::new(),
        session_id: view.scope.session_id,
        node_did: view.scope.node_did,
        requester_did: view.scope.requester_did,
    };
    control(&access, &scope, &request.tool_call_id, request.action)
        .await
        .map_err(control_error)
}
