use std::sync::Arc;

use gents::store::config_client::ConfigAccess;
use gents_desktop_core::client::ClientCore;
use gents_protocol::process_control::{
    OperatorProcessControlRequest, ProcessControlOutcome, PROCESS_CONTROL_PATH,
};

/// Native processes live in the owning runtime, not in the client's replica.
/// The existing operator endpoint supplies the authenticated node identity.
pub async fn process_control(
    core: &Arc<ClientCore>,
    request: &OperatorProcessControlRequest,
) -> Result<ProcessControlOutcome, String> {
    let access = core
        .operator_access(&request.node_did)
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
    let response = endpoint
        .authorize(client.post(url))
        .map_err(|error| error.to_string())?
        .json(request)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let status = response.status();
    let value: serde_json::Value = response
        .json()
        .await
        .map_err(|error| format!("Runtime process control returned HTTP {status}: {error}"))?;
    if let Ok(outcome) = serde_json::from_value::<ProcessControlOutcome>(value.clone()) {
        let consistent = match &outcome {
            ProcessControlOutcome::Cancelled { .. }
            | ProcessControlOutcome::AlreadyTerminal { .. } => status == reqwest::StatusCode::OK,
            ProcessControlOutcome::NotFound => status == reqwest::StatusCode::NOT_FOUND,
            ProcessControlOutcome::NotBackground
            | ProcessControlOutcome::Lost
            | ProcessControlOutcome::Unverified => status == reqwest::StatusCode::CONFLICT,
        };
        if consistent {
            return Ok(outcome);
        }
    }
    if !status.is_success() {
        return Err(value["error"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| {
                format!("Process control was not confirmed (HTTP {status}). Refresh its state.")
            }));
    }
    Err("Runtime process control returned an invalid outcome.".into())
}
