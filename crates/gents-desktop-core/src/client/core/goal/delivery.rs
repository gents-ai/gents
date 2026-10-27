use anyhow::{Context, Result};
use chrono::Utc;
use gents::NodeIdentity;
use gents_protocol::goal_control::{GoalOperation, GoalRevision, SessionGoalControlReceipt};
use gents_protocol::request_input::RequestInput;

use crate::client::mutations::{goal_delivery, submission_journal};
use crate::client::ClientCore;

pub use goal_delivery::Reference as GoalSubmissionReference;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct RetainedGoalSubmission {
    pub node_did: String,
    pub session_id: String,
    pub reference: GoalSubmissionReference,
    pub receipt: Option<SessionGoalControlReceipt>,
    pub acknowledged: bool,
}

fn view(record: &goal_delivery::Record) -> RetainedGoalSubmission {
    RetainedGoalSubmission {
        node_did: record.intent.node_did.clone(),
        session_id: record.intent.session_id.clone(),
        reference: record.reference.clone(),
        receipt: record.receipt.clone(),
        acknowledged: record.acknowledged,
    }
}

impl ClientCore {
    /// This query reads the existing encrypted journal and verifies any recorded
    /// outcome. It does not prepare, sign, allocate a session or publish work.
    pub async fn recover_goal_submission(
        &self,
        node_did: &str,
        command_id: &str,
    ) -> Result<Option<RetainedGoalSubmission>> {
        let mut record = submission_journal::goal::lookup(
            self.node.as_ref(),
            self.node_identity.did(),
            node_did,
            command_id,
        )
        .await?;
        if let Some(retained) = &record {
            if retained.receipt.is_none() {
                let command = retained
                    .prepared
                    .as_ref()
                    .context("Goal signed preparation is unavailable")?;
                let peer = self
                    .peer_record_for_chat_write(node_did, Utc::now())
                    .await?;
                let (signer, _, _) = self.request_authority(node_did, peer.as_ref()).await?;
                if let Some(receipt) = gents::goal::recover_session_goal_control(
                    &self.session_queue_access(node_did),
                    signer.as_ref(),
                    command,
                )
                .await?
                {
                    record = Some(
                        submission_journal::goal::update(
                            &self.node_arc(),
                            &retained.intent,
                            &retained.reference,
                            Some(&receipt),
                        )
                        .await?,
                    );
                }
            }
        }
        if let Some(record) = &record {
            if let Some(receipt) = &record.receipt {
                anyhow::ensure!(
                    self.node_identity
                        .verify(node_did, &receipt.signing_payload()?, &receipt.signature)
                        .await?,
                    "retained Goal outcome has no verified runtime signature"
                );
            }
        }
        Ok(record.as_ref().map(view))
    }

    /// Only an explicit resume publishes the retained preparation. The command
    /// and its first request remain byte-identical, including their expiry.
    pub async fn resume_goal_submission(
        &self,
        node_did: &str,
        reference: &GoalSubmissionReference,
    ) -> Result<SessionGoalControlReceipt> {
        let retained = self
            .recover_goal_submission(node_did, &reference.command_id)
            .await?
            .context("Goal caller preparation is unavailable")?;
        anyhow::ensure!(
            retained.reference == *reference,
            "Goal caller reference changed"
        );
        if let Some(receipt) = retained.receipt {
            return Ok(receipt);
        }
        let record = submission_journal::goal::lookup(
            self.node.as_ref(),
            self.node_identity.did(),
            node_did,
            &reference.command_id,
        )
        .await?
        .context("Goal caller preparation is unavailable")?;
        anyhow::ensure!(
            record.reference == *reference,
            "Goal caller reference changed"
        );
        let (decision, publication) =
            goal_delivery::resume(std::slice::from_ref(&record), &record.intent);
        if let goal_delivery::Decision::Confirmed(_, receipt) = decision {
            anyhow::ensure!(
                self.node_identity
                    .verify(node_did, &receipt.signing_payload()?, &receipt.signature)
                    .await?,
                "retained Goal outcome has no verified runtime signature"
            );
            return Ok(*receipt);
        }
        anyhow::ensure!(
            publication.as_ref() == Some(reference),
            "Goal caller preparation cannot publish"
        );
        let command = record
            .prepared
            .as_ref()
            .context("Goal signed preparation is unavailable")?;
        let peer = self
            .peer_record_for_chat_write(node_did, Utc::now())
            .await?;
        super::ensure_peer_chat_ready_at(node_did, peer.as_ref(), Utc::now())?;
        let (signer, _, _) = self.request_authority(node_did, peer.as_ref()).await?;
        let receipt = gents::goal::submit_prepared_session_goal_control(
            &self.session_queue_access(node_did),
            signer.as_ref(),
            command,
        )
        .await?;
        let settled = submission_journal::goal::update(
            &self.node_arc(),
            &record.intent,
            reference,
            Some(&receipt),
        )
        .await?;
        settled.receipt.context("Goal outcome was not retained")
    }

    /// The caller retains its command and destination identities before this
    /// call. Authored equality covers the complete input and observed revision;
    /// a racing proposal cannot replace the journal winner's signed request.
    #[allow(clippy::too_many_arguments)]
    pub async fn control_session_goal_correlated(
        &self,
        command_id: &str,
        node_did: &str,
        session_id: &str,
        session_doc_id: Option<String>,
        expected: Option<GoalRevision>,
        operation: GoalOperation,
        agent_id: Option<&str>,
        input: RequestInput,
        resume_reference: Option<&GoalSubmissionReference>,
    ) -> Result<SessionGoalControlReceipt> {
        let authored =
            serde_json::to_vec(&(&session_doc_id, &expected, &operation, agent_id, &input))?;
        let intent = goal_delivery::Intent::new(
            self.node_identity.did().into(),
            node_did.into(),
            command_id.into(),
            session_id.into(),
            &authored,
        );
        let existing = submission_journal::goal::recover(self.node.as_ref(), &intent).await?;
        let record = if let Some(record) = existing {
            anyhow::ensure!(
                goal_delivery::query(std::slice::from_ref(&record), &intent)
                    != goal_delivery::Decision::Refused,
                "Goal caller identity is bound to different authored input"
            );
            record
        } else {
            anyhow::ensure!(
                resume_reference.is_none(),
                "Goal caller preparation is unavailable; its delivery remains unconfirmed"
            );
            let (_, command) = self
                .prepare_session_goal_command(
                    node_did,
                    session_id,
                    session_doc_id,
                    expected,
                    operation,
                    agent_id,
                    input,
                    command_id.into(),
                )
                .await?;
            let proposal = goal_delivery::Record::proposal(intent, command)?;
            submission_journal::goal::reserve(&self.node_arc(), &proposal).await?
        };
        anyhow::ensure!(
            resume_reference.is_none_or(|reference| reference == &record.reference),
            "Goal caller reference changed"
        );
        self.resume_goal_submission(node_did, &record.reference)
            .await
    }

    /// The editor calls this only after saving its confirmed workflow. Retire
    /// signed publication bytes while preserving the exact binding and outcome.
    pub async fn acknowledge_goal_submission(
        &self,
        node_did: &str,
        reference: &GoalSubmissionReference,
    ) -> Result<()> {
        let retained = self
            .recover_goal_submission(node_did, &reference.command_id)
            .await?
            .context("Goal caller preparation is unavailable")?;
        anyhow::ensure!(
            retained.reference == *reference && retained.receipt.is_some(),
            "Goal caller acknowledgement has no confirmed binding"
        );
        let record = submission_journal::goal::lookup(
            self.node.as_ref(),
            self.node_identity.did(),
            node_did,
            &reference.command_id,
        )
        .await?
        .context("Goal caller preparation is unavailable")?;
        submission_journal::goal::update(&self.node_arc(), &record.intent, reference, None).await?;
        Ok(())
    }
}
