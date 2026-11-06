use anyhow::Result;

use crate::eval::optimization::driver::verified_checkpoint;
use crate::eval::optimization::job::{
    append, append_in_txn, checkpoint, derive_state, load_job, DriftedRef, JobOrigin, JobRecord,
    JobState, JournalEntry,
};
use crate::eval::optimization::subject::{baseline_text, materialize_pack};
use crate::eval::optimization::target::{
    apply_text, capture_closure, closure_digests, current_text, target_digest, target_plan,
    Closure, FrozenDocument,
};
use crate::store::config_client::{
    apply_desired_state_plan, stale_expectation, ConfigAccess, DesiredStateApplyPlan,
    TransactionFailure,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Promotion {
    pub target_digest: String,
    pub previous_text: String,
    pub previous_digest: String,
}

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

#[derive(Debug)]
struct ClosureDrift(Vec<DriftedRef>);

impl std::fmt::Display for ClosureDrift {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "the frozen closure moved: {:?}", self.0)
    }
}

impl std::error::Error for ClosureDrift {}

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

async fn job_in_state(
    access: &ConfigAccess,
    owner: &str,
    job_id: &str,
    expected: JobState,
) -> Result<JobRecord> {
    let job = load_job(access, owner, job_id)
        .await?
        .ok_or_else(|| refused("unknown_job", format!("no job {job_id:?} for {owner}")))?;
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

#[cfg(test)]
tokio::task_local! {
    static BETWEEN_CHECK_AND_WRITE: std::sync::Arc<(tokio::sync::Notify, tokio::sync::Notify)>;
}

/// A conflicted signed promotion is not replayed. After a confirmed rollback,
/// a fresh authorized read may establish closure drift and justify a stale refusal.
pub async fn promote(
    access: &ConfigAccess,
    owner: &str,
    job_id: &str,
    digest: &str,
) -> Result<Promotion> {
    let by = access.actor_did().ok_or_else(|| {
        refused(
            "actor_required",
            "optimization requires an authenticated caller",
        )
    })?;
    let mut job = job_in_state(access, owner, job_id, JobState::ReadyToPromote).await?;
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

    let origin = &job.origin;
    let checked = verified_checkpoint(origin, job_id, &retained).and_then(|path| {
        let pack = materialize_pack(
            &path,
            &origin.owner,
            &origin.subject.agent_slot,
            &origin.target.job_target(),
        )?;
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
        .transact_write_with_finality("optimization.promote", |txn| {
            let (job, text, by) = (&job, &retained.text, &by);
            Box::pin(async move {
                let live = capture_closure(txn, &job.owner).await?;
                let drifted = between(&job.origin.closure, &closure_digests(&live)?);
                if !drifted.is_empty() {
                    return Err(anyhow::Error::new(ClosureDrift(drifted)));
                }
                #[cfg(test)]
                if let Ok(gate) = BETWEEN_CHECK_AND_WRITE.try_with(std::sync::Arc::clone) {
                    gate.0.notify_one();
                    gate.1.notified().await;
                }
                let (plan, promotion) = promotion_plan(&job.origin, &live, text)?;
                apply_desired_state_plan(txn, &plan).await?;
                let written =
                    target_digest(&capture_closure(txn, &job.owner).await?, &job.origin.target)?;
                anyhow::ensure!(
                    written == promotion.target_digest,
                    "the promoted target digests to {written}, not the patch's {}",
                    promotion.target_digest
                );
                let entry = JournalEntry::Promoted {
                    by: (*by).clone(),
                    target_digest: promotion.target_digest.clone(),
                    previous_text: promotion.previous_text.clone(),
                    previous_digest: promotion.previous_digest.clone(),
                };
                append_in_txn(txn, job, &entry).await?;
                Ok(promotion)
            })
        })
        .await;

    match applied {
        Ok(promotion) => {
            tracing::info!(job_id, by = %by, "optimization checkpoint promoted");
            Ok(promotion)
        }
        Err(failure) => {
            if failure.unconfirmed {
                return Err(failure.error);
            }
            let error = failure.error;
            let drifted = if let Some(drifted) = drift_of(&error) {
                drifted
            } else if error
                .config_client_error()
                .is_some_and(|source| source.transaction_conflict().is_some())
            {
                let observed = access
                    .transact_readonly("optimization.promote_conflict_closure", |txn| {
                        Box::pin(async move { capture_closure(txn, owner).await })
                    })
                    .await;
                let Ok(live) = observed else {
                    return Err(error);
                };
                let Ok(digests) = closure_digests(&live) else {
                    return Err(error);
                };
                let drifted = between(&job.origin.closure, &digests);
                if drifted.is_empty() {
                    return Err(error);
                }
                drifted
            } else {
                return Err(error);
            };
            let entry = JournalEntry::PromotionRefused {
                drifted: drifted.clone(),
            };
            append(access, &mut job, entry).await?;
            tracing::warn!(job_id, ?drifted, "promotion refused; the job is stale");
            Err(refused("stale_closure", format!("{drifted:?}")))
        }
    }
}

pub async fn revert(access: &ConfigAccess, owner: &str, job_id: &str, digest: &str) -> Result<()> {
    let by = access.actor_did().ok_or_else(|| {
        refused(
            "actor_required",
            "optimization requires an authenticated caller",
        )
    })?;
    let job = job_in_state(access, owner, job_id, JobState::Promoted).await?;
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
        .transact_write("optimization.revert", |txn| {
            let (job, promoted_digest, previous_text, by) =
                (&job, &promoted_digest, &previous_text, &by);
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
                let expectation = [FrozenDocument {
                    collection: target.field.collection(),
                    owner: target.owner.clone(),
                    id: target.id.clone(),
                    digest: promoted_digest.clone(),
                }];
                let plan = target_plan(&restored, target, &expectation)?;
                apply_desired_state_plan(txn, &plan).await?;
                let entry = JournalEntry::Reverted { by: (*by).clone() };
                append_in_txn(txn, job, &entry).await?;
                Ok(())
            })
        })
        .await;

    match result {
        Ok(()) => {
            tracing::warn!(job_id, by = %by, "promoted prompt reverted; the job is closed");
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
    use crate::eval::optimization::driver::job_dir;
    use crate::eval::optimization::driver::matrix::fixtures::{BASELINE_PROMPT, CANDIDATE_PROMPT};
    use crate::eval::optimization::driver::matrix::{
        accepting_harness, accepting_task_harness, rejecting_harness, Harness, CANDIDATE_TEMPLATE,
    };
    use crate::eval::optimization::job::load_job;
    use crate::eval::optimization::subject::tests::FIXTURE_TEMPLATE;
    use crate::eval::optimization::target::TargetField;
    use crate::store::config_client::DesiredStateApplyPlan;
    use crate::Collection;
    use serde_json::json;

    async fn read_closure(harness: &Harness) -> Closure {
        harness
            .access()
            .transact_write("test.read_closure", |txn| {
                Box::pin(async move { capture_closure(txn, harness.owner()).await })
            })
            .await
            .unwrap()
    }

    async fn live_prompt(harness: &Harness) -> Option<String> {
        read_closure(harness)
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

    async fn live_template(harness: &Harness) -> Option<String> {
        read_closure(harness)
            .await
            .iter()
            .find_map(|(collection, value)| {
                (*collection == Collection::Task && value["task_id"] == "plan").then(|| {
                    value["prompt_template"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned()
                })
            })
    }

    #[tokio::test]
    async fn a_task_template_promotion_writes_the_task_and_reverts_it() {
        let (harness, request) = accepting_task_harness("promote-task").await;
        let digest = checkpoint_digest(&harness, &request.job_id).await;
        let job = load_job(harness.access(), harness.owner(), &request.job_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(job.origin.target.field, TargetField::TaskPromptTemplate);
        assert_eq!(job.origin.target.id, "plan");
        let before = read_closure(&harness).await;

        let promotion = promote(harness.access(), harness.owner(), &request.job_id, &digest)
            .await
            .unwrap();
        assert_eq!(promotion.previous_text, FIXTURE_TEMPLATE);
        assert_eq!(
            live_template(&harness).await.as_deref(),
            Some(CANDIDATE_TEMPLATE)
        );
        assert_eq!(
            live_prompt(&harness).await.as_deref(),
            Some(BASELINE_PROMPT),
            "the context is untouched"
        );
        let after = read_closure(&harness).await;
        assert_eq!(after.len(), before.len());

        revert(
            harness.access(),
            harness.owner(),
            &request.job_id,
            &promotion.target_digest,
        )
        .await
        .unwrap();
        assert_eq!(
            live_template(&harness).await.as_deref(),
            Some(FIXTURE_TEMPLATE)
        );
        assert_eq!(state(&harness, &request.job_id).await, JobState::Reverted);
    }

    async fn state(harness: &Harness, job_id: &str) -> JobState {
        let job = load_job(harness.access(), harness.owner(), job_id)
            .await
            .unwrap()
            .unwrap();
        derive_state(&job.journal)
    }

    async fn checkpoint_digest(harness: &Harness, job_id: &str) -> String {
        let job = load_job(harness.access(), harness.owner(), job_id)
            .await
            .unwrap()
            .unwrap();
        checkpoint(&job.journal)
            .expect("the harness drove an accepting job")
            .pack_digest
    }

    fn operator_edit(owner: &str) -> (Collection, serde_json::Value) {
        (
            Collection::AgentContext,
            json!({
                "context_id": "monitor-context",
                "node_did": owner,
                "display_name": "Monitor",
                "system_prompt": "An operator wrote this by hand.\n",
            }),
        )
    }

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
        let before = read_closure(&harness).await;
        let on_disk = tree(&job_dir(&request.jobs_dir, &request.job_id));

        let promotion = promote(harness.access(), harness.owner(), &request.job_id, &digest)
            .await
            .unwrap();
        assert_eq!(promotion.previous_text, BASELINE_PROMPT);
        assert_ne!(promotion.target_digest, promotion.previous_digest);
        assert_eq!(
            live_prompt(&harness).await.as_deref(),
            Some(CANDIDATE_PROMPT)
        );
        assert_eq!(state(&harness, &request.job_id).await, JobState::Promoted);

        let after = read_closure(&harness).await;
        let job = load_job(harness.access(), harness.owner(), &request.job_id)
            .await
            .unwrap()
            .unwrap();
        let target = &job.origin.target;
        assert_eq!(
            after,
            apply_text(&before, target, CANDIDATE_PROMPT).unwrap()
        );

        let Some(JournalEntry::Promoted {
            by,
            target_digest: journaled,
            previous_text,
            previous_digest,
        }) = job.journal.last()
        else {
            panic!("the last entry is not Promoted: {:?}", job.journal.last());
        };
        assert_eq!(
            by.as_str(),
            harness.evaluator_did(),
            "the caller's DID is recorded"
        );
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

        assert_eq!(
            tree(&job_dir(&request.jobs_dir, &request.job_id)),
            on_disk,
            "promote writes no scratch directory"
        );
    }

    #[tokio::test]
    async fn the_promotion_plan_expects_the_target_contexts_own_baseline_digest() {
        let (harness, request) = accepting_harness("promote-plan").await;
        let job = load_job(harness.access(), harness.owner(), &request.job_id)
            .await
            .unwrap()
            .unwrap();
        let live = read_closure(&harness).await;
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

        harness.install(vec![operator_edit(harness.owner())]).await;
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
            live_prompt(&harness).await.as_deref(),
            Some("An operator wrote this by hand.\n")
        );
    }

    async fn apply(access: &ConfigAccess, plan: &DesiredStateApplyPlan) -> Result<()> {
        access
            .transact_write("test.apply_plan", |txn| {
                Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
            })
            .await
    }

    #[tokio::test]
    async fn an_operator_edit_of_the_target_before_promotion_refuses_it_and_survives() {
        let (harness, request) = accepting_harness("promote-edited").await;
        let digest = checkpoint_digest(&harness, &request.job_id).await;
        harness.install(vec![operator_edit(harness.owner())]).await;

        let error = promote(harness.access(), harness.owner(), &request.job_id, &digest)
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
            live_prompt(&harness).await.as_deref(),
            Some("An operator wrote this by hand.\n"),
            "the operator's text is unchanged"
        );
        assert_eq!(state(&harness, &request.job_id).await, JobState::Stale);
        let job = load_job(harness.access(), harness.owner(), &request.job_id)
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

    #[tokio::test]
    async fn a_revert_restores_the_previous_text_and_is_terminal() {
        let (harness, request) = accepting_harness("revert-once").await;
        let digest = checkpoint_digest(&harness, &request.job_id).await;
        let promotion = promote(harness.access(), harness.owner(), &request.job_id, &digest)
            .await
            .unwrap();

        revert(
            harness.access(),
            harness.owner(),
            &request.job_id,
            &promotion.target_digest,
        )
        .await
        .unwrap();
        assert_eq!(
            live_prompt(&harness).await.as_deref(),
            Some(BASELINE_PROMPT)
        );
        assert_eq!(state(&harness, &request.job_id).await, JobState::Reverted);
        let job = load_job(harness.access(), harness.owner(), &request.job_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            job.journal.last(),
            Some(&JournalEntry::Reverted {
                by: harness.evaluator_did()
            })
        );

        let again = promote(harness.access(), harness.owner(), &request.job_id, &digest)
            .await
            .unwrap_err();
        assert_eq!(promote_refused(&again).unwrap().reason, "not_ready");
        assert_eq!(
            live_prompt(&harness).await.as_deref(),
            Some(BASELINE_PROMPT)
        );
    }

    #[tokio::test]
    async fn a_revert_is_refused_once_the_target_moved_and_the_edit_survives() {
        let (harness, request) = accepting_harness("revert-moved").await;
        let digest = checkpoint_digest(&harness, &request.job_id).await;
        let promotion = promote(harness.access(), harness.owner(), &request.job_id, &digest)
            .await
            .unwrap();
        harness.install(vec![operator_edit(harness.owner())]).await;

        let error = revert(
            harness.access(),
            harness.owner(),
            &request.job_id,
            &promotion.target_digest,
        )
        .await
        .unwrap_err();
        assert_eq!(promote_refused(&error).unwrap().reason, "target_moved");
        assert_eq!(
            live_prompt(&harness).await.as_deref(),
            Some("An operator wrote this by hand.\n")
        );
        assert_eq!(state(&harness, &request.job_id).await, JobState::Promoted);
    }

    #[tokio::test]
    async fn a_closure_edited_after_the_freeze_refuses_the_promotion_and_marks_the_job_stale() {
        let (harness, request) = accepting_harness("promote-stale").await;
        let digest = checkpoint_digest(&harness, &request.job_id).await;
        harness
            .install(vec![(
                Collection::Agent,
                json!({
                    "agent_slot": "monitor",
                    "node_did": harness.owner(),
                    "display_name": "Monitor, renamed",
                    "context_id": "monitor-context",
                    "inference_profile_id": "local",
                }),
            )])
            .await;

        let error = promote(harness.access(), harness.owner(), &request.job_id, &digest)
            .await
            .unwrap_err();
        let refusal = promote_refused(&error).unwrap_or_else(|| panic!("{error:#}"));
        assert_eq!(refusal.reason, "stale_closure");
        assert!(refusal.detail.contains("Agent"), "{}", refusal.detail);
        assert_eq!(
            live_prompt(&harness).await.as_deref(),
            Some(BASELINE_PROMPT)
        );
        assert_eq!(state(&harness, &request.job_id).await, JobState::Stale);
    }

    #[tokio::test]
    async fn a_closure_edit_committed_between_the_check_and_the_write_refuses_the_promotion() {
        let (harness, request) = accepting_harness("promote-raced").await;
        let digest = checkpoint_digest(&harness, &request.job_id).await;
        let mut tools = read_closure(&harness)
            .await
            .into_iter()
            .find_map(|(collection, value)| {
                (collection == Collection::Tools && value["tools_id"] == "monitor-tools")
                    .then_some(value)
            })
            .expect("selected Tools");
        tools["display_name"] = json!("Monitor tools, renamed");
        let gate = std::sync::Arc::new((tokio::sync::Notify::new(), tokio::sync::Notify::new()));
        let promotion = BETWEEN_CHECK_AND_WRITE.scope(
            std::sync::Arc::clone(&gate),
            promote(harness.access(), harness.owner(), &request.job_id, &digest),
        );
        let edit = async {
            gate.0.notified().await;
            harness.install(vec![(Collection::Tools, tools)]).await;
            gate.1.notify_one();
        };
        let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(60), async {
            tokio::join!(promotion, edit)
        })
        .await
        .expect("the promotion and the edit finished");

        let error = result.unwrap_err();
        let refusal = promote_refused(&error).unwrap_or_else(|| panic!("{error:#}"));
        assert_eq!(refusal.reason, "stale_closure");
        assert!(refusal.detail.contains("Tools"), "{}", refusal.detail);
        assert_eq!(
            live_prompt(&harness).await.as_deref(),
            Some(BASELINE_PROMPT)
        );
        assert_eq!(state(&harness, &request.job_id).await, JobState::Stale);
        let job = load_job(harness.access(), harness.owner(), &request.job_id)
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(
                job.journal.last(),
                Some(JournalEntry::PromotionRefused { drifted })
                    if drifted.iter().any(|document| document.collection == "Tools"
                        && document.id == "monitor-tools")
            ),
            "{:?}",
            job.journal.last()
        );
    }

    #[tokio::test]
    async fn held_user_promotion_and_revert_match_native_job_permissions() -> anyhow::Result<()> {
        use crate::store::config_client::GraphqlEndpoint;
        use crate::NodeIdentity;

        let modeled =
            &crate::lean_vocab_test::lean_contract_snapshot().identity_optimization_promotion_cases;
        let expected = |name: &str| {
            modeled
                .iter()
                .find(|case| case["name"] == name)
                .expect("modeled case")
        };
        let (harness, request) = accepting_harness("native-promotion").await;
        let digest = checkpoint_digest(&harness, &request.job_id).await;
        let keys = tempfile::tempdir()?;
        let outsider = crate::KeyIdentity::load_or_create(keys.path().join("outsider.key"), None)?;
        let endpoint = match harness.access() {
            ConfigAccess::Graphql(endpoint) => endpoint.url(),
            _ => panic!("held evaluator must use HTTP"),
        };
        let foreign = ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(endpoint, outsider.did())
                .with_client_signing_capability(outsider.client_signing_capability()?)?
                .with_client_signed_agent_creates(),
        );
        let hidden = promote(&foreign, harness.owner(), &request.job_id, &digest)
            .await
            .unwrap_err();
        let missing = promote(&foreign, harness.owner(), "missing-job", &digest)
            .await
            .unwrap_err();
        assert_eq!(
            promote_refused(&hidden).map(|error| error.reason),
            Some("unknown_job")
        );
        assert_eq!(
            promote_refused(&hidden).map(|error| error.reason),
            promote_refused(&missing).map(|error| error.reason)
        );
        let anonymous = ConfigAccess::graphql(endpoint);
        let no_actor = promote(&anonymous, harness.owner(), &request.job_id, &digest)
            .await
            .unwrap_err();
        assert_eq!(
            promote_refused(&no_actor).map(|error| error.reason),
            Some("actor_required")
        );
        assert_eq!(expected("missing-actor")["expected_target"], "before");

        let owner = harness.owner().to_owned();
        let node_doc = harness
            .access()
            .transact_readonly("test.promotion.node", |txn| {
                let owner = &owner;
                Box::pin(async move {
                    let (doc_id, _) =
                        crate::store::config_client::read_desired_state_record_in_txn(
                            txn,
                            Collection::Node,
                            owner,
                            owner,
                        )
                        .await?
                        .expect("user-owned Node");
                    Ok::<_, anyhow::Error>(doc_id)
                })
            })
            .await?;
        harness
            .access()
            .add_actor_relationship(
                "Node",
                &node_doc,
                "administrator",
                &identity::Did::new(outsider.did())?,
            )
            .await?;
        harness
            .launching
            .grant_node_permissions(
                outsider.did(),
                &[
                    crate::store::config_client::NodePermission::CollectionGet,
                    crate::store::config_client::NodePermission::DocumentRead,
                    crate::store::config_client::NodePermission::DocumentUpdate,
                ],
            )
            .await?;
        let before_prompt = live_prompt(&harness).await.expect("target prompt");
        let before_job = load_job(harness.access(), harness.owner(), &request.job_id)
            .await?
            .unwrap();
        let retained = checkpoint(&before_job.journal).expect("ready checkpoint");

        // Establish that the foreign caller can stage the target update, then
        // abort that independent probe before exercising the joint transaction.
        let probe = foreign
            .transact_write("test.promotion.target_probe", |txn| {
                let origin = &before_job.origin;
                let text = &retained.text;
                Box::pin(async move {
                    let live = capture_closure(txn, &origin.owner).await?;
                    let (plan, _) = promotion_plan(origin, &live, text)?;
                    apply_desired_state_plan(txn, &plan).await?;
                    Err::<(), anyhow::Error>(anyhow::anyhow!("target probe rollback"))
                })
            })
            .await
            .unwrap_err();
        assert_eq!(probe.to_string(), "target probe rollback");
        assert_eq!(
            live_prompt(&harness).await.as_deref(),
            Some(before_prompt.as_str())
        );
        assert!(promote(&foreign, harness.owner(), &request.job_id, &digest)
            .await
            .is_err());
        let denied = expected("reader-cannot-update-job");
        assert_eq!(denied["expected_target"], "before");
        assert_eq!(
            live_prompt(&harness).await.as_deref(),
            Some(before_prompt.as_str())
        );
        let after_denial = load_job(harness.access(), harness.owner(), &request.job_id)
            .await?
            .unwrap();
        assert_eq!(after_denial.journal, before_job.journal);
        assert_eq!(denied["expected_journal"].as_array().unwrap().len(), 0);

        let promotion =
            promote(harness.access(), harness.owner(), &request.job_id, &digest).await?;
        let granted = expected("held-user-promote");
        assert_eq!(granted["expected_target"], "after");
        assert_eq!(
            live_prompt(&harness).await.as_deref(),
            Some(CANDIDATE_PROMPT)
        );
        let promoted = load_job(harness.access(), harness.owner(), &request.job_id)
            .await?
            .unwrap();
        assert_eq!(
            promoted.journal.len(),
            before_job.journal.len() + granted["expected_journal"].as_array().unwrap().len()
        );
        assert!(
            matches!(promoted.journal.last(), Some(JournalEntry::Promoted { by, .. }) if by == &harness.evaluator_did())
        );

        assert!(revert(
            &foreign,
            harness.owner(),
            &request.job_id,
            &promotion.target_digest
        )
        .await
        .is_err());
        assert_eq!(
            live_prompt(&harness).await.as_deref(),
            Some(CANDIDATE_PROMPT)
        );
        assert_eq!(
            load_job(harness.access(), harness.owner(), &request.job_id)
                .await?
                .unwrap()
                .journal,
            promoted.journal
        );
        revert(
            harness.access(),
            harness.owner(),
            &request.job_id,
            &promotion.target_digest,
        )
        .await?;
        let reverted = expected("held-user-revert");
        assert_eq!(reverted["expected_target"], "after");
        assert_eq!(
            live_prompt(&harness).await.as_deref(),
            Some(before_prompt.as_str())
        );
        let final_job = load_job(harness.access(), harness.owner(), &request.job_id)
            .await?
            .unwrap();
        assert_eq!(
            final_job.journal.len(),
            promoted.journal.len() + reverted["expected_journal"].as_array().unwrap().len()
        );
        assert!(
            matches!(final_job.journal.last(), Some(JournalEntry::Reverted { by }) if by == &harness.evaluator_did())
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_wrong_digest_and_an_unready_job_are_each_refused() {
        let (harness, request) = accepting_harness("promote-refusals").await;
        let wrong = promote(
            harness.access(),
            harness.owner(),
            &request.job_id,
            "sha256:not-it",
        )
        .await
        .unwrap_err();
        assert_eq!(promote_refused(&wrong).unwrap().reason, "wrong_digest");
        assert_eq!(
            live_prompt(&harness).await.as_deref(),
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
            rejecting.owner(),
            &unready.job_id,
            "sha256:anything",
        )
        .await
        .unwrap_err();
        assert_eq!(promote_refused(&error).unwrap().reason, "not_ready");
    }

    #[tokio::test]
    async fn an_unknown_job_and_an_edited_checkpoint_pack_are_each_refused() {
        let (harness, request) = accepting_harness("promote-tampered").await;
        let digest = checkpoint_digest(&harness, &request.job_id).await;

        let unknown = promote(harness.access(), harness.owner(), "no-such-job", &digest)
            .await
            .unwrap_err();
        assert_eq!(promote_refused(&unknown).unwrap().reason, "unknown_job");

        let job = load_job(harness.access(), harness.owner(), &request.job_id)
            .await
            .unwrap()
            .unwrap();
        let round = checkpoint(&job.journal).unwrap().round;
        let prompt = crate::eval::optimization::driver::candidate_dir(
            &request.jobs_dir,
            &request.job_id,
            round,
        )
        .join("agents/monitor/system_prompt.md");
        assert!(prompt.exists(), "{}", prompt.display());
        std::fs::write(&prompt, "Edited on disk after the job accepted it.\n").unwrap();

        let error = promote(harness.access(), harness.owner(), &request.job_id, &digest)
            .await
            .unwrap_err();
        assert_eq!(promote_refused(&error).unwrap().reason, "rebuild_mismatch");
        assert_eq!(
            live_prompt(&harness).await.as_deref(),
            Some(BASELINE_PROMPT)
        );
        assert_eq!(
            state(&harness, &request.job_id).await,
            JobState::ReadyToPromote
        );
    }

    #[test]
    fn the_model_facing_config_tool_has_no_optimization_surface() {
        for name in crate::tools::builtin::self_config::SELF_CONFIG_TOOL_NAMES {
            assert!(
                !name.contains("optim"),
                "the self-config tool set names {name}"
            );
        }
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tools/builtin/self_config");
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

    #[test]
    fn no_agent_tool_surface_reaches_the_eval_operator_verbs() {
        fn sources(dir: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    sources(&path, found);
                } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
                    found.push(path);
                }
            }
        }
        for name in crate::tools::builtin::self_config::SELF_CONFIG_TOOL_NAMES {
            for verb in ["cancel", "invalidate", "rm", "gc"] {
                assert!(
                    !(name.contains("eval") && name.contains(verb)),
                    "the self-config tool set names {name}"
                );
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        for surface in [
            "tools/builtin/self_config",
            "tools/native",
            "tools/mcp/meta",
            "tools/background/status",
            "tools/surface",
        ] {
            sources(&root.join(surface), &mut files);
        }
        for module in [
            "tools/native.rs",
            "tools/mcp/meta.rs",
            "tools/background/status.rs",
            "tools/call/control.rs",
        ] {
            files.push(root.join(module));
        }
        assert!(
            files.len() > 10,
            "expected the agent tool surfaces under {}",
            root.display()
        );
        for path in files {
            let source = std::fs::read_to_string(&path).unwrap();
            for word in [
                "runner::request_cancel",
                "CANCEL_MARKER",
                "invalidate_run",
                "eval::runner::run_dir",
                "run_finished",
                "held_runs",
                "referenced_run_ids",
            ] {
                assert!(
                    !source.contains(word),
                    "{} mentions {word}; the eval operator verbs are not an agent's",
                    path.display()
                );
            }
        }
    }
}
