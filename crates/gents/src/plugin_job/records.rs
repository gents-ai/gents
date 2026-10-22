//! The two job records on an `AgentToolCall` row: `job_pin` (written once)
//! and `job_progress` (last writer wins under a revision compare-and-set).

use anyhow::{Context, Result};
use gents_protocol::job::{
    JobBudget, JobPauseReason, JobPin, JobProgress, JobResume, JobState, JOB_GUEST_STATE_MAX_BYTES,
    JOB_PROGRESS_MAX_BYTES, JOB_PROGRESS_TEXT_MAX_BYTES,
};
use gents_protocol::plugin::PluginExecutionReceipt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config_client::{ConfigApplyTxn, ConfigRead};
use crate::graphql::escape_graphql_string;

/// A record refusal a caller can tell from a storage failure.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RecordError {
    #[error("job_pin of {0} is already written and differs")]
    PinConflict(String),
    #[error("job_step_state.resume of {0} is already written and differs")]
    ResumeConflict(String),
    #[error("job_progress of {row} is at revision {stored}, not {expected}")]
    StaleRevision {
        row: String,
        stored: u64,
        expected: u64,
    },
    #[error("job_progress is {bytes} bytes, over the {max} byte cap")]
    TooLarge { bytes: usize, max: usize },
    #[error("the guest state is {bytes} bytes, over the {max} byte cap")]
    GuestStateTooLarge { bytes: usize, max: usize },
}

/// Progress fields the host owns; the guest's values for them are overwritten.
/// `revision` is not here: the compare-and-set assigns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostProgressFacts {
    pub spawned: u32,
    pub budget: JobBudget,
    pub observed_terminal_children: u32,
    pub state: JobState,
    pub pause_reason: Option<JobPauseReason>,
}

/// What the host keeps on a job row between steps (`AgentToolCall.job_step_state`):
/// the evidence of the last step that ran, from which the terminal
/// compare-and-set publishes the row's single execution receipt, and the
/// validated resume block the row was adopted with, which every step echoes,
/// and the guest's own state from its last step that carried one, stored
/// verbatim and never read by the host.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobStepState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<PluginExecutionReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<JobResume>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest: Option<Value>,
}

async fn read_record<R: ConfigRead>(
    reader: &R,
    row_doc_id: &str,
    field: &str,
) -> Result<Option<Value>> {
    let query = format!(
        r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ {field} }} }}"#,
        escape_graphql_string(row_doc_id)
    );
    let response = reader.execute_read(&query).await?;
    let rows = response
        .pointer("/data/AgentToolCall")
        .and_then(Value::as_array)
        .with_context(|| format!("AgentToolCall query omitted rows for {row_doc_id}"))?;
    let row = rows
        .first()
        .with_context(|| format!("job row {row_doc_id} does not exist"))?;
    Ok(row.get(field).filter(|value| !value.is_null()).cloned())
}

async fn write_record(
    txn: &ConfigApplyTxn<'_>,
    row_doc_id: &str,
    field: &str,
    value: Value,
) -> Result<()> {
    let mutation = format!(
        r#"mutation($record: JSON) {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ {field}: $record }}) {{ _docID }} }}"#,
        escape_graphql_string(row_doc_id)
    );
    txn.execute_with_variables(&mutation, &json!({ "record": value }))
        .await?;
    Ok(())
}

fn decode<T: serde::de::DeserializeOwned>(value: Option<Value>, what: &str) -> Result<Option<T>> {
    value
        .map(|value| serde_json::from_value(value).with_context(|| format!("decoding {what}")))
        .transpose()
}

/// Writes `pin` once. An identical replay is accepted; a different pin is
/// [`RecordError::PinConflict`].
pub async fn write_pin_in_txn(
    txn: &ConfigApplyTxn<'_>,
    row_doc_id: &str,
    pin: &JobPin,
) -> Result<()> {
    let stored: Option<JobPin> = decode(read_record(txn, row_doc_id, "job_pin").await?, "job_pin")?;
    match stored {
        Some(stored) if stored == *pin => Ok(()),
        Some(_) => Err(RecordError::PinConflict(row_doc_id.to_owned()).into()),
        None => write_record(txn, row_doc_id, "job_pin", serde_json::to_value(pin)?).await,
    }
}

pub async fn read_pin<R: ConfigRead>(reader: &R, row_doc_id: &str) -> Result<Option<JobPin>> {
    decode(read_record(reader, row_doc_id, "job_pin").await?, "job_pin")
}

/// Stores `evidence` as the evidence of the job step that just ran on
/// `row_doc_id`, replacing the previous step's. `resume` is written once:
/// `None` keeps the stored block, and a block that differs from a stored one
/// is [`RecordError::ResumeConflict`]. `guest` replaces the stored guest
/// state when given (within [`JOB_GUEST_STATE_MAX_BYTES`], else
/// [`RecordError::GuestStateTooLarge`]) and `None` keeps it. Only the step and
/// adoption transactions call it; the row's `plugin_execution_receipt` is
/// never written here.
pub async fn write_step_state_in_txn(
    txn: &ConfigApplyTxn<'_>,
    row_doc_id: &str,
    evidence: &PluginExecutionReceipt,
    resume: Option<&JobResume>,
    guest: Option<&Value>,
) -> Result<()> {
    let stored = read_step_state(txn, row_doc_id).await?;
    let stored_guest = stored.as_ref().and_then(|state| state.guest.clone());
    let stored_resume = stored.and_then(|state| state.resume);
    let guest = match guest {
        Some(guest) => {
            let bytes = guest.to_string().len();
            if bytes > JOB_GUEST_STATE_MAX_BYTES {
                return Err(RecordError::GuestStateTooLarge {
                    bytes,
                    max: JOB_GUEST_STATE_MAX_BYTES,
                }
                .into());
            }
            Some(guest.clone())
        }
        None => stored_guest,
    };
    let resume = match (stored_resume, resume) {
        (Some(stored), Some(new)) if stored != *new => {
            return Err(RecordError::ResumeConflict(row_doc_id.to_owned()).into());
        }
        (Some(stored), _) => Some(stored),
        (None, new) => new.cloned(),
    };
    let state = JobStepState {
        evidence: Some(evidence.clone()),
        resume,
        guest,
    };
    write_record(
        txn,
        row_doc_id,
        "job_step_state",
        serde_json::to_value(state)?,
    )
    .await
}

pub async fn read_step_state<R: ConfigRead>(
    reader: &R,
    row_doc_id: &str,
) -> Result<Option<JobStepState>> {
    decode(
        read_record(reader, row_doc_id, "job_step_state").await?,
        "job_step_state",
    )
}

/// Merges `guest` progress with `host` facts and writes it at
/// `expected_revision + 1`. The stored revision (0 when none) must equal
/// `expected_revision`, otherwise [`RecordError::StaleRevision`]. Titles and
/// events are cut to 256 bytes; a record over 16 KiB is [`RecordError::TooLarge`].
pub async fn write_progress_in_txn(
    txn: &ConfigApplyTxn<'_>,
    row_doc_id: &str,
    expected_revision: u64,
    guest: JobProgress,
    host: HostProgressFacts,
) -> Result<JobProgress> {
    let stored: Option<JobProgress> = decode(
        read_record(txn, row_doc_id, "job_progress").await?,
        "job_progress",
    )?;
    let stored_revision = stored.map_or(0, |progress| progress.revision);
    if stored_revision != expected_revision {
        return Err(RecordError::StaleRevision {
            row: row_doc_id.to_owned(),
            stored: stored_revision,
            expected: expected_revision,
        }
        .into());
    }
    let merged = JobProgress {
        title: truncate(guest.title),
        phases: guest
            .phases
            .into_iter()
            .map(|mut phase| {
                phase.title = truncate(phase.title);
                phase
            })
            .collect(),
        active: guest
            .active
            .into_iter()
            .map(|mut child| {
                child.title = truncate(child.title);
                child
            })
            .collect(),
        last_event: truncate(guest.last_event),
        spawned: host.spawned,
        budget: host.budget,
        observed_terminal_children: host.observed_terminal_children,
        state: host.state,
        pause_reason: host.pause_reason,
        revision: expected_revision + 1,
        ..guest
    };
    let value = serde_json::to_value(&merged)?;
    let bytes = serde_json::to_vec(&value)?.len();
    if bytes > JOB_PROGRESS_MAX_BYTES {
        return Err(RecordError::TooLarge {
            bytes,
            max: JOB_PROGRESS_MAX_BYTES,
        }
        .into());
    }
    write_record(txn, row_doc_id, "job_progress", value).await?;
    Ok(merged)
}

pub async fn read_progress<R: ConfigRead>(
    reader: &R,
    row_doc_id: &str,
) -> Result<Option<JobProgress>> {
    decode(
        read_record(reader, row_doc_id, "job_progress").await?,
        "job_progress",
    )
}

fn truncate(mut text: String) -> String {
    let mut end = JOB_PROGRESS_TEXT_MAX_BYTES;
    if text.len() > end {
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_client::ConfigAccess;
    use crate::graphql::escape_graphql_string;
    use crate::tool_call_lifecycle::admission_fixture::{
        fixture_job_pin, published_admission, PublishedAdmission, PublishedAdmissionOptions,
    };
    use gents_protocol::job::{
        JobActiveChild, JobBudget, JobPauseReason, JobPhase, JobPhaseState, JobPin, JobProgress,
        JobState,
    };

    fn guest_progress() -> JobProgress {
        JobProgress {
            title: "audit".to_owned(),
            state: JobState::Completed,
            pause_reason: None,
            phases: vec![JobPhase {
                title: "scan".to_owned(),
                state: JobPhaseState::Active,
            }],
            active: Vec::new(),
            spawned: 99,
            budget: JobBudget {
                total: 99,
                spent: 99,
                remaining: 0,
            },
            last_event: "scanning".to_owned(),
            last_event_at: None,
            question: None,
            observed_terminal_children: 99,
            revision: 99,
        }
    }

    fn host_facts() -> HostProgressFacts {
        HostProgressFacts {
            spawned: 2,
            budget: JobBudget {
                total: 4,
                spent: 2,
                remaining: 2,
            },
            observed_terminal_children: 1,
            state: JobState::Running,
            pause_reason: None,
        }
    }

    async fn row(name: &str) -> (PublishedAdmission, String) {
        let admission = published_admission(PublishedAdmissionOptions {
            name: name.into(),
            ..Default::default()
        })
        .await
        .unwrap();
        let doc_id = admission.tool.doc_id().unwrap().to_owned();
        (admission, doc_id)
    }

    async fn finish(admission: PublishedAdmission) {
        admission.node.shutdown().await;
        let _ = std::fs::remove_dir_all(admission.path);
    }

    async fn write_pin(
        admission: &PublishedAdmission,
        doc_id: &str,
        pin: &JobPin,
    ) -> anyhow::Result<()> {
        ConfigAccess::transact_local_write(&admission.node, None, "test.records.pin", |txn| {
            Box::pin(async move { write_pin_in_txn(txn, doc_id, pin).await })
        })
        .await
    }

    async fn write_progress(
        admission: &PublishedAdmission,
        doc_id: &str,
        expected: u64,
        guest: JobProgress,
        host: HostProgressFacts,
    ) -> anyhow::Result<JobProgress> {
        ConfigAccess::transact_local_write(&admission.node, None, "test.records.progress", |txn| {
            let (guest, host) = (guest.clone(), host.clone());
            Box::pin(async move { write_progress_in_txn(txn, doc_id, expected, guest, host).await })
        })
        .await
    }

    fn access(admission: &PublishedAdmission) -> ConfigAccess {
        ConfigAccess::local(admission.node.clone())
    }

    #[tokio::test]
    async fn identical_pin_replay_is_accepted() {
        let (admission, doc_id) = row("records-pin-replay").await;
        assert_eq!(read_pin(&access(&admission), &doc_id).await.unwrap(), None);
        let pin = fixture_job_pin();
        write_pin(&admission, &doc_id, &pin).await.unwrap();
        write_pin(&admission, &doc_id, &pin).await.unwrap();
        assert_eq!(
            read_pin(&access(&admission), &doc_id).await.unwrap(),
            Some(pin)
        );
        finish(admission).await;
    }

    #[tokio::test]
    async fn different_pin_is_an_integrity_error() {
        let (admission, doc_id) = row("records-pin-conflict").await;
        let pin = fixture_job_pin();
        write_pin(&admission, &doc_id, &pin).await.unwrap();
        let mut other = pin.clone();
        other.handle = "another-handle".to_owned();
        let error = write_pin(&admission, &doc_id, &other).await.unwrap_err();
        assert!(matches!(
            error.downcast_ref::<RecordError>(),
            Some(RecordError::PinConflict(_))
        ));
        assert_eq!(
            read_pin(&access(&admission), &doc_id).await.unwrap(),
            Some(pin)
        );
        finish(admission).await;
    }

    #[tokio::test]
    async fn stale_progress_revision_is_refused() {
        let (admission, doc_id) = row("records-stale").await;
        write_progress(&admission, &doc_id, 0, guest_progress(), host_facts())
            .await
            .unwrap();
        let error = write_progress(&admission, &doc_id, 0, guest_progress(), host_facts())
            .await
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<RecordError>(),
            Some(RecordError::StaleRevision {
                stored: 1,
                expected: 0,
                ..
            })
        ));
        let stored = read_progress(&access(&admission), &doc_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.revision, 1);
        finish(admission).await;
    }

    #[tokio::test]
    async fn progress_write_increments_the_revision_and_ignores_the_guest_revision() {
        let (admission, doc_id) = row("records-revision").await;
        assert_eq!(
            read_progress(&access(&admission), &doc_id).await.unwrap(),
            None
        );
        let first = write_progress(&admission, &doc_id, 0, guest_progress(), host_facts())
            .await
            .unwrap();
        assert_eq!(first.revision, 1);
        let mut facts = host_facts();
        facts.state = JobState::Paused;
        facts.pause_reason = Some(JobPauseReason::BackOff);
        let second = write_progress(&admission, &doc_id, 1, guest_progress(), facts)
            .await
            .unwrap();
        assert_eq!(second.revision, 2);
        assert_eq!(second.state, JobState::Paused);
        assert_eq!(second.pause_reason, Some(JobPauseReason::BackOff));
        assert_eq!(second.spawned, 2);
        assert_eq!(second.budget.total, 4);
        assert_eq!(second.observed_terminal_children, 1);
        assert_eq!(second.title, "audit");
        assert_eq!(second.last_event, "scanning");
        assert_eq!(
            read_progress(&access(&admission), &doc_id).await.unwrap(),
            Some(second)
        );
        finish(admission).await;
    }

    #[tokio::test]
    async fn titles_and_events_truncate_to_256_bytes_on_a_char_boundary() {
        let (admission, doc_id) = row("records-truncate").await;
        let mut guest = guest_progress();
        // 2-byte chars: 300 bytes cut at 256 is already on a boundary; the
        // 3-byte chars below put byte 256 mid-character.
        guest.title = "\u{e9}".repeat(150);
        guest.last_event = "\u{20ac}".repeat(100);
        guest.phases[0].title = "p".repeat(300);
        guest.active = vec![JobActiveChild {
            slot: "a".to_owned(),
            title: "\u{20ac}".repeat(100),
            session_id: "s".to_owned(),
        }];
        let stored = write_progress(&admission, &doc_id, 0, guest, host_facts())
            .await
            .unwrap();
        assert_eq!(stored.title.len(), 256);
        assert_eq!(stored.last_event, "\u{20ac}".repeat(85));
        assert_eq!(stored.phases[0].title.len(), 256);
        assert_eq!(stored.active[0].title, "\u{20ac}".repeat(85));
        assert_eq!(
            read_progress(&access(&admission), &doc_id).await.unwrap(),
            Some(stored)
        );
        finish(admission).await;
    }

    #[tokio::test]
    async fn progress_over_16_kib_fails_closed() {
        let (admission, doc_id) = row("records-oversize").await;
        let mut guest = guest_progress();
        guest.phases = (0..80)
            .map(|_| JobPhase {
                title: "p".repeat(256),
                state: JobPhaseState::Pending,
            })
            .collect();
        let error = write_progress(&admission, &doc_id, 0, guest, host_facts())
            .await
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<RecordError>(),
            Some(RecordError::TooLarge { .. })
        ));
        assert_eq!(
            read_progress(&access(&admission), &doc_id).await.unwrap(),
            None
        );
        finish(admission).await;
    }

    fn step_evidence(input: &str) -> PluginExecutionReceipt {
        PluginExecutionReceipt {
            coordinate: "fixture/engine".into(),
            artifact_digest: "sha256:engine".into(),
            input_digest: input.into(),
            output_digest: Some("sha256:output".into()),
            authority: None,
            limits: None,
            verdict: gents_protocol::plugin::PluginExecutionVerdict::Success,
        }
    }

    fn resume(answer: &str) -> JobResume {
        JobResume {
            row_doc_id: "row".to_owned(),
            engine: None,
            answer: Some(answer.to_owned()),
            budget: None,
        }
    }

    async fn write_step_state(
        admission: &PublishedAdmission,
        doc_id: &str,
        evidence: &PluginExecutionReceipt,
        resume: Option<&JobResume>,
    ) -> anyhow::Result<()> {
        ConfigAccess::transact_local_write(&admission.node, None, "test.records.step", |txn| {
            Box::pin(
                async move { write_step_state_in_txn(txn, doc_id, evidence, resume, None).await },
            )
        })
        .await
    }

    #[tokio::test]
    async fn step_state_write_replaces_the_evidence_and_keeps_the_resume() {
        let (admission, doc_id) = row("records-step-state").await;
        assert_eq!(
            read_step_state(&access(&admission), &doc_id).await.unwrap(),
            None
        );
        let (a, b, r) = (step_evidence("a"), step_evidence("b"), resume("yes"));
        write_step_state(&admission, &doc_id, &a, Some(&r))
            .await
            .unwrap();
        write_step_state(&admission, &doc_id, &b, None)
            .await
            .unwrap();
        let expected = JobStepState {
            evidence: Some(b),
            resume: Some(r),
            guest: None,
        };
        assert_eq!(
            read_step_state(&access(&admission), &doc_id).await.unwrap(),
            Some(expected.clone())
        );
        let error = write_step_state(&admission, &doc_id, &a, Some(&resume("no")))
            .await
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<RecordError>(),
            Some(&RecordError::ResumeConflict(doc_id.clone()))
        );
        assert_eq!(
            read_step_state(&access(&admission), &doc_id).await.unwrap(),
            Some(expected)
        );
        let receipt = access(&admission)
            .execute(&format!(
                r#"{{ AgentToolCall(docID: "{}") {{ plugin_execution_receipt }} }}"#,
                escape_graphql_string(&doc_id)
            ))
            .await
            .unwrap();
        assert!(receipt["data"]["AgentToolCall"][0]["plugin_execution_receipt"].is_null());
        finish(admission).await;
    }
}
