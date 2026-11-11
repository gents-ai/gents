use std::sync::Arc;

use gents::tools::background::job::dispatch::split_address;
use gents::tools::background::job::view::{
    list_jobs, load_job_view, JobFilter, JobKey, StateFilter,
};
use gents_desktop_core::client::ClientCore;
use gents_protocol::job::{JobReadError, JobState, JobView};

use crate::error::{BridgeError, BridgeErrorCode};

const ALL_STATES: [JobState; 7] = [
    JobState::Running,
    JobState::Paused,
    JobState::AwaitingUser,
    JobState::Completed,
    JobState::Failed,
    JobState::Cancelled,
    JobState::TimedOut,
];

/// Which rows `jobs_list` reads; an absent or empty `states` keeps every state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobsListFilter {
    pub states: Option<Vec<JobState>>,
    pub plugin: Option<String>,
    pub session_id: Option<String>,
}

impl JobsListFilter {
    fn job_filter(self) -> JobFilter {
        let states = self
            .states
            .filter(|states| !states.is_empty())
            .unwrap_or_else(|| ALL_STATES.to_vec());
        JobFilter {
            session_id: self.session_id.filter(|id| !id.is_empty()),
            state: StateFilter::States(states),
            plugin: self.plugin.filter(|plugin| !plugin.is_empty()),
        }
    }
}

/// The node's job rows as its operator reads them, newest first.
pub async fn jobs_list(
    core: &Arc<ClientCore>,
    node_did: &str,
    filter: JobsListFilter,
) -> Result<Vec<JobView>, BridgeError> {
    let access = core
        .operator_access(node_did)
        .map_err(BridgeError::classify_transport_error)?;
    list_jobs(&access, &filter.job_filter())
        .await
        .map_err(read_error)
}

/// One job row by its address (`SESSION_ID:TOOL_CALL_ID`) or, for a resume
/// chain's `resume_of`, by its row doc id.
pub async fn job_view(
    core: &Arc<ClientCore>,
    node_did: &str,
    address: Option<String>,
    row_doc_id: Option<String>,
) -> Result<JobView, BridgeError> {
    let key = job_key(address, row_doc_id)?;
    let access = core
        .operator_access(node_did)
        .map_err(BridgeError::classify_transport_error)?;
    load_job_view(&access, None, key).await.map_err(read_error)
}

fn job_key(address: Option<String>, row_doc_id: Option<String>) -> Result<JobKey, BridgeError> {
    let address = address.filter(|address| !address.is_empty());
    let row_doc_id = row_doc_id.filter(|id| !id.is_empty());
    match (address, row_doc_id) {
        (Some(address), None) => split_address(&address)
            .map(|(session_id, tool_call_id)| JobKey::Address {
                session_id,
                tool_call_id,
            })
            .ok_or_else(|| {
                BridgeError::new(
                    BridgeErrorCode::InvalidArgument,
                    "A run's address is its session id and tool call id, joined by `:`.",
                )
            }),
        (None, Some(row_doc_id)) => Ok(JobKey::RowDocId(row_doc_id)),
        _ => Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "Name the run by exactly one of its address and its row id.",
        )),
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
    fn an_absent_or_empty_state_set_keeps_every_state() {
        for states in [None, Some(Vec::new())] {
            let filter = JobsListFilter {
                states,
                ..JobsListFilter::default()
            }
            .job_filter();
            assert_eq!(filter.state, StateFilter::States(ALL_STATES.to_vec()));
            assert_eq!(filter.session_id, None);
            assert_eq!(filter.plugin, None);
        }
        let filter = JobsListFilter {
            states: Some(vec![JobState::Paused]),
            plugin: Some("fixture/workflow".into()),
            session_id: Some("sess".into()),
        }
        .job_filter();
        assert_eq!(filter.state, StateFilter::States(vec![JobState::Paused]));
        assert_eq!(filter.plugin.as_deref(), Some("fixture/workflow"));
        assert_eq!(filter.session_id.as_deref(), Some("sess"));
    }

    #[test]
    fn a_view_names_its_row_by_address_or_doc_id_alone() {
        assert_eq!(
            job_key(Some("sess:a:call".into()), None).unwrap(),
            JobKey::Address {
                session_id: "sess:a".into(),
                tool_call_id: "call".into(),
            }
        );
        assert_eq!(
            job_key(None, Some("bae-row".into())).unwrap(),
            JobKey::RowDocId("bae-row".into())
        );
        for (address, row) in [
            (None, None),
            (Some("sess:call".to_owned()), Some("bae-row".to_owned())),
            (Some("no-separator".to_owned()), None),
        ] {
            assert_eq!(
                job_key(address, row).unwrap_err().code,
                BridgeErrorCode::InvalidArgument
            );
        }
    }

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
