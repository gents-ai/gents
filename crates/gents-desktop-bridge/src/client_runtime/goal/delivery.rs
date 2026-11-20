use anyhow::{Context, Result};
use gents_protocol::goal_control::{GoalOperation, GoalRevision, SessionGoalControlReceipt};
use gents_protocol::request_input::RequestInput;
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
    #[allow(clippy::too_many_arguments)]
    pub async fn control_session_goal_correlated(
        &self,
        command_id: &str,
        node_did: &str,
        session_id: &str,
        session_doc_id: Option<String>,
        expected: Option<GoalRevision>,
        operation: GoalOperation,
        agent_did: Option<&str>,
        input: RequestInput,
        resume_reference: Option<&GoalSubmissionReference>,
    ) -> Result<SessionGoalControlReceipt> {
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
            &gents_protocol::authored_submission::AuthoredSubmission::Goal(Box::new(
                gents_protocol::authored_submission::AuthoredGoal {
                    node_did: node_did.to_owned(),
                    command_id: command_id.to_owned(),
                    session_id: session_id.to_owned(),
                    session_doc_id: session_doc_id.clone(),
                    expected: expected.clone(),
                    operation: operation.clone(),
                    agent_did: agent_did.map(str::to_owned),
                    input: input.clone(),
                },
            )),
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
        let agent_did = input.agent_did.as_deref();
        let session_doc_id = input.session_doc_id.clone();
        let expected = input.expected.clone();
        let operation = input.operation.clone();
        let input = input.input.clone();
        let authored =
            serde_json::to_vec(&(&session_doc_id, &expected, &operation, agent_did, &input))?;
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
            let (_, command) = self
                .prepare_session_goal_command(
                    node_did,
                    session_id,
                    session_doc_id,
                    expected,
                    operation,
                    agent_did,
                    input,
                    command_id.into(),
                )
                .await?;
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
