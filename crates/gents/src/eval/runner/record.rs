//! The recorder seam: the document operations the loop performs.
//!
//! The loop writes one trial row, its verdicts and its completion, and reads
//! back what a run has already written so a resumed run plans only what it
//! still owes. Naming those four as a trait lets a test crash the loop between
//! any two of them without a fault-injection path in the runtime.
//!
//! A recorder never interprets a failure. Whatever it returns as `Err` is a
//! failure of the record, not of the trial, so the loop propagates it
//! unchanged rather than inventing an outcome for a trial that may well have
//! succeeded.

use std::path::Path;

use anyhow::Result;

use crate::config_client::ConfigAccess;
use crate::document_config::EvalSplit;
use crate::eval::report::{load_report, EvalReport};
use crate::eval::{
    append_verdict, complete_trial, create_trial, load_trials, TrialCompletion, TrialIdentity,
    TrialRecord, VerdictDraft,
};

#[async_trait::async_trait]
pub trait Recorder: Send + Sync {
    async fn create_trial(&self, owner: &str, identity: &TrialIdentity) -> Result<()>;

    async fn append_verdict(
        &self,
        owner: &str,
        split: EvalSplit,
        draft: &VerdictDraft,
    ) -> Result<()>;

    async fn complete_trial(
        &self,
        owner: &str,
        trial_id: &str,
        completion: &TrialCompletion,
    ) -> Result<()>;

    async fn load_trials(&self, owner: &str, run_id: &str) -> Result<Vec<TrialRecord>>;

    /// The run's report as the documents written so far derive it, for the
    /// loop's `report.json`; `None` from a recorder with no documents to
    /// derive it from.
    async fn report(
        &self,
        _owner: &str,
        _runs_dir: &Path,
        _run_id: &str,
    ) -> Result<Option<EvalReport>> {
        Ok(None)
    }
}

/// The recorder every real run uses: the eval documents themselves.
pub struct DocumentRecorder<'a> {
    access: &'a ConfigAccess,
    /// Parallel trials share one report store. Serialize their bookkeeping
    /// transactions to avoid exhausting conflict retries over HTTP; trial
    /// execution and each trial's own database remain concurrent.
    writes: tokio::sync::Mutex<()>,
}

impl<'a> DocumentRecorder<'a> {
    pub fn new(access: &'a ConfigAccess) -> Self {
        Self {
            access,
            writes: tokio::sync::Mutex::new(()),
        }
    }
}

#[async_trait::async_trait]
impl Recorder for DocumentRecorder<'_> {
    async fn create_trial(&self, owner: &str, identity: &TrialIdentity) -> Result<()> {
        let _write = self.writes.lock().await;
        create_trial(self.access, owner, identity).await
    }

    async fn append_verdict(
        &self,
        owner: &str,
        split: EvalSplit,
        draft: &VerdictDraft,
    ) -> Result<()> {
        let _write = self.writes.lock().await;
        append_verdict(self.access, owner, split, draft).await
    }

    async fn complete_trial(
        &self,
        owner: &str,
        trial_id: &str,
        completion: &TrialCompletion,
    ) -> Result<()> {
        let _write = self.writes.lock().await;
        complete_trial(self.access, owner, trial_id, completion).await
    }

    async fn load_trials(&self, owner: &str, run_id: &str) -> Result<Vec<TrialRecord>> {
        load_trials(self.access, owner, run_id).await
    }

    async fn report(
        &self,
        owner: &str,
        runs_dir: &Path,
        run_id: &str,
    ) -> Result<Option<EvalReport>> {
        load_report(self.access, owner, runs_dir, run_id)
            .await
            .map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document_config::EvalTier;
    use crate::eval::{load_verdicts, Anchor, OutcomeKind, TrialUsage};
    use futures::{stream, StreamExt, TryStreamExt};
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn thirty_two_trials_record_over_http_without_losing_completions_or_verdicts() {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = probe.local_addr().unwrap();
        drop(probe);
        let node = Arc::new(
            crate::defra_node::EmbeddedNode::builder()
                .with_http(crate::defra_node::HttpConfig::with_addr(address))
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if tokio::net::TcpStream::connect(address).await.is_ok() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let access = ConfigAccess::graphql(format!("http://{address}/api/v0/graphql"));
        let recorder = DocumentRecorder::new(&access);
        let owner = "did:key:eval-recorder-test";
        let completion = TrialCompletion {
            ended_at: "2026-09-30T00:00:00Z".into(),
            stages: Vec::new(),
            usage: TrialUsage::default(),
            anchor: Anchor {
                terminal_states: Vec::new(),
                requests: 0,
                inference_calls: 0,
            },
            evidence_digest: None,
        };
        stream::iter(0..32u32)
            .map(|i| {
                let recorder = &recorder;
                let completion = &completion;
                async move {
                    let id = format!("trial-{i}");
                    recorder
                        .create_trial(
                            owner,
                            &TrialIdentity {
                                trial_id: id.clone(),
                                run_id: "parallel-http".into(),
                                cell_id: "engineer".into(),
                                case_id: "case".into(),
                                trial_index: i,
                                attempt: 1,
                                trial_node_did: owner.into(),
                                session_id: format!("session-{i}"),
                                seed: i.into(),
                                home_hint: None,
                            },
                        )
                        .await?;
                    recorder
                        .append_verdict(
                            owner,
                            EvalSplit::Train,
                            &VerdictDraft {
                                verdict_id: format!("verdict-{i}"),
                                run_id: "parallel-http".into(),
                                trial_id: id.clone(),
                                stage_id: "stage".into(),
                                check: "captured_rows_count".into(),
                                check_version: "1".into(),
                                tier: EvalTier::Acceptance,
                                kind: OutcomeKind::Passed,
                                provider_reason: None,
                                score_bp: Some(10_000),
                                weight: 1,
                                raw: json!({"reason_code":"in_range","count":1}),
                                feedback: None,
                                regrade_of: None,
                            },
                        )
                        .await?;
                    recorder.complete_trial(owner, &id, completion).await
                }
            })
            .buffer_unordered(32)
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        let trials = recorder.load_trials(owner, "parallel-http").await.unwrap();
        assert_eq!(trials.len(), 32);
        assert!(trials
            .iter()
            .all(|trial| trial.completion.as_ref() == Some(&completion)));
        assert_eq!(
            load_verdicts(&access, owner, "parallel-http")
                .await
                .unwrap()
                .len(),
            32
        );
        node.shutdown().await;
    }
}
