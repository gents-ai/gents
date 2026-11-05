use std::sync::Arc;

use gents::store::config_client::ConfigAccess;
use gents_desktop_core::client::ClientCore;
use gents_protocol::process_control::{
    OperatorProcessControlRequest, ProcessControlOutcome, PROCESS_CONTROL_PATH,
};

/// Native processes live in the owning runtime, not in the client's replica.
/// The endpoint authenticates the client's identity.
pub async fn process_control(
    core: &Arc<ClientCore>,
    request: &OperatorProcessControlRequest,
) -> Result<ProcessControlOutcome, String> {
    let changes = core.store_change_updates();
    let access = core
        .operator_access(&request.node_did)
        .map_err(|error| error.to_string())?;
    let intent = gents::tools::call::control::client::prepare(
        &access,
        core.node_identity(),
        &request.node_did,
        &request.tool_call_doc_id,
        Some((&request.session_id, &request.tool_call_id)),
        "process_stop",
    )
    .await
    .map_err(|error| error.to_string())?;
    let ConfigAccess::Graphql(endpoint) = access else {
        return Err("Connect to the owning runtime to stop this process.".into());
    };
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
    let path = format!("{base}{PROCESS_CONTROL_PATH}");
    url.set_path(&path);
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|error| error.to_string())?;
    let request_builder = endpoint
        .authorize(client.post(url))
        .map_err(|error| error.to_string())?
        .json(&intent);
    let response = crate::tool_control::send(request_builder, &intent.intent_id).await?;
    let status = response.status();
    let value: serde_json::Value = response.json().await.map_err(|error| {
        crate::tool_control::unconfirmed_submission(
            &intent.intent_id,
            &format!("Runtime returned HTTP {status}: {error}"),
        )
    })?;
    if let Ok(ProcessControlOutcome::Queued { intent_doc_id }) =
        serde_json::from_value::<ProcessControlOutcome>(value.clone())
    {
        if status == reqwest::StatusCode::ACCEPTED {
            return match crate::tool_control::await_receipt(
                    core,
                    &request.node_did,
                    &intent_doc_id,
                    &request.tool_call_doc_id,
                    "process_stop",
                    changes,
                )
                .await?
                {
                    gents_protocol::process_control::ToolControlReceiptOutcome::Process(
                        outcome,
                    ) => Ok(outcome),
                    gents_protocol::process_control::ToolControlReceiptOutcome::Rejected => {
                        Err("The control request was denied or the process was unavailable.".into())
                    }
                    gents_protocol::process_control::ToolControlReceiptOutcome::Unconfirmed => Err(format!("Control {intent_doc_id} could not be confirmed. Refresh the process state; the action may have taken effect.")),
                    _ => Err("The control receipt has an unexpected outcome.".into()),
                };
        }
    }
    if !status.is_success() {
        let detail = value["error"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| {
                format!("Process control was not confirmed (HTTP {status}). Refresh its state.")
            });
        return Err(
            if status.is_server_error() || status == reqwest::StatusCode::REQUEST_TIMEOUT {
                crate::tool_control::unconfirmed_submission(&intent.intent_id, &detail)
            } else {
                detail
            },
        );
    }
    Err(crate::tool_control::unconfirmed_submission(
        &intent.intent_id,
        "The runtime returned an unsigned or invalid outcome.",
    ))
}
