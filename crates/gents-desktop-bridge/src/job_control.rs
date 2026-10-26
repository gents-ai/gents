use std::sync::Arc;

use gents::config_client::ConfigAccess;
use gents::plugin_job::control::operator_control;
use gents_desktop_core::client::ClientCore;
use gents_protocol::job::{
    JobControlOutcome, JobReadError, OperatorJobControlRequest, JOB_CONTROL_PATH,
};

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
    let command = OperatorJobControlRequest {
        node_did: request.node_did.clone(),
        tool_call_doc_id: request.tool_call_doc_id.clone(),
        action: request.action,
    };
    let access = core
        .operator_access(&request.node_did)
        .map_err(|error| error.to_string())?;
    match &access {
        ConfigAccess::Local(_) => operator_control(
            &access,
            access
                .local_node()
                .and_then(|node| node.node_identity_did()),
            &command,
        )
        .await
        .map_err(control_error),
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
            let response = endpoint
                .authorize(client.post(url))
                .map_err(|error| error.to_string())?
                .json(&command)
                .send()
                .await
                .map_err(|error| error.to_string())?;
            let status = response.status();
            let value: serde_json::Value = response
                .json()
                .await
                .map_err(|error| format!("Runtime job control returned HTTP {status}: {error}"))?;
            if !status.is_success() {
                return Err(value["error"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("Runtime job control failed (HTTP {status})")));
            }
            serde_json::from_value(value).map_err(|error| error.to_string())
        }
    }
}
