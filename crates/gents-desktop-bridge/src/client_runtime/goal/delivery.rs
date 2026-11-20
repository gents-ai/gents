use anyhow::{Context, Result};
use gents_protocol::authored_submission::AuthoredGoal;
use gents_protocol::goal_control::SessionGoalControlReceipt;
use gents_store::identity::NodeIdentity;

use crate::client_runtime::ClientRuntime;
use gents_protocol::goal_control::GoalSubmissionReference;
use gents_store::client_journal::{goal_delivery, records};

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

impl ClientRuntime {
    /// This query reads the existing encrypted journal and verifies any recorded
    /// outcome. It does not prepare, sign, allocate a session or publish work.
    pub async fn recover_goal_submission(
        &self,
        node_did: &str,
        command_id: &str,
    ) -> Result<Option<RetainedGoalSubmission>> {
        let mut record =
            records::goal::lookup(&self.node, self.node_identity.did(), node_did, command_id)
                .await?;
        if let Some(retained) = &record {
            if retained.receipt.is_none() {
                let command = retained
                    .prepared
                    .as_ref()
                    .context("Goal signed preparation is unavailable")?;
                let peer = self.connection_for_write(node_did)?;
                let (signer, _, _) = self.request_authority(node_did, peer.as_ref()).await?;
                if let Some(receipt) = gents_automation::goal::recover_session_goal_control(
                    &self.session_queue_access(node_did)?,
                    signer.as_ref(),
                    command,
                )
                .await?
                {
                    record = Some(
                        records::goal::update(
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
                        .verify(
                            &receipt.agent_did,
                            &receipt.signing_payload()?,
                            &receipt.signature
                        )
                        .await?,
                    "retained Goal outcome has no verified agent signature"
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
        let record = records::goal::lookup(
            &self.node,
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
                    .verify(
                        &receipt.agent_did,
                        &receipt.signing_payload()?,
                        &receipt.signature
                    )
                    .await?,
                "retained Goal outcome has no verified agent signature"
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
        let peer = self.connection_for_write(node_did)?;
        super::ensure_runtime_connection(node_did, peer.as_ref())?;
        let (signer, _, _) = self.request_authority(node_did, peer.as_ref()).await?;
        let receipt = gents_automation::goal::submit_prepared_session_goal_control(
            &self.session_queue_access(node_did)?,
            signer.as_ref(),
            command,
        )
        .await?;
        let settled =
            records::goal::update(&self.node_arc(), &record.intent, reference, Some(&receipt))
                .await?;
        settled.receipt.context("Goal outcome was not retained")
    }

    /// The caller retains its command and destination identities before this
    /// call. Authored equality covers the complete input and observed revision;
    /// a racing proposal cannot replace the journal winner's signed request.
    pub async fn control_session_goal_correlated(
        &self,
        goal: AuthoredGoal,
        resume_reference: Option<&GoalSubmissionReference>,
    ) -> Result<SessionGoalControlReceipt> {
        let node_did = goal.node_did.clone();
        let command_id = goal.command_id.clone();
        let node_did = node_did.as_str();
        let command_id = command_id.as_str();
        if let Some(reference) = resume_reference {
            let retained =
                records::goal::lookup(&self.node, self.node_identity.did(), node_did, command_id)
                    .await?
                    .context(
                        "Goal caller preparation is unavailable; its delivery remains unconfirmed",
                    )?;
            anyhow::ensure!(
                retained.reference == *reference,
                "Goal caller reference changed"
            );
        }
        super::ensure_runtime_connection(node_did, self.connection_for_write(node_did)?.as_ref())?;
        let queued = gents_store::client_journal::outbox::reserve(
            &self.node,
            self.node_identity.did(),
            &gents_protocol::authored_submission::AuthoredSubmission::Goal(Box::new(goal)),
        )
        .await?;
        if !queued.pending {
            return self
                .recover_goal_submission(node_did, command_id)
                .await?
                .and_then(|retained| retained.receipt)
                .context("the acknowledged Goal has no verified receipt");
        }
        self.require_reachable(&queued)?;
        let _delivery = self.delivery_lock.lock().await;
        while let Some(next) = gents_store::client_journal::outbox::next(
            &self.node,
            self.node_identity.did(),
            node_did,
            None,
        )
        .await?
        {
            self.deliver_authored_record(&next).await?;
            if next.submission_id == command_id {
                break;
            }
        }
        self.recover_goal_submission(node_did, command_id)
            .await?
            .and_then(|retained| retained.receipt)
            .context("the acknowledged Goal has no verified receipt")
    }

    pub(in crate::client_runtime) async fn publish_authored_goal(
        &self,
        queued: &gents_store::client_journal::outbox::Record,
    ) -> Result<SessionGoalControlReceipt> {
        let gents_protocol::authored_submission::AuthoredSubmission::Goal(input) =
            queued.authored()?
        else {
            anyhow::bail!("the authored record is not a Goal");
        };
        let node_did = input.node_did.as_str();
        let command_id = input.command_id.as_str();
        let session_id = input.session_id.as_str();
        let authored = serde_json::to_vec(&(
            &input.session_doc_id,
            &input.expected,
            &input.operation,
            input.agent_did.as_deref(),
            &input.input,
        ))?;
        let intent = goal_delivery::Intent::new(
            self.node_identity.did().into(),
            node_did.into(),
            command_id.into(),
            session_id.into(),
            &authored,
        );
        let existing = records::goal::recover(&self.node, &intent).await?;
        let record = if let Some(record) = existing {
            anyhow::ensure!(
                goal_delivery::query(std::slice::from_ref(&record), &intent)
                    != goal_delivery::Decision::Refused,
                "Goal caller identity is bound to different authored input"
            );
            record
        } else {
            let (_, command) = self.prepare_session_goal_command(*input.clone()).await?;
            let proposal = goal_delivery::Record::proposal(intent, command)?;
            records::goal::reserve(&self.node_arc(), &proposal).await?
        };
        let receipt = self
            .resume_goal_submission(node_did, &record.reference)
            .await?;
        Ok(receipt)
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
        let record = records::goal::lookup(
            &self.node,
            self.node_identity.did(),
            node_did,
            &reference.command_id,
        )
        .await?
        .context("Goal caller preparation is unavailable")?;
        records::goal::update(&self.node_arc(), &record.intent, reference, None).await?;
        Ok(())
    }
}
