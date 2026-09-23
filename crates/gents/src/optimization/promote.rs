//! Promotion and revert: the operator's two verbs.
//!
//! Both run in one transaction that writes only the target document. There
//! is no force flag: on drift the transaction rolls back with the live node
//! untouched, a second transaction journals the refusal, and the job is
//! `Stale`. Nothing reverts on its own, and a revert ends the job.
//!
//! Spec section 7 rebuilds the patch from the live target and checks it
//! against the journal. Here the retained checkpoint pack is verified in place
//! (`verified_checkpoint`) and must read back the journaled text, the patch is
//! built from the live target document, and the digest the write leaves on
//! the live target is asserted equal to the one the patch predicts before it
//! is journaled (ruling P-N8). Nothing is materialized on disk.

use anyhow::Result;

use crate::config_client::{
    apply_desired_state_plan, stale_expectation, ConfigAccess, DesiredStateApplyPlan,
};
use crate::optimization::driver::verified_checkpoint;
use crate::optimization::job::{
    append, append_in_txn, checkpoint, derive_state, load_job, DriftedRef, JobOrigin, JobRecord,
    JobState, JournalEntry,
};
use crate::optimization::subject::{baseline_text, materialize_pack};
use crate::optimization::target::{
    apply_text, capture_closure, closure_digests, current_text, target_digest, target_plan,
    Closure, FrozenDocument,
};

/// What a promotion did, and what it replaced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Promotion {
    pub target_digest: String,
    pub previous_text: String,
    pub previous_digest: String,
}

/// Why an operator's verb did not run. `reason` is a closed vocabulary.
#[derive(Debug)]
pub struct PromoteRefused {
    pub reason: &'static str,
    pub detail: String,
}

impl std::fmt::Display for PromoteRefused {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.reason, self.detail)
    }
}

impl std::error::Error for PromoteRefused {}

pub fn promote_refused(error: &anyhow::Error) -> Option<&PromoteRefused> {
    error.downcast_ref::<PromoteRefused>()
}

fn refused(reason: &'static str, detail: impl Into<String>) -> anyhow::Error {
    anyhow::Error::from(PromoteRefused {
        reason,
        detail: detail.into(),
    })
}

/// The closure moved under the job. Carried out of the transaction so the
/// refusal can be journaled after the rollback.
#[derive(Debug)]
struct ClosureDrift(Vec<DriftedRef>);

impl std::fmt::Display for ClosureDrift {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "the frozen closure moved: {:?}", self.0)
    }
}

impl std::error::Error for ClosureDrift {}

/// Both ways a promotion learns the closure moved: the explicit comparison,
/// which also catches a document added since the freeze, and the transaction's
/// own digest preconditions, which catch a concurrent edit.
fn drift_of(error: &anyhow::Error) -> Option<Vec<DriftedRef>> {
    if let Some(drift) = error.downcast_ref::<ClosureDrift>() {
        return Some(drift.0.clone());
    }
    stale_expectation(error).map(|stale| {
        stale
            .drifted
            .iter()
            .map(|document| DriftedRef {
                collection: document.collection.graphql_type().to_owned(),
                id: document.id.clone(),
            })
            .collect()
    })
}

fn same_document(one: &FrozenDocument, other: &FrozenDocument) -> bool {
    one.collection == other.collection && one.owner == other.owner && one.id == other.id
}

/// Every frozen document that changed or vanished, then every live document
/// the freeze did not see.
fn between(frozen: &[FrozenDocument], live: &[FrozenDocument]) -> Vec<DriftedRef> {
    let reference = |document: &FrozenDocument| DriftedRef {
        collection: document.collection.graphql_type().to_owned(),
        id: document.id.clone(),
    };
    let mut drifted: Vec<DriftedRef> = frozen
        .iter()
        .filter(|document| {
            !live.iter().any(|candidate| {
                same_document(candidate, document) && candidate.digest == document.digest
            })
        })
        .map(reference)
        .collect();
    drifted.extend(
        live.iter()
            .filter(|document| {
                !frozen
                    .iter()
                    .any(|candidate| same_document(candidate, document))
            })
            .map(reference),
    );
    drifted
}

/// The promotion's plan over the live closure: `text` on the target, guarded
/// by the entire frozen closure, the target context's own baseline digest
/// included, so an edit of the target itself also fails the write.
fn promotion_plan(
    origin: &JobOrigin,
    live: &Closure,
    text: &str,
) -> Result<(DesiredStateApplyPlan, Promotion)> {
    let target = &origin.target;
    anyhow::ensure!(
        origin.closure.iter().any(|document| {
            document.collection == target.field.collection()
                && document.owner == target.owner
                && document.id == target.id
        }),
        "the frozen closure has no digest for the target {:?}",
        target.id
    );
    let patched = apply_text(live, target, text)?;
    let plan = target_plan(&patched, target, &origin.closure)?;
    let promotion = Promotion {
        target_digest: target_digest(&patched, target)?,
        previous_text: current_text(live, target)?,
        previous_digest: target_digest(live, target)?,
    };
    Ok((plan, promotion))
}

/// Load the job and check what both verbs require: who is asking, and what
/// state the job is in. `by` is the launching home's DID as its caller states
/// it; an enforced, authenticated boundary is spec 2b's.
async fn job_in_state(
    access: &ConfigAccess,
    owner: &str,
    job_id: &str,
    by: &str,
    expected: JobState,
) -> Result<JobRecord> {
    let job = load_job(access, owner, job_id)
        .await?
        .ok_or_else(|| refused("unknown_job", format!("no job {job_id:?} for {owner}")))?;
    if by != job.origin.owner {
        return Err(refused(
            "foreign_did",
            format!("{by:?} does not own job {job_id:?}"),
        ));
    }
    let state = derive_state(&job.journal);
    if state != expected {
        let reason = if expected == JobState::Promoted {
            "not_promoted"
        } else {
            "not_ready"
        };
        return Err(refused(
            reason,
            format!("job {job_id:?} is {}", state.label()),
        ));
    }
    Ok(job)
}

/// Write the retained checkpoint's text onto the live target, guarded by the
/// whole frozen closure.
pub async fn promote(
    access: &ConfigAccess,
    owner: &str,
    job_id: &str,
    digest: &str,
    by: &str,
) -> Result<Promotion> {
    let mut job = job_in_state(access, owner, job_id, by, JobState::ReadyToPromote).await?;
    let retained = checkpoint(&job.journal).ok_or_else(|| {
        refused(
            "no_checkpoint",
            format!("job {job_id:?} is ready but retained nothing"),
        )
    })?;
    if retained.pack_digest != digest {
        return Err(refused(
            "wrong_digest",
            format!(
                "job {job_id:?} retains {}, not {digest}",
                retained.pack_digest
            ),
        ));
    }

    // The pack the job evaluated, located through the origin (ruling R6),
    // still digests to the journal's and holds the text about to go live.
    let origin = &job.origin;
    let checked = verified_checkpoint(origin, job_id, &retained).and_then(|path| {
        let pack = materialize_pack(&path, &origin.owner, &origin.subject.behavior_id)?;
        let text = baseline_text(&pack)?;
        anyhow::ensure!(
            text == retained.text,
            "the checkpoint at {} does not hold the journaled text",
            path.display()
        );
        Ok(())
    });
    if let Err(error) = checked {
        return Err(refused("rebuild_mismatch", format!("{error:#}")));
    }

    let applied = access
        .transact("optimization.promote", |txn| {
            let (job, text, by) = (&job, &retained.text, by);
            Box::pin(async move {
                let live = capture_closure(txn, &job.owner).await?;
                let drifted = between(&job.origin.closure, &closure_digests(&live)?);
                if !drifted.is_empty() {
                    return Err(anyhow::Error::new(ClosureDrift(drifted)));
                }
                // Writes only the target; expects the entire frozen closure.
                let (plan, promotion) = promotion_plan(&job.origin, &live, text)?;
                apply_desired_state_plan(txn, &plan).await?;
                // Ruling P-N8: the live target now digests to what the patch
                // predicted, and that is the digest the journal records.
                let written =
                    target_digest(&capture_closure(txn, &job.owner).await?, &job.origin.target)?;
                anyhow::ensure!(
                    written == promotion.target_digest,
                    "the promoted target digests to {written}, not the patch's {}",
                    promotion.target_digest
                );
                let entry = JournalEntry::Promoted {
                    by: by.to_owned(),
                    target_digest: promotion.target_digest.clone(),
                    previous_text: promotion.previous_text.clone(),
                    previous_digest: promotion.previous_digest.clone(),
                };
                // Length-guarded on the journal the state checks read.
                append_in_txn(txn, job, &entry).await?;
                Ok(promotion)
            })
        })
        .await;

    match applied {
        Ok(promotion) => {
            tracing::info!(job_id, by, "optimization checkpoint promoted");
            Ok(promotion)
        }
        Err(error) => {
            let Some(drifted) = drift_of(&error) else {
                return Err(error);
            };
            // The write rolled back; a second transaction records why.
            let entry = JournalEntry::PromotionRefused {
                drifted: drifted.clone(),
            };
            append(access, &mut job, entry).await?;
            tracing::warn!(job_id, ?drifted, "promotion refused; the job is stale");
            Err(refused("stale_closure", format!("{drifted:?}")))
        }
    }
}

/// Write `previous_text` back, expecting the target to still hold exactly what
/// the promotion wrote. Terminal (ruling R4).
pub async fn revert(
    access: &ConfigAccess,
    owner: &str,
    job_id: &str,
    digest: &str,
    by: &str,
) -> Result<()> {
    let job = job_in_state(access, owner, job_id, by, JobState::Promoted).await?;
    let (promoted_digest, previous_text) = job
        .journal
        .iter()
        .rev()
        .find_map(|entry| match entry {
            JournalEntry::Promoted {
                target_digest,
                previous_text,
                ..
            } => Some((target_digest.clone(), previous_text.clone())),
            _ => None,
        })
        .ok_or_else(|| refused("not_promoted", "the job has no promotion to revert"))?;
    if promoted_digest != digest {
        return Err(refused(
            "wrong_digest",
            format!("job {job_id:?} promoted {promoted_digest}, not {digest}"),
        ));
    }

    let result = access
        .transact("optimization.revert", |txn| {
            let (job, promoted_digest, previous_text, by) =
                (&job, &promoted_digest, &previous_text, by);
            Box::pin(async move {
                let live = capture_closure(txn, &job.owner).await?;
                let target = &job.origin.target;
                let found = target_digest(&live, target)?;
                if &found != promoted_digest {
                    return Err(refused(
                        "target_moved",
                        format!(
                            "the target now digests to {found}, not the promoted {promoted_digest}"
                        ),
                    ));
                }
                let restored = apply_text(&live, target, previous_text)?;
                // Expects the digest the promotion journaled, on the target only.
                let expectation = [FrozenDocument {
                    collection: target.field.collection(),
                    owner: target.owner.clone(),
                    id: target.id.clone(),
                    digest: promoted_digest.clone(),
                }];
                let plan = target_plan(&restored, target, &expectation)?;
                apply_desired_state_plan(txn, &plan).await?;
                let entry = JournalEntry::Reverted { by: by.to_owned() };
                append_in_txn(txn, job, &entry).await?;
                Ok(())
            })
        })
        .await;

    match result {
        Ok(()) => {
            tracing::warn!(job_id, by, "promoted prompt reverted; the job is closed");
            Ok(())
        }
        Err(error) if stale_expectation(&error).is_some() => {
            Err(refused("target_moved", format!("{error:#}")))
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_client::DesiredStateApplyPlan;
    use crate::eval::runner::freeze::tests::OWNER;
    use crate::optimization::driver::job_dir;
    use crate::optimization::driver::matrix::{
        accepting_harness, rejecting_harness, Harness, BASELINE_PROMPT, CANDIDATE_PROMPT,
    };
    use crate::optimization::job::load_job;
    use crate::Collection;
    use serde_json::json;

    async fn read_closure(access: &ConfigAccess) -> Closure {
        access
            .transact("test.read_closure", |txn| {
                Box::pin(async move { capture_closure(txn, OWNER).await })
            })
            .await
            .unwrap()
    }

    /// The live target's text, read the way an operator would.
    async fn live_prompt(access: &ConfigAccess) -> Option<String> {
        read_closure(access)
            .await
            .iter()
            .find_map(|(collection, value)| {
                (*collection == Collection::AgentContext
                    && value["context_id"] == "monitor-context")
                    .then(|| {
                        value["system_prompt"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned()
                    })
            })
    }

    async fn state(harness: &Harness, job_id: &str) -> JobState {
        let job = load_job(harness.access(), OWNER, job_id)
            .await
            .unwrap()
            .unwrap();
        derive_state(&job.journal)
    }

    async fn checkpoint_digest(harness: &Harness, job_id: &str) -> String {
        let job = load_job(harness.access(), OWNER, job_id)
            .await
            .unwrap()
            .unwrap();
        checkpoint(&job.journal)
            .expect("the harness drove an accepting job")
            .pack_digest
    }

    fn operator_edit() -> (Collection, serde_json::Value) {
        (
            Collection::AgentContext,
            json!({
                "context_id": "monitor-context",
                "agent_did": OWNER,
                "display_name": "Monitor",
                "system_prompt": "An operator wrote this by hand.\n",
            }),
        )
    }

    /// Every path under `dir`, relative to it.
    fn tree(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut paths = Vec::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(next) = pending.pop() {
            for entry in std::fs::read_dir(&next).unwrap() {
                let path = entry.unwrap().path();
                paths.push(path.strip_prefix(dir).unwrap().to_path_buf());
                if path.is_dir() {
                    pending.push(path);
                }
            }
        }
        paths.sort();
        paths
    }

    #[tokio::test]
    async fn a_promotion_writes_only_the_target_and_records_what_it_replaced() {
        let (harness, request) = accepting_harness("promote-once").await;
        let digest = checkpoint_digest(&harness, &request.job_id).await;
        let before = read_closure(harness.access()).await;
        let on_disk = tree(&job_dir(&request.jobs_dir, &request.job_id));

        let promotion = promote(harness.access(), OWNER, &request.job_id, &digest, OWNER)
            .await
            .unwrap();
        assert_eq!(promotion.previous_text, BASELINE_PROMPT);
        assert_ne!(promotion.target_digest, promotion.previous_digest);
        assert_eq!(
            live_prompt(harness.access()).await.as_deref(),
            Some(CANDIDATE_PROMPT)
        );
        assert_eq!(state(&harness, &request.job_id).await, JobState::Promoted);

        // Only the target document moved.
        let after = read_closure(harness.access()).await;
        let job = load_job(harness.access(), OWNER, &request.job_id)
            .await
            .unwrap()
            .unwrap();
        let target = &job.origin.target;
        assert_eq!(
            after,
            apply_text(&before, target, CANDIDATE_PROMPT).unwrap()
        );

        // Ruling P-N8: the journaled digest is the live target's, and the one
        // the patch of the live document predicts.
        let Some(JournalEntry::Promoted {
            by,
            target_digest: journaled,
            previous_text,
            previous_digest,
        }) = job.journal.last()
        else {
            panic!("the last entry is not Promoted: {:?}", job.journal.last());
        };
        assert_eq!(by, OWNER, "the caller's DID is recorded");
        assert_eq!(journaled, &promotion.target_digest);
        assert_eq!(journaled, &target_digest(&after, target).unwrap());
        assert_eq!(
            journaled,
            &target_digest(
                &apply_text(&before, target, CANDIDATE_PROMPT).unwrap(),
                target
            )
            .unwrap()
        );
        assert_eq!(previous_text, BASELINE_PROMPT);
        assert_eq!(previous_digest, &target_digest(&before, target).unwrap());

        // Ruling C4: the promotion leaves nothing behind on disk.
        assert_eq!(
            tree(&job_dir(&request.jobs_dir, &request.job_id)),
            on_disk,
            "promote writes no scratch directory"
        );
    }

    /// Constraint 14 and the PR 1 carry-forward: the plan expects the whole
    /// frozen closure, the target context's own baseline digest included, so a
    /// concurrent edit of the target itself fails the write.
    #[tokio::test]
    async fn the_promotion_plan_expects_the_target_contexts_own_baseline_digest() {
        let (harness, request) = accepting_harness("promote-plan").await;
        let job = load_job(harness.access(), OWNER, &request.job_id)
            .await
            .unwrap()
            .unwrap();
        let live = read_closure(harness.access()).await;
        let target = &job.origin.target;
        let (plan, promotion) = promotion_plan(&job.origin, &live, CANDIDATE_PROMPT).unwrap();

        assert_eq!(plan.documents().len(), 1, "only the target is written");
        assert_eq!(plan.expected().len(), job.origin.closure.len());
        let baseline_digest = target_digest(&live, target).unwrap();
        assert_eq!(promotion.previous_digest, baseline_digest);
        let expectation = plan
            .expected()
            .iter()
            .find(|expectation| {
                expectation.collection == Collection::AgentContext
                    && expectation.owner == target.owner
                    && expectation.id == target.id
            })
            .expect("the target context is among the expectations");
        assert_eq!(
            expectation.digest.as_deref(),
            Some(baseline_digest.as_str())
        );

        // The operator edits the target between the read and the write.
        harness.install(vec![operator_edit()]).await;
        let error = apply(harness.access(), &plan).await.unwrap_err();
        let stale = stale_expectation(&error).unwrap_or_else(|| panic!("{error:#}"));
        assert!(
            stale
                .drifted
                .iter()
                .any(|document| document.collection == Collection::AgentContext
                    && document.id == target.id),
            "{stale}"
        );
        assert_eq!(
            live_prompt(harness.access()).await.as_deref(),
            Some("An operator wrote this by hand.\n")
        );
    }

    async fn apply(access: &ConfigAccess, plan: &DesiredStateApplyPlan) -> Result<()> {
        access
            .transact("test.apply_plan", |txn| {
                Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
            })
            .await
    }

    /// Spec section 9: an operator's edit of the target after the job froze is
    /// never overwritten; the promotion is refused and the job is stale.
    #[tokio::test]
    async fn an_operator_edit_of_the_target_before_promotion_refuses_it_and_survives() {
        let (harness, request) = accepting_harness("promote-edited").await;
        let digest = checkpoint_digest(&harness, &request.job_id).await;
        harness.install(vec![operator_edit()]).await;

        let error = promote(harness.access(), OWNER, &request.job_id, &digest, OWNER)
            .await
            .unwrap_err();
        let refusal = promote_refused(&error).unwrap_or_else(|| panic!("{error:#}"));
        assert_eq!(refusal.reason, "stale_closure");
        assert!(
            refusal.detail.contains("AgentContext"),
            "{}",
            refusal.detail
        );
        assert_eq!(
            live_prompt(harness.access()).await.as_deref(),
            Some("An operator wrote this by hand.\n"),
            "the operator's text is unchanged"
        );
        assert_eq!(state(&harness, &request.job_id).await, JobState::Stale);
        let job = load_job(harness.access(), OWNER, &request.job_id)
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(
                job.journal.last(),
                Some(JournalEntry::PromotionRefused { drifted })
                    if drifted.iter().any(|document| document.collection == "AgentContext"
                        && document.id == "monitor-context")
            ),
            "{:?}",
            job.journal.last()
        );
    }

    /// Ruling R4: a revert restores the previous text and ends the job.
    #[tokio::test]
    async fn a_revert_restores_the_previous_text_and_is_terminal() {
        let (harness, request) = accepting_harness("revert-once").await;
        let digest = checkpoint_digest(&harness, &request.job_id).await;
        let promotion = promote(harness.access(), OWNER, &request.job_id, &digest, OWNER)
            .await
            .unwrap();

        revert(
            harness.access(),
            OWNER,
            &request.job_id,
            &promotion.target_digest,
            OWNER,
        )
        .await
        .unwrap();
        assert_eq!(
            live_prompt(harness.access()).await.as_deref(),
            Some(BASELINE_PROMPT)
        );
        assert_eq!(state(&harness, &request.job_id).await, JobState::Reverted);
        let job = load_job(harness.access(), OWNER, &request.job_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            job.journal.last(),
            Some(&JournalEntry::Reverted { by: OWNER.into() })
        );

        let again = promote(harness.access(), OWNER, &request.job_id, &digest, OWNER)
            .await
            .unwrap_err();
        assert_eq!(
            promote_refused(&again).unwrap().reason,
            "not_ready",
            "a further promotion is a new job"
        );
        assert_eq!(
            live_prompt(harness.access()).await.as_deref(),
            Some(BASELINE_PROMPT)
        );
    }

    #[tokio::test]
    async fn a_revert_is_refused_once_the_target_moved_and_the_edit_survives() {
        let (harness, request) = accepting_harness("revert-moved").await;
        let digest = checkpoint_digest(&harness, &request.job_id).await;
        let promotion = promote(harness.access(), OWNER, &request.job_id, &digest, OWNER)
            .await
            .unwrap();
        harness.install(vec![operator_edit()]).await;

        let error = revert(
            harness.access(),
            OWNER,
            &request.job_id,
            &promotion.target_digest,
            OWNER,
        )
        .await
        .unwrap_err();
        assert_eq!(promote_refused(&error).unwrap().reason, "target_moved");
        assert_eq!(
            live_prompt(harness.access()).await.as_deref(),
            Some("An operator wrote this by hand.\n"),
            "the user's edit is preserved"
        );
        assert_eq!(state(&harness, &request.job_id).await, JobState::Promoted);
    }

    #[tokio::test]
    async fn a_closure_edited_after_the_freeze_refuses_the_promotion_and_marks_the_job_stale() {
        let (harness, request) = accepting_harness("promote-stale").await;
        let digest = checkpoint_digest(&harness, &request.job_id).await;
        // A document of the frozen closure that the promotion does not write.
        harness
            .install(vec![(
                Collection::AgentBehavior,
                json!({
                    "behavior_id": "monitor",
                    "agent_did": OWNER,
                    "display_name": "Monitor, renamed",
                    "context_id": "monitor-context",
                    "inference_profile_id": "local",
                }),
            )])
            .await;

        let error = promote(harness.access(), OWNER, &request.job_id, &digest, OWNER)
            .await
            .unwrap_err();
        let refusal = promote_refused(&error).unwrap_or_else(|| panic!("{error:#}"));
        assert_eq!(refusal.reason, "stale_closure");
        assert!(
            refusal.detail.contains("AgentBehavior"),
            "{}",
            refusal.detail
        );
        assert_eq!(
            live_prompt(harness.access()).await.as_deref(),
            Some(BASELINE_PROMPT),
            "the live node is unchanged"
        );
        assert_eq!(state(&harness, &request.job_id).await, JobState::Stale);
    }

    #[tokio::test]
    async fn a_foreign_did_a_wrong_digest_and_an_unready_job_are_each_refused() {
        let (harness, request) = accepting_harness("promote-refusals").await;
        let digest = checkpoint_digest(&harness, &request.job_id).await;

        let foreign = promote(
            harness.access(),
            OWNER,
            &request.job_id,
            &digest,
            "did:key:someone-else",
        )
        .await
        .unwrap_err();
        assert_eq!(promote_refused(&foreign).unwrap().reason, "foreign_did");

        let wrong = promote(
            harness.access(),
            OWNER,
            &request.job_id,
            "sha256:not-it",
            OWNER,
        )
        .await
        .unwrap_err();
        assert_eq!(promote_refused(&wrong).unwrap().reason, "wrong_digest");
        assert_eq!(
            live_prompt(harness.access()).await.as_deref(),
            Some(BASELINE_PROMPT),
            "a refused promotion writes nothing"
        );
        assert_eq!(
            state(&harness, &request.job_id).await,
            JobState::ReadyToPromote
        );

        let (rejecting, unready) = rejecting_harness("unready").await;
        let error = promote(
            rejecting.access(),
            OWNER,
            &unready.job_id,
            "sha256:anything",
            OWNER,
        )
        .await
        .unwrap_err();
        assert_eq!(promote_refused(&error).unwrap().reason, "not_ready");
    }

    #[tokio::test]
    async fn an_unknown_job_and_an_edited_checkpoint_pack_are_each_refused() {
        let (harness, request) = accepting_harness("promote-tampered").await;
        let digest = checkpoint_digest(&harness, &request.job_id).await;

        let unknown = promote(harness.access(), OWNER, "no-such-job", &digest, OWNER)
            .await
            .unwrap_err();
        assert_eq!(promote_refused(&unknown).unwrap().reason, "unknown_job");

        let job = load_job(harness.access(), OWNER, &request.job_id)
            .await
            .unwrap()
            .unwrap();
        let round = checkpoint(&job.journal).unwrap().round;
        let prompt =
            crate::optimization::driver::candidate_dir(&request.jobs_dir, &request.job_id, round)
                .join("agent_behaviors/monitor/system_prompt.md");
        assert!(prompt.exists(), "{}", prompt.display());
        std::fs::write(&prompt, "Edited on disk after the job accepted it.\n").unwrap();

        let error = promote(harness.access(), OWNER, &request.job_id, &digest, OWNER)
            .await
            .unwrap_err();
        assert_eq!(promote_refused(&error).unwrap().reason, "rebuild_mismatch");
        assert_eq!(
            live_prompt(harness.access()).await.as_deref(),
            Some(BASELINE_PROMPT)
        );
        assert_eq!(
            state(&harness, &request.job_id).await,
            JobState::ReadyToPromote
        );
    }

    /// Spec section 9: optimization is an operator's verb, never a model's.
    /// `self_config` is a directory module, so every file in it is scanned.
    #[test]
    fn the_model_facing_config_tool_has_no_optimization_surface() {
        for name in crate::self_config::SELF_CONFIG_TOOL_NAMES {
            assert!(
                !name.contains("optim"),
                "the self-config tool set names {name}"
            );
        }
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/self_config");
        let mut scanned = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            for word in ["optimization", "OptimizationJob", "promote", "revert"] {
                assert!(
                    !source.contains(word),
                    "{} mentions {word}; the optimization surface is the operator's",
                    path.display()
                );
            }
            scanned += 1;
        }
        assert!(
            scanned >= 2,
            "expected mod.rs and its siblings in {}",
            dir.display()
        );
    }
}
