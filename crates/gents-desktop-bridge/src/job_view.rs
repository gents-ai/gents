use crate::error::{BridgeError, BridgeErrorCode};
use gents_desktop_core::client::ClientCore;
use gents_protocol::job::{JobReadError, JobView};
use gents_protocol::operations::job::{JobListInput, JobQueryError, JobViewInput};
use std::sync::Arc;

pub async fn jobs_list(
    core: &Arc<ClientCore>,
    input: JobListInput,
) -> Result<Vec<JobView>, BridgeError> {
    let access = core
        .operator_access(&input.node_did)
        .map_err(BridgeError::classify_transport_error)?;
    gents_toolcalls::background::job::view::query::list(&access, input)
        .await
        .map_err(query_error)
}

pub async fn job_view(
    core: &Arc<ClientCore>,
    node_did: &str,
    address: Option<String>,
    row_doc_id: Option<String>,
) -> Result<JobView, BridgeError> {
    let access = core
        .operator_access(node_did)
        .map_err(BridgeError::classify_transport_error)?;
    gents_toolcalls::background::job::view::query::get(
        &access,
        JobViewInput {
            node_did: node_did.to_owned(),
            address,
            row_doc_id,
        },
    )
    .await
    .map_err(query_error)
}

fn query_error(error: JobQueryError) -> BridgeError {
    match error {
        JobQueryError::Read(error) => read_error(error),
        error => BridgeError::new(BridgeErrorCode::InvalidArgument, error.to_string()),
    }
}

fn read_error(error: JobReadError) -> BridgeError {
    let (code, message) = match &error {
        JobReadError::NotAJob => (
            BridgeErrorCode::NotFound,
            "This tool call is not a workflow run.".to_owned(),
        ),
        JobReadError::NotYetObservable => (
            BridgeErrorCode::NotFound,
            "This run is not visible on the node yet.".to_owned(),
        ),
        JobReadError::NotAuthorized => (
            BridgeErrorCode::PermissionDenied,
            "This run belongs to another session or requester.".to_owned(),
        ),
        JobReadError::PermissionDenied => (
            BridgeErrorCode::PermissionDenied,
            "Access was denied. Check this client's permissions with the runtime owner.".to_owned(),
        ),
        JobReadError::Unavailable { message } => (
            BridgeErrorCode::Backend,
            format!("The node did not answer: {message}"),
        ),
        JobReadError::Malformed { message } => (
            BridgeErrorCode::Unknown,
            format!("This run's record does not decode: {message}"),
        ),
    };
    BridgeError::new(code, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn read_errors_keep_their_kind() {
        assert_eq!(
            read_error(JobReadError::NotYetObservable).code,
            BridgeErrorCode::NotFound
        );
        assert_eq!(
            read_error(JobReadError::NotAuthorized).code,
            BridgeErrorCode::PermissionDenied
        );
        let unavailable = read_error(JobReadError::Unavailable {
            message: "timeout".into(),
        });
        assert_eq!(unavailable.code, BridgeErrorCode::Backend);
        assert!(unavailable.retryable);
    }
}
