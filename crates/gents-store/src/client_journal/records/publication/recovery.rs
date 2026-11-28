use std::sync::Arc;

use anyhow::{bail, ensure, Result};

use super::{lookup, replace, Record};
use crate::config_client::host_grant::HostCredential;
use crate::config_client::{
    recovery_disposition, ConfigAccess, ConfigRelationshipGrant, HostReading, PublicationStage,
    PublicationStatus, RecoveryDisposition,
};

/// An Agent creation recovery could not finish.
#[derive(Debug, thiserror::Error)]
pub enum HostRecoveryError {
    /// `Identity.ClientAgentJournal.RecoveryDisposition.absentHost`.
    #[error("Agent {agent_did} (document {agent_doc}) is not placed on any device: its host grant to {node_did} was never confirmed and its approval lapsed, so it is not sent again. Place the Agent on a device to finish its creation.")]
    NotPlaced {
        agent_did: String,
        agent_doc: String,
        node_did: String,
    },
}

/// Settle `client`'s retained publication of `agent_did` on `node_did`
/// before it publishes again (`Identity.ClientAgentJournal.recoveryDisposition`).
pub async fn ensure_recovered(
    node: &Arc<defra_node::Node>,
    access: &ConfigAccess,
    client: &str,
    node_did: &str,
    agent_did: &str,
) -> Result<()> {
    super::super::require_client_signer(node, client)?;
    let actor_matches = access.actor_did().as_deref() == Some(client);
    ensure!(actor_matches, "Agent publication actor changed");
    let record = lookup(node, client, node_did, agent_did).await?;
    let binding_matches = record.as_ref().is_some_and(|record| {
        record.journal.client == client
            && record.journal.node_did == node_did
            && record.journal.agent_did == agent_did
    });
    let now = u64::try_from(access.home_now().await.timestamp()).unwrap_or_default();
    let token_valid = record
        .as_ref()
        .and_then(|record| record.host_credential.as_ref())
        .is_some_and(|credential| credential.holds_at(now));
    let reading = match &record {
        Some(record) if binding_matches && record.journal.reads_host(token_valid) => {
            read_host(access, record).await?
        }
        _ => HostReading::Absent,
    };
    let disposition = recovery_disposition(
        actor_matches,
        record.as_ref().map(|record| &record.journal),
        binding_matches,
        token_valid,
        reading,
    );
    let Some(record) = record else {
        return Ok(());
    };
    let journal = &record.journal;
    match disposition {
        RecoveryDisposition::Proceed => Ok(()),
        RecoveryDisposition::BindingMismatch => bail!("retained Agent publication identity changed"),
        RecoveryDisposition::CommitUnknown => bail!("Agent {agent_did} creation for document {} has an unknown outcome; inspect its retained publication before repair", journal.agent_doc),
        RecoveryDisposition::ExplicitRepair => bail!("Agent {agent_did} document {} has unfinished relationships (parent: {:?}, executor: {:?}); current parent and ownership bindings cannot be verified atomically, so inspect and repair them explicitly", journal.agent_doc, journal.parent, journal.executor),
        RecoveryDisposition::ResendHost => {
            let credential = record
                .host_credential
                .clone()
                .filter(|_| token_valid);
            resend(node, access, client, record, token_valid, credential).await
        }
        RecoveryDisposition::ObservedHost | RecoveryDisposition::SupersededHost => {
            let mut next = record.clone();
            next.journal.observe_host(reading);
            replace(node, &[record], &[next]).await
        }
        RecoveryDisposition::AbsentHost => Err(HostRecoveryError::NotPlaced {
            agent_did: agent_did.to_owned(),
            agent_doc: journal.agent_doc.clone(),
            node_did: node_did.to_owned(),
        }
        .into()),
    }
}

/// Send the `host` grant again: a pending one only under its retained
/// delegation, which the provider applies once.
async fn resend(
    node: &Arc<defra_node::Node>,
    access: &ConfigAccess,
    client: &str,
    record: Record,
    token_valid: bool,
    credential: Option<HostCredential>,
) -> Result<()> {
    let mut next = record.clone();
    if next.journal.host == PublicationStatus::Pending {
        next.journal.resume_attempt(token_valid);
    } else {
        next.journal.record_attempt(PublicationStage::Host, true);
    }
    ensure!(
        next.journal.may_dispatch(PublicationStage::Host),
        "Agent {} host grant cannot be sent again",
        record.journal.agent_did
    );
    replace(node, &[record], std::slice::from_ref(&next)).await?;
    next.journal.dispatch(PublicationStage::Host);
    let sent = next.clone();
    match (&next.host, &credential) {
        (ConfigRelationshipGrant::Actor { doc_id, target, .. }, Some(credential)) => {
            access.grant_host_bound(doc_id, target, credential).await?
        }
        _ => {
            access
                .complete_relationship_grants(client, std::slice::from_ref(&next.host))
                .await?
        }
    }
    next.journal
        .finish_attempt(PublicationStage::Host, true, false);
    replace(node, &[sent], &[next]).await
}

/// What the Agent's `host` relationships show of this publication's Node.
/// External premise (DefraDB `acp_ops`): a Vera home answers whether one actor
/// holds a relation but lists no relationships, so on a home whose grant was
/// retained under a request-bound delegation another Node's `host` reads as
/// none, and the Agent is reported rather than taken as moved.
async fn read_host(access: &ConfigAccess, record: &Record) -> Result<HostReading> {
    let doc = &record.journal.agent_doc;
    let node = identity::Did::new(&record.journal.node_did)?;
    if access
        .observe_actor_relationship("Agent", doc, "host", &node)
        .await?
    {
        return Ok(HostReading::ThisNode);
    }
    if record.host_credential.is_some() {
        return Ok(HostReading::Absent);
    }
    let moved = access
        .list_document_relationships("Agent", doc)
        .await?
        .iter()
        .any(|relationship| relationship.relation == "host");
    Ok(if moved {
        HostReading::OtherNode
    } else {
        HostReading::Absent
    })
}
