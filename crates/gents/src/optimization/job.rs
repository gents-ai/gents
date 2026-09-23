//! The `OptimizationJob` document: a frozen origin, an append-only journal
//! guarded by its own length, and a state derived from the journal.
//!
//! This is the Rust refinement of `Optimization.appendIf`. The guard is the
//! `journal_len` predicate on the update mutation, so two writers who read the
//! same journal cannot both extend it: the loser matches no row, gets a
//! [`JournalConflict`], and the journal is left exactly as it was.
//!
//! The job is the driver's notebook, not a request. `state` is derived, never
//! claimed, and no runtime ever reconciles this collection.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::document_config::EvalSplit;
use crate::eval::{DefinitionRef, SubjectRef};
use crate::graphql::escape_graphql_string;
use crate::optimization::policy::{Decision, Mode, PolicyV2};
use crate::optimization::target::{FrozenDocument, Target};

/// What a job may spend. `max_rounds` is also the Bonferroni divisor: the
/// driver refuses a job whose policy disagrees with it, so the significance
/// level a decision is judged at always matches the number of candidates the
/// job may try on one validation split.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budgets {
    pub max_rounds: u32,
    /// Total case-trials across every run of the job, including the reserved
    /// held-out run.
    pub max_case_trials: u64,
    pub max_tokens: u64,
    pub deadline_unix_secs: Option<u64>,
}

/// Frozen at creation and never rewritten.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobOrigin {
    pub target: Target,
    /// The owner's full desired configuration at freeze, as digests.
    pub closure: Vec<FrozenDocument>,
    pub subject: SubjectRef,
    pub definition: DefinitionRef,
    pub policy: PolicyV2,
    pub trials_per_case: u32,
    pub budgets: Budgets,
    /// The target's text at freeze: the first checkpoint, and what `revert`
    /// would restore if nothing had been promoted since.
    pub baseline_text: String,
    pub owner: String,
    pub seed_base: i64,
    /// Frozen so a resumed job cannot silently change the model both arms run
    /// on; a resume that names another profile is refused.
    pub inference_profile_id: String,
    pub max_text_bytes: usize,
    /// `<launching home>/eval/jobs` (ruling R8). The job owns
    /// `<jobs_dir>/<job_id>/`; `promote` rebuilds the checkpoint from the
    /// baseline copy there, so the operator's verb needs only the job id.
    pub jobs_dir: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum JobState {
    Running,
    ReadyToPromote,
    NothingToPromote,
    Exhausted,
    Failed {
        reason: String,
    },
    Promoted,
    Stale,
    /// Terminal (ruling R4). A further promotion is a new job.
    Reverted,
}

impl JobState {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::ReadyToPromote => "ready_to_promote",
            Self::NothingToPromote => "nothing_to_promote",
            Self::Exhausted => "exhausted",
            Self::Failed { .. } => "failed",
            Self::Promoted => "promoted",
            Self::Stale => "stale",
            Self::Reverted => "reverted",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriftedRef {
    pub collection: String,
    pub id: String,
}

/// The convenience numbers behind a decision. The authority is the referenced
/// runs' `EvalVerdict` rows; `optimization::show::show`
/// recomputes every decision from them and flags a mismatch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionSummary {
    pub improved: u32,
    pub tied: u32,
    pub worsened: u32,
    pub mean_diff_bp: Option<i64>,
    pub p_ppm: Option<u64>,
    pub alpha_effective_ppm: u64,
    pub cost_skipped: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum JournalEntry {
    Frozen,
    /// Written before the runner is asked for a run. One without a matching
    /// `Decided` means the run may be incomplete, and replay resumes it.
    RunStarted {
        run_id: String,
        round: Option<u32>,
        split: EvalSplit,
    },
    Proposed {
        round: u32,
        text: String,
        rationale: String,
        candidate_digest: String,
    },
    StructuralReject {
        round: u32,
        diagnostics: String,
    },
    Decided {
        round: Option<u32>,
        attempt: u32,
        run_ids: Vec<String>,
        mode: Mode,
        decision: Decision,
        policy_version: String,
        summary: DecisionSummary,
    },
    /// A candidate that was never evaluated is not a rejection.
    BudgetExhausted {
        round: Option<u32>,
        reason: String,
    },
    Finalized {
        state: JobState,
    },
    Promoted {
        by: String,
        target_digest: String,
        previous_text: String,
        previous_digest: String,
    },
    PromotionRefused {
        drifted: Vec<DriftedRef>,
    },
    Reverted {
        by: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobRecord {
    pub job_id: String,
    pub owner: String,
    pub origin: JobOrigin,
    pub journal: Vec<JournalEntry>,
}

/// The retained checkpoint: the candidate of the last round the policy
/// accepted. Never the best candidate the job ever saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub round: u32,
    pub text: String,
    pub pack_digest: String,
}

pub fn derive_state(journal: &[JournalEntry]) -> JobState {
    let mut state = JobState::Running;
    for entry in journal {
        match entry {
            JournalEntry::Finalized { state: finalized } => state = finalized.clone(),
            JournalEntry::Promoted { .. } => state = JobState::Promoted,
            JournalEntry::PromotionRefused { .. } => state = JobState::Stale,
            // Terminal, as spec section 6 draws it: `Promoted -> Reverted`.
            JournalEntry::Reverted { .. } => state = JobState::Reverted,
            _ => {}
        }
    }
    state
}

pub fn checkpoint(journal: &[JournalEntry]) -> Option<Checkpoint> {
    let mut retained = None;
    for entry in journal {
        let JournalEntry::Decided {
            round: Some(round),
            decision: Decision::Accept,
            ..
        } = entry
        else {
            continue;
        };
        retained = journal.iter().find_map(|candidate| match candidate {
            JournalEntry::Proposed {
                round: proposed_round,
                text,
                candidate_digest,
                ..
            } if proposed_round == round => Some(Checkpoint {
                round: *round,
                text: text.clone(),
                pack_digest: candidate_digest.clone(),
            }),
            _ => None,
        });
    }
    retained
}

/// `Optimization.roundsUsed`: one per `Proposed`, whatever it came to.
pub fn rounds_used(journal: &[JournalEntry]) -> u32 {
    journal
        .iter()
        .filter(|entry| matches!(entry, JournalEntry::Proposed { .. }))
        .count() as u32
}

/// The append lost the length guard: another writer extended the journal
/// first, so nothing was written.
#[derive(Debug)]
pub struct JournalConflict {
    pub expected: usize,
}

impl std::fmt::Display for JournalConflict {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "optimization job journal moved past length {}; another writer appended first",
            self.expected
        )
    }
}

impl std::error::Error for JournalConflict {}

pub fn journal_conflict(error: &anyhow::Error) -> Option<&JournalConflict> {
    error.downcast_ref::<JournalConflict>()
}

fn filter(owner: &str, job_id: &str) -> String {
    format!(
        r#"owner_agent_did: {{ _eq: "{}" }}, job_id: {{ _eq: "{}" }}"#,
        escape_graphql_string(owner),
        escape_graphql_string(job_id)
    )
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub async fn create_job(
    access: &ConfigAccess,
    job_id: &str,
    owner: &str,
    origin: &JobOrigin,
) -> Result<JobRecord> {
    let variables = json!({ "input": {
        "job_id": job_id,
        "owner_agent_did": owner,
        "origin": serde_json::to_string(origin)?,
        "journal": "[]",
        "journal_len": 0,
        "state": JobState::Running.label(),
        "created_at": now(),
    }});
    access
        .transact("optimization.create_job", |txn| {
            let variables = &variables;
            Box::pin(async move {
                txn.execute_with_variables(
                    "mutation($input: OptimizationJobMutationInputArg!) { create_OptimizationJob(input: $input) { _docID } }",
                    variables,
                )
                .await
                .map(|_| ())
            })
        })
        .await
        .context("create_OptimizationJob")?;
    tracing::info!(job_id, owner, "optimization job created");
    Ok(JobRecord {
        job_id: job_id.to_owned(),
        owner: owner.to_owned(),
        origin: origin.clone(),
        journal: Vec::new(),
    })
}

const JOB_FIELDS: &str = "job_id owner_agent_did origin journal journal_len state";

fn decode(row: &Value) -> Result<JobRecord> {
    Ok(JobRecord {
        job_id: row["job_id"].as_str().context("job_id")?.to_owned(),
        owner: row["owner_agent_did"]
            .as_str()
            .context("owner_agent_did")?
            .to_owned(),
        origin: serde_json::from_str(row["origin"].as_str().context("origin")?)
            .context("decoding job origin")?,
        journal: serde_json::from_str(row["journal"].as_str().unwrap_or("[]"))
            .context("decoding job journal")?,
    })
}

pub(crate) async fn load_job_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    job_id: &str,
) -> Result<Option<JobRecord>> {
    let response = txn
        .execute(&format!(
            "{{ OptimizationJob(filter: {{ {} }}) {{ {JOB_FIELDS} }} }}",
            filter(owner, job_id)
        ))
        .await?;
    response["data"]["OptimizationJob"]
        .as_array()
        .and_then(|rows| rows.first())
        .map(decode)
        .transpose()
}

pub async fn load_job(
    access: &ConfigAccess,
    owner: &str,
    job_id: &str,
) -> Result<Option<JobRecord>> {
    let (owner, job_id) = (owner.to_owned(), job_id.to_owned());
    access
        .transact("optimization.load_job", |txn| {
            let (owner, job_id) = (&owner, &job_id);
            Box::pin(async move { load_job_in_txn(txn, owner, job_id).await })
        })
        .await
}

/// `Optimization.appendIf`. The update matches only while `journal_len` still
/// equals the length this writer read, so a stale writer changes nothing.
pub(crate) async fn append_in_txn(
    txn: &ConfigApplyTxn<'_>,
    job: &JobRecord,
    entry: &JournalEntry,
) -> Result<()> {
    let expected = job.journal.len();
    let mut journal = job.journal.clone();
    journal.push(entry.clone());
    let variables = json!({ "input": {
        "journal": serde_json::to_string(&journal)?,
        "journal_len": journal.len(),
        "state": derive_state(&journal).label(),
    }});
    let response = txn
        .execute_with_variables(
            &format!(
                "mutation($input: OptimizationJobMutationInputArg!) {{ update_OptimizationJob(filter: {{ {}, journal_len: {{ _eq: {expected} }} }}, input: $input) {{ _docID }} }}",
                filter(&job.owner, &job.job_id)
            ),
            &variables,
        )
        .await?;
    let matched = response["data"]["update_OptimizationJob"]
        .as_array()
        .is_some_and(|rows| !rows.is_empty());
    if matched {
        Ok(())
    } else {
        Err(anyhow::Error::new(JournalConflict { expected }))
    }
}

/// Append one entry, advancing `job` only when the write took.
pub async fn append(access: &ConfigAccess, job: &mut JobRecord, entry: JournalEntry) -> Result<()> {
    {
        let (current, entry) = (&*job, &entry);
        access
            .transact("optimization.append_journal", |txn| {
                Box::pin(async move { append_in_txn(txn, current, entry).await })
            })
            .await?;
    }
    job.journal.push(entry);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::optimization::policy::{InconclusiveReason, RejectReason};
    use crate::optimization::target::{Target, TargetField};
    use std::sync::Arc;

    async fn access() -> ConfigAccess {
        let node = Arc::new(
            crate::defra_node::EmbeddedNode::builder()
                .build()
                .await
                .unwrap(),
        );
        crate::schema::ensure_runtime_schemas(&node).await.unwrap();
        ConfigAccess::Local(node)
    }

    const OWNER: &str = "did:key:job-owner";

    fn origin() -> JobOrigin {
        JobOrigin {
            target: Target {
                field: TargetField::AgentContextSystemPrompt,
                owner: OWNER.into(),
                id: "monitor-context".into(),
            },
            closure: Vec::new(),
            subject: crate::eval::SubjectRef {
                pack_digest: "sha256:baseline".into(),
                behavior_id: "monitor".into(),
            },
            definition: crate::eval::DefinitionRef {
                definition_id: "monitor-findings".into(),
                comparability_version: 1,
                digest: "sha256:definition".into(),
            },
            policy: PolicyV2::uncalibrated(),
            trials_per_case: 2,
            budgets: Budgets {
                max_rounds: 3,
                max_case_trials: 1_000,
                max_tokens: 1_000_000,
                deadline_unix_secs: None,
            },
            baseline_text: "Watch the mailbox.\n".into(),
            owner: OWNER.into(),
            seed_base: 1_000,
            inference_profile_id: "local".into(),
            max_text_bytes: 32 * 1024,
            jobs_dir: std::path::PathBuf::from("/home/eval/jobs"),
        }
    }

    fn summary() -> DecisionSummary {
        DecisionSummary {
            improved: 6,
            tied: 0,
            worsened: 0,
            mean_diff_bp: Some(3_000),
            p_ppm: Some(15_625),
            alpha_effective_ppm: 16_666,
            cost_skipped: true,
        }
    }

    fn proposed(round: u32, text: &str) -> JournalEntry {
        JournalEntry::Proposed {
            round,
            text: text.into(),
            rationale: format!("round {round}"),
            candidate_digest: format!("sha256:candidate-{round}"),
        }
    }

    fn decided(round: u32, decision: Decision) -> JournalEntry {
        JournalEntry::Decided {
            round: Some(round),
            attempt: 0,
            run_ids: vec![format!("job-r{round}-v0")],
            mode: Mode::Improve,
            decision,
            policy_version: crate::optimization::POLICY_VERSION.to_owned(),
            summary: summary(),
        }
    }

    #[tokio::test]
    async fn a_job_round_trips_through_the_document() {
        let access = access().await;
        let mut job = create_job(&access, "job-1", OWNER, &origin())
            .await
            .unwrap();
        append(&access, &mut job, JournalEntry::Frozen)
            .await
            .unwrap();
        append(
            &access,
            &mut job,
            JournalEntry::RunStarted {
                run_id: "job-1-r1-train".into(),
                round: Some(1),
                split: EvalSplit::Train,
            },
        )
        .await
        .unwrap();
        let loaded = load_job(&access, OWNER, "job-1").await.unwrap().unwrap();
        assert_eq!(loaded, job);
        assert_eq!(derive_state(&loaded.journal), JobState::Running);
    }

    /// `Optimization.appendIf_match` and `appendIf_prefix`: an append at the
    /// length the writer read extends the journal by exactly one entry, and
    /// every earlier entry is preserved in order.
    #[tokio::test]
    async fn an_append_at_the_expected_length_extends_the_journal_by_one() {
        let access = access().await;
        let mut job = create_job(&access, "job-2", OWNER, &origin())
            .await
            .unwrap();
        let entries = [
            JournalEntry::Frozen,
            proposed(1, "one"),
            decided(1, Decision::Accept),
        ];
        for entry in entries.iter().cloned() {
            let before = job.journal.clone();
            append(&access, &mut job, entry.clone()).await.unwrap();
            assert_eq!(job.journal.len(), before.len() + 1);
            assert_eq!(
                &job.journal[..before.len()],
                &before[..],
                "a prefix is never rewritten"
            );
            assert_eq!(job.journal.last(), Some(&entry));
        }
        let loaded = load_job(&access, OWNER, "job-2").await.unwrap().unwrap();
        assert_eq!(loaded.journal, entries.to_vec());
    }

    /// `Optimization.appendIf_stale_unchanged`: an append whose expected length
    /// does not match leaves the journal exactly as it was.
    #[tokio::test]
    async fn a_stale_writer_cannot_append_and_changes_nothing() {
        let access = access().await;
        let mut job = create_job(&access, "job-3", OWNER, &origin())
            .await
            .unwrap();
        let mut stale = job.clone();
        append(&access, &mut job, JournalEntry::Frozen)
            .await
            .unwrap();

        let error = append(&access, &mut stale, proposed(1, "racing"))
            .await
            .unwrap_err();
        let conflict = journal_conflict(&error).unwrap_or_else(|| panic!("{error:#}"));
        assert_eq!(conflict.expected, 0);
        assert!(
            stale.journal.is_empty(),
            "a refused append never mutates the caller's copy"
        );

        let loaded = load_job(&access, OWNER, "job-3").await.unwrap().unwrap();
        assert_eq!(loaded.journal, vec![JournalEntry::Frozen]);
        assert_eq!(derive_state(&loaded.journal), JobState::Running);
    }

    #[test]
    fn the_checkpoint_is_the_last_accepted_round_and_never_the_best_seen() {
        let journal = vec![
            JournalEntry::Frozen,
            proposed(1, "one"),
            decided(1, Decision::Accept),
            proposed(2, "two"),
            decided(2, Decision::Reject(RejectReason::NoImprovement)),
            proposed(3, "three"),
            decided(3, Decision::Inconclusive(InconclusiveReason::Insufficient)),
        ];
        let retained = checkpoint(&journal).expect("round 1 was accepted");
        assert_eq!((retained.round, retained.text.as_str()), (1, "one"));
        assert_eq!(retained.pack_digest, "sha256:candidate-1");
        assert_eq!(
            checkpoint(&journal[..2]),
            None,
            "a proposal alone is not a checkpoint"
        );
        assert_eq!(rounds_used(&journal), 3, "every Proposed is a spent round");
    }

    #[test]
    fn the_state_is_derived_from_the_terminal_entries_in_order() {
        let mut journal = vec![JournalEntry::Frozen];
        assert_eq!(derive_state(&journal), JobState::Running);
        journal.push(JournalEntry::Finalized {
            state: JobState::ReadyToPromote,
        });
        assert_eq!(derive_state(&journal), JobState::ReadyToPromote);
        journal.push(JournalEntry::PromotionRefused {
            drifted: vec![DriftedRef {
                collection: "AgentContext".into(),
                id: "monitor-context".into(),
            }],
        });
        assert_eq!(derive_state(&journal), JobState::Stale);
        journal.push(JournalEntry::Promoted {
            by: OWNER.into(),
            target_digest: "sha256:after".into(),
            previous_text: "Watch the mailbox.\n".into(),
            previous_digest: "sha256:before".into(),
        });
        assert_eq!(derive_state(&journal), JobState::Promoted);
        journal.push(JournalEntry::Reverted { by: OWNER.into() });
        assert_eq!(
            derive_state(&journal),
            JobState::Reverted,
            "ruling R4: a revert is terminal; a further promotion is a new job"
        );
        assert_eq!(JobState::Reverted.label(), "reverted");
    }

    /// Ruling P-B3: `origin` is stored as a JSON string, so integers past
    /// `i64::MAX` survive the document unchanged.
    #[tokio::test]
    async fn an_origin_with_extreme_budgets_round_trips_through_the_document() {
        let access = access().await;
        let mut written = origin();
        written.budgets = Budgets {
            max_rounds: u32::MAX,
            max_case_trials: u64::MAX - 1,
            max_tokens: u64::MAX,
            deadline_unix_secs: Some(u64::MAX),
        };
        written.max_text_bytes = usize::MAX;
        written.baseline_text = "Quote \"this\"\\ and\nthat {}.\n".into();
        create_job(&access, "job-4", OWNER, &written).await.unwrap();
        let loaded = load_job(&access, OWNER, "job-4").await.unwrap().unwrap();
        assert_eq!(loaded.origin, written);
        assert!(loaded.journal.is_empty());
    }
}
