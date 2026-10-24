use anyhow::Result;
use gents_protocol::error_chain::ErrorChain;

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
    stale_expectation(error.as_ref()).map(|stale| {
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

#[cfg(test)]
tokio::task_local! {
    static BETWEEN_CHECK_AND_WRITE: std::sync::Arc<(tokio::sync::Notify, tokio::sync::Notify)>;
}

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

    let origin = &job.origin;
    let checked = verified_checkpoint(origin, job_id, &retained).and_then(|path| {
        let pack = materialize_pack(
            &path,
            &origin.owner,
            &origin.subject.agent_id,
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
        return Err(refused(
            "rebuild_mismatch",
            format!("{}", ErrorChain(error.as_ref())),
        ));
    }

    let applied = access
        .transact_write("optimization.promote", |txn| {
            let (job, text, by) = (&job, &retained.text, by);
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
                    by: by.to_owned(),
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
            tracing::info!(job_id, by, "optimization checkpoint promoted");
            Ok(promotion)
        }
        Err(error) => {
            let Some(drifted) = drift_of(&error) else {
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
        .transact_write("optimization.revert", |txn| {
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
        Err(error) if stale_expectation(error.as_ref()).is_some() => Err(refused(
            "target_moved",
            format!("{}", ErrorChain(error.as_ref())),
        )),
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
        accepting_harness, accepting_task_harness, rejecting_harness, Harness, BASELINE_PROMPT,
        CANDIDATE_PROMPT, CANDIDATE_TEMPLATE,
    };
    use crate::optimization::job::load_job;
    use crate::optimization::subject::tests::FIXTURE_TEMPLATE;
    use crate::optimization::target::TargetField;
    use crate::Collection;
    use serde_json::json;

    async fn read_closure(access: &ConfigAccess) -> Closure {
        access
            .transact_write("test.read_closure", |txn| {
                Box::pin(async move { capture_closure(txn, OWNER).await })
            })
            .await
            .unwrap()
    }

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

    async fn live_template(access: &ConfigAccess) -> Option<String> {
        read_closure(access)
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
        let job = load_job(harness.access(), OWNER, &request.job_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(job.origin.target.field, TargetField::TaskPromptTemplate);
        assert_eq!(job.origin.target.id, "plan");
        let before = read_closure(harness.access()).await;

        let promotion = promote(harness.access(), OWNER, &request.job_id, &digest, OWNER)
            .await
            .unwrap();
        assert_eq!(promotion.previous_text, FIXTURE_TEMPLATE);
        assert_eq!(
            live_template(harness.access()).await.as_deref(),
            Some(CANDIDATE_TEMPLATE)
        );
        assert_eq!(
            live_prompt(harness.access()).await.as_deref(),
            Some(BASELINE_PROMPT),
            "the context is untouched"
        );
        let after = read_closure(harness.access()).await;
        assert_eq!(after.len(), before.len());

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
            live_template(harness.access()).await.as_deref(),
            Some(FIXTURE_TEMPLATE)
        );
        assert_eq!(state(&harness, &request.job_id).await, JobState::Reverted);
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
                "node_did": OWNER,
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

        assert_eq!(
            tree(&job_dir(&request.jobs_dir, &request.job_id)),
            on_disk,
            "promote writes no scratch directory"
        );
    }

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

        harness.install(vec![operator_edit()]).await;
        let error = apply(harness.access(), &plan).await.unwrap_err();
        let stale = stale_expectation(error.as_ref())
            .unwrap_or_else(|| panic!("{}", ErrorChain(error.as_ref())));
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
            .transact_write("test.apply_plan", |txn| {
                Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
            })
            .await
    }

    #[tokio::test]
    async fn an_operator_edit_of_the_target_before_promotion_refuses_it_and_survives() {
        let (harness, request) = accepting_harness("promote-edited").await;
        let digest = checkpoint_digest(&harness, &request.job_id).await;
        harness.install(vec![operator_edit()]).await;

        let error = promote(harness.access(), OWNER, &request.job_id, &digest, OWNER)
            .await
            .unwrap_err();
        let refusal =
            promote_refused(&error).unwrap_or_else(|| panic!("{}", ErrorChain(error.as_ref())));
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
        assert_eq!(promote_refused(&again).unwrap().reason, "not_ready");
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
                    "agent_id": "monitor",
                    "node_did": OWNER,
                    "display_name": "Monitor, renamed",
                    "context_id": "monitor-context",
                    "inference_profile_id": "local",
                }),
            )])
            .await;

        let error = promote(harness.access(), OWNER, &request.job_id, &digest, OWNER)
            .await
            .unwrap_err();
        let refusal =
            promote_refused(&error).unwrap_or_else(|| panic!("{}", ErrorChain(error.as_ref())));
        assert_eq!(refusal.reason, "stale_closure");
        assert!(refusal.detail.contains("Agent"), "{}", refusal.detail);
        assert_eq!(
            live_prompt(harness.access()).await.as_deref(),
            Some(BASELINE_PROMPT)
        );
        assert_eq!(state(&harness, &request.job_id).await, JobState::Stale);
    }

    #[tokio::test]
    async fn a_closure_edit_committed_between_the_check_and_the_write_refuses_the_promotion() {
        let (harness, request) = accepting_harness("promote-raced").await;
        let digest = checkpoint_digest(&harness, &request.job_id).await;
        let gate = std::sync::Arc::new((tokio::sync::Notify::new(), tokio::sync::Notify::new()));
        let promotion = BETWEEN_CHECK_AND_WRITE.scope(
            std::sync::Arc::clone(&gate),
            promote(harness.access(), OWNER, &request.job_id, &digest, OWNER),
        );
        let node = harness
            .access()
            .local_node()
            .expect("the harness is embedded");
        let edit = async {
            gate.0.notified().await;
            let response = node
                .execute(
                    r#"mutation { update_Tools(filter: {tools_id: {_eq: "monitor-tools"}}, input: {display_name: "Monitor tools, renamed"}) { _docID } }"#,
                )
                .await;
            assert!(!response.has_errors(), "{:?}", response.errors);
            gate.1.notify_one();
        };
        let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(60), async {
            tokio::join!(promotion, edit)
        })
        .await
        .expect("the promotion and the edit finished");

        let error = result.unwrap_err();
        let refusal =
            promote_refused(&error).unwrap_or_else(|| panic!("{}", ErrorChain(error.as_ref())));
        assert_eq!(refusal.reason, "stale_closure");
        assert!(refusal.detail.contains("Tools"), "{}", refusal.detail);
        assert_eq!(
            live_prompt(harness.access()).await.as_deref(),
            Some(BASELINE_PROMPT)
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
                    if drifted.iter().any(|document| document.collection == "Tools"
                        && document.id == "monitor-tools")
            ),
            "{:?}",
            job.journal.last()
        );
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
                .join("agents/monitor/system_prompt.md");
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
        for name in crate::self_config::SELF_CONFIG_TOOL_NAMES {
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
            "self_config",
            "toolset",
            "meta_tools",
            "background_tools",
            "tool_surface",
        ] {
            sources(&root.join(surface), &mut files);
        }
        for module in [
            "toolset.rs",
            "meta_tools.rs",
            "background_tools.rs",
            "tool_control.rs",
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
