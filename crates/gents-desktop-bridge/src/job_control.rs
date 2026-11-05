use std::sync::Arc;

use gents::store::config_client::ConfigAccess;
use gents_desktop_core::client::ClientCore;
use gents_protocol::job::{JobControlOutcome, JOB_CONTROL_PATH};

use crate::types::DesktopJobControlRequest;

/// Sends the authenticated client's control intent to the owning runtime.
pub async fn job_control(
    core: &Arc<ClientCore>,
    request: &DesktopJobControlRequest,
) -> Result<JobControlOutcome, String> {
    let changes = core.store_change_updates();
    let access = core
        .operator_access(&request.node_did)
        .map_err(|error| error.to_string())?;
    let action = match request.action {
        gents_protocol::job::JobControlKind::Stop => "job_stop",
        gents_protocol::job::JobControlKind::Pause => "job_pause",
    };
    let command = gents::tools::call::control::client::prepare(
        &access,
        core.node_identity(),
        &request.node_did,
        &request.tool_call_doc_id,
        None,
        action,
    )
    .await
    .map_err(|error| error.to_string())?;
    match &access {
        ConfigAccess::Local(_) => Err("Connect to the owning runtime to control this job.".into()),
        ConfigAccess::Graphql(endpoint) => {
            let mut url = reqwest::Url::parse(endpoint.url()).map_err(|error| error.to_string())?;
            if !matches!(url.scheme(), "http" | "https")
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err("runtime endpoint must be an HTTP URL without credentials".into());
            }
            let base = url
                .path()
                .strip_suffix("/api/v0/graphql")
                .ok_or("runtime endpoint has no GraphQL path")?;
            let path = format!("{base}{JOB_CONTROL_PATH}");
            url.set_path(&path);
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .map_err(|error| error.to_string())?;
            let request_builder = endpoint
                .authorize(client.post(url))
                .map_err(|error| error.to_string())?
                .json(&command);
            let response = crate::tool_control::send(request_builder, &command.intent_id).await?;
            let status = response.status();
            let value: serde_json::Value = response.json().await.map_err(|error| {
                crate::tool_control::unconfirmed_submission(
                    &command.intent_id,
                    &format!("Runtime returned HTTP {status}: {error}"),
                )
            })?;
            if !status.is_success() {
                let detail = value["error"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("Runtime job control failed (HTTP {status})"));
                return Err(
                    if status.is_server_error() || status == reqwest::StatusCode::REQUEST_TIMEOUT {
                        crate::tool_control::unconfirmed_submission(&command.intent_id, &detail)
                    } else {
                        detail
                    },
                );
            }
            let outcome: JobControlOutcome = serde_json::from_value(value).map_err(|error| {
                crate::tool_control::unconfirmed_submission(&command.intent_id, &error.to_string())
            })?;
            if let JobControlOutcome::Queued { intent_doc_id } = &outcome {
                let action = match request.action {
                    gents_protocol::job::JobControlKind::Stop => "job_stop",
                    gents_protocol::job::JobControlKind::Pause => "job_pause",
                };
                return match crate::tool_control::await_receipt(
                    core,
                    &request.node_did,
                    intent_doc_id,
                    &request.tool_call_doc_id,
                    action,
                    changes,
                )
                .await?
                {
                    gents_protocol::process_control::ToolControlReceiptOutcome::Job(outcome) => {
                        Ok(outcome)
                    }
                    gents_protocol::process_control::ToolControlReceiptOutcome::Rejected => {
                        Err("The control request was denied or the job was unavailable.".into())
                    }
                    gents_protocol::process_control::ToolControlReceiptOutcome::Unconfirmed => Err(format!("Control {intent_doc_id} could not be confirmed. Refresh the job state; the action may have taken effect.")),
                    _ => Err("The control receipt has an unexpected outcome.".into()),
                };
            }
            Err(crate::tool_control::unconfirmed_submission(
                &command.intent_id,
                "The runtime returned an unsigned outcome.",
            ))
        }
    }
}
