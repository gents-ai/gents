use gents_protocol::job::{JobState, JobView};
use gents_protocol::operations::job::{JobListInput, JobQueryError, JobViewInput};
use gents_store::config_client::ConfigAccess;

use super::{JobFilter, JobKey, StateFilter};
use crate::background::job::dispatch::split_address;

const ALL_STATES: [JobState; 7] = [
    JobState::Running,
    JobState::Paused,
    JobState::AwaitingUser,
    JobState::Completed,
    JobState::Failed,
    JobState::Cancelled,
    JobState::TimedOut,
];

fn job_filter(input: JobListInput) -> JobFilter {
    JobFilter {
        session_id: input.session_id.filter(|id| !id.is_empty()),
        state: StateFilter::States(
            input
                .states
                .filter(|states| !states.is_empty())
                .unwrap_or_else(|| ALL_STATES.to_vec()),
        ),
        plugin: input.plugin.filter(|plugin| !plugin.is_empty()),
    }
}

fn job_key(input: JobViewInput) -> Result<JobKey, JobQueryError> {
    match (
        input.address.filter(|address| !address.is_empty()),
        input.row_doc_id.filter(|id| !id.is_empty()),
    ) {
        (Some(address), None) => split_address(&address)
            .map(|(session_id, tool_call_id)| JobKey::Address {
                session_id,
                tool_call_id,
            })
            .ok_or(JobQueryError::InvalidAddress),
        (None, Some(id)) => Ok(JobKey::RowDocId(id)),
        _ => Err(JobQueryError::InvalidSelector),
    }
}

pub async fn list(
    access: &ConfigAccess,
    input: JobListInput,
) -> Result<Vec<JobView>, JobQueryError> {
    let node = input.node_did.clone();
    Ok(super::list_scoped_jobs(access, &job_filter(input), Some(node)).await?)
}

pub async fn get(access: &ConfigAccess, input: JobViewInput) -> Result<JobView, JobQueryError> {
    let node = input.node_did.clone();
    Ok(super::load_scoped_job_view(access, None, job_key(input)?, Some(node)).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn cases() -> Value {
        let output = gents_lean_contract::run_prepared_generator(
            &gents_lean_contract::proofs_dir().unwrap(),
            "Proofs.Conformance.JobRead:olean",
            "Proofs/Conformance/JobRead.lean",
        )
        .unwrap();
        serde_json::from_str(&output).unwrap()
    }

    #[test]
    fn an_absent_or_empty_state_set_keeps_every_state() {
        let cases = cases();
        let rows = cases["filters"].as_array().unwrap();
        assert!(!rows.is_empty());
        for row in rows {
            let states = serde_json::from_value(row["states"].clone()).unwrap();
            let filter = job_filter(JobListInput {
                states,
                ..Default::default()
            });
            let StateFilter::States(actual) = filter.state;
            assert_eq!(
                serde_json::to_value(actual).unwrap(),
                row["expected"],
                "{row}"
            );
        }
    }

    #[test]
    fn a_view_names_its_row_by_address_or_doc_id_alone() {
        let cases = cases();
        let rows = cases["selectors"].as_array().unwrap();
        assert!(!rows.is_empty());
        for row in rows {
            let input = JobViewInput {
                address: serde_json::from_value(row["address"].clone()).unwrap(),
                row_doc_id: serde_json::from_value(row["row_doc_id"].clone()).unwrap(),
                ..Default::default()
            };
            let actual = match job_key(input) {
                Ok(JobKey::Address {
                    session_id,
                    tool_call_id,
                }) => {
                    json!({"kind": "address", "session_id": session_id, "tool_call_id": tool_call_id})
                }
                Ok(JobKey::RowDocId(row_doc_id)) => {
                    json!({"kind": "row", "row_doc_id": row_doc_id})
                }
                Err(JobQueryError::InvalidAddress) => json!({"kind": "invalid_address"}),
                Err(JobQueryError::InvalidSelector) => json!({"kind": "invalid_selector"}),
                Err(error) => panic!("{error}"),
            };
            assert_eq!(actual, row["expected"], "{row}");
        }
    }
}
