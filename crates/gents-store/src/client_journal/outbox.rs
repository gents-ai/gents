use std::sync::Arc;

use gents_protocol::{
    authored_submission::AuthoredSubmission,
    document_query::{
        fields::{client_authored_submission as fields, client_outbox_sequence as sequence},
        Create, Direction, Filter, Input, Query, Rendered, Selection, Update,
    },
};
use serde::{Deserialize, Serialize};

use crate::config_client::{
    ConfigAccess, ConfigApplyTxn, ConfigClientError, IdempotentTransactionRetry, TransactionFailure,
};

#[derive(Debug, thiserror::Error)]
pub enum OutboxError {
    #[error(fmt = gents_protocol::error_chain::held)]
    Access(#[from] ConfigClientError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(fmt = gents_protocol::error_chain::held)]
    Response(#[from] crate::graphql::GraphqlResponseError),
    #[error(transparent)]
    Identity(#[from] identity::Error),
    #[error("the authored submission is bound to different input, client or runtime")]
    Binding,
    #[error("the client outbox contains an invalid record")]
    Record,
    #[error("the client outbox sequence is exhausted")]
    SequenceExhausted,
}

impl TransactionFailure for OutboxError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Access(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reservation {
    Insert,
    Retained,
    Refused,
}

pub fn reserve_decision(local_signer: bool, valid: bool, exists: bool, same: bool) -> Reservation {
    if !local_signer || !valid {
        Reservation::Refused
    } else if !exists {
        Reservation::Insert
    } else if same {
        Reservation::Retained
    } else {
        Reservation::Refused
    }
}

pub fn prepare_decision(binding: bool, pending: bool, exists: bool, same: bool) -> Reservation {
    if !binding {
        Reservation::Refused
    } else if !pending {
        Reservation::Retained
    } else if !exists {
        Reservation::Insert
    } else if same {
        Reservation::Retained
    } else {
        Reservation::Refused
    }
}

pub fn acknowledges(binding: bool, runtime_confirmed: bool) -> bool {
    binding && runtime_confirmed
}

pub fn select<'a>(pending: &'a [String], steer: Option<&str>) -> Option<&'a str> {
    match steer {
        None => pending.first().map(String::as_str),
        Some(id) => pending
            .iter()
            .find(|pending| pending.as_str() == id)
            .map(String::as_str),
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Record {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    pub submission_id: String,
    pub client_did: String,
    pub node_did: String,
    pub ordinal: i64,
    pub digest: String,
    pub pending: bool,
    pub payload: Option<String>,
    pub prepared: Option<String>,
}

impl Record {
    pub fn authored(&self) -> Result<AuthoredSubmission, OutboxError> {
        let payload = self.payload.as_deref().ok_or(OutboxError::Record)?;
        if blake3::hash(payload.as_bytes()).to_hex().as_str() != self.digest {
            return Err(OutboxError::Binding);
        }
        let authored: AuthoredSubmission = serde_json::from_str(payload)?;
        if authored.submission_id() != Some(self.submission_id.as_str())
            || authored.node_did() != self.node_did
        {
            return Err(OutboxError::Binding);
        }
        Ok(authored)
    }
}

fn selection() -> Selection<fields::Doc> {
    Selection::doc_id()
        .field(fields::SUBMISSION_ID)
        .field(fields::CLIENT_DID)
        .field(fields::NODE_DID)
        .field(fields::ORDINAL)
        .field(fields::DIGEST)
        .field(fields::PENDING)
        .field(fields::PAYLOAD)
        .field(fields::PREPARED)
}

fn bound_query(client: &str, node: &str, id: &str) -> Rendered {
    Query::select(selection())
        .filter(
            Filter::eq(fields::SUBMISSION_ID, id)
                .and(Filter::eq(fields::CLIENT_DID, client))
                .and(Filter::eq(fields::NODE_DID, node)),
        )
        .limit(2)
        .render()
}

fn records(response: &serde_json::Value) -> Result<Vec<Record>, OutboxError> {
    Ok(serde_json::from_value(
        response["data"]["ClientAuthoredSubmission"].clone(),
    )?)
}

async fn retained(
    txn: &ConfigApplyTxn<'_>,
    client: &str,
    node: &str,
    id: &str,
) -> Result<Option<Record>, OutboxError> {
    let mut records = records(&txn.run(&bound_query(client, node, id)).await?)?;
    if records.len() > 1 {
        return Err(OutboxError::Record);
    }
    Ok(records.pop())
}

fn written(response: &serde_json::Value, field: &str) -> Result<(), OutboxError> {
    if !crate::graphql::mutation_documents(response, field)?.is_some_and(|rows| rows.len() == 1) {
        return Err(OutboxError::Record);
    }
    Ok(())
}

#[derive(Deserialize)]
struct Sequence {
    #[serde(rename = "_docID")]
    doc_id: String,
    next_ordinal: i64,
}

async fn allocate(txn: &ConfigApplyTxn<'_>, client: &str, node: &str) -> Result<i64, OutboxError> {
    let query = Query::select(Selection::doc_id().field(sequence::NEXT_ORDINAL))
        .filter(Filter::eq(sequence::CLIENT_DID, client).and(Filter::eq(sequence::NODE_DID, node)))
        .limit(2)
        .render();
    let response = txn.run(&query).await?;
    let mut rows: Vec<Sequence> =
        serde_json::from_value(response["data"]["ClientOutboxSequence"].clone())?;
    if rows.len() > 1 {
        return Err(OutboxError::Record);
    }
    if let Some(row) = rows.pop() {
        let next = row
            .next_ordinal
            .checked_add(1)
            .filter(|next| *next > 0)
            .ok_or(OutboxError::SequenceExhausted)?;
        let update =
            Update::doc(row.doc_id, Input::new().set(sequence::NEXT_ORDINAL, next)).render();
        written(&txn.run(&update).await?, "update_ClientOutboxSequence")?;
        Ok(row.next_ordinal)
    } else {
        let create = Create::new(
            Input::new()
                .set(sequence::CLIENT_DID, client)
                .set(sequence::NODE_DID, node)
                .set(sequence::NEXT_ORDINAL, 1_i64),
        )
        .render();
        written(&txn.run(&create).await?, "create_ClientOutboxSequence")?;
        Ok(0)
    }
}

pub async fn reserve(
    node: &Arc<defra_node::Node>,
    client: &str,
    authored: &AuthoredSubmission,
) -> Result<Record, OutboxError> {
    let id = authored
        .submission_id()
        .filter(|id| !id.is_empty())
        .ok_or(OutboxError::Binding)?;
    let runtime = authored.node_did();
    let valid = !runtime.is_empty()
        && authored
            .session_id()
            .is_some_and(|session| !session.is_empty());
    let local_signer = node.node_identity_did() == Some(client) && !client.is_empty();
    let mut value = serde_json::to_value(authored)?;
    value.sort_all_objects();
    let payload = serde_json::to_string(&value)?;
    let digest = blake3::hash(payload.as_bytes()).to_hex().to_string();
    ConfigAccess::transact_local_idempotent(
        node,
        Some(client.parse()?),
        IdempotentTransactionRetry::Standard,
        "client.outbox.reserve",
        |txn| {
            let payload = &payload;
            let digest = &digest;
            Box::pin(async move {
                let existing = retained(txn, client, runtime, id).await?;
                let same = existing
                    .as_ref()
                    .is_some_and(|record| record.digest == *digest);
                match reserve_decision(local_signer, valid, existing.is_some(), same) {
                    Reservation::Refused => Err(OutboxError::Binding),
                    Reservation::Retained => existing.ok_or(OutboxError::Record),
                    Reservation::Insert => {
                        let ordinal = allocate(txn, client, runtime).await?;
                        let create = Create::new(
                            Input::new()
                                .set(fields::SUBMISSION_ID, id)
                                .set(fields::CLIENT_DID, client)
                                .set(fields::NODE_DID, runtime)
                                .set(fields::ORDINAL, ordinal)
                                .set(fields::DIGEST, digest.as_str())
                                .set(fields::PENDING, true)
                                .set(fields::PAYLOAD, payload.as_str())
                                .set_null(fields::PREPARED),
                        )
                        .render();
                        written(&txn.run(&create).await?, "create_ClientAuthoredSubmission")?;
                        retained(txn, client, runtime, id)
                            .await?
                            .ok_or(OutboxError::Record)
                    }
                }
            })
        },
    )
    .await
}

pub async fn pending(
    node: &Arc<defra_node::Node>,
    client: &str,
    runtime: &str,
) -> Result<Vec<Record>, OutboxError> {
    if node.node_identity_did() != Some(client) {
        return Err(OutboxError::Binding);
    }
    let query = Query::select(selection())
        .filter(
            Filter::eq(fields::CLIENT_DID, client)
                .and(Filter::eq(fields::NODE_DID, runtime))
                .and(Filter::eq(fields::PENDING, true)),
        )
        .order_by(fields::ORDINAL, Direction::Asc)
        .render();
    records(
        &ConfigAccess::local_as(node.clone(), client.parse()?)
            .read(&query)
            .await?,
    )
}

pub async fn prepare(
    node: &Arc<defra_node::Node>,
    proposal: &Record,
    prepared: &str,
) -> Result<Record, OutboxError> {
    ConfigAccess::transact_local_idempotent(
        node,
        Some(proposal.client_did.parse()?),
        IdempotentTransactionRetry::Standard,
        "client.outbox.prepare",
        |txn| {
            Box::pin(async move {
                let existing = retained(
                    txn,
                    &proposal.client_did,
                    &proposal.node_did,
                    &proposal.submission_id,
                )
                .await?
                .ok_or(OutboxError::Record)?;
                let binding = node.node_identity_did() == Some(proposal.client_did.as_str())
                    && existing.digest == proposal.digest
                    && existing.ordinal == proposal.ordinal;
                match prepare_decision(
                    binding,
                    existing.pending,
                    existing.prepared.is_some(),
                    existing.prepared.as_deref() == Some(prepared),
                ) {
                    Reservation::Refused => Err(OutboxError::Binding),
                    Reservation::Retained => Ok(existing),
                    Reservation::Insert => {
                        let update = Update::doc(
                            &existing.doc_id,
                            Input::new().set(fields::PREPARED, prepared),
                        )
                        .render();
                        written(&txn.run(&update).await?, "update_ClientAuthoredSubmission")?;
                        retained(
                            txn,
                            &proposal.client_did,
                            &proposal.node_did,
                            &proposal.submission_id,
                        )
                        .await?
                        .ok_or(OutboxError::Record)
                    }
                }
            })
        },
    )
    .await
}

pub async fn acknowledge(
    node: &Arc<defra_node::Node>,
    proposal: &Record,
    runtime_confirmed: bool,
) -> Result<bool, OutboxError> {
    ConfigAccess::transact_local_idempotent(
        node,
        Some(proposal.client_did.parse()?),
        IdempotentTransactionRetry::Standard,
        "client.outbox.acknowledge",
        |txn| {
            Box::pin(async move {
                let existing = retained(
                    txn,
                    &proposal.client_did,
                    &proposal.node_did,
                    &proposal.submission_id,
                )
                .await?
                .ok_or(OutboxError::Record)?;
                let binding = node.node_identity_did() == Some(proposal.client_did.as_str())
                    && existing.digest == proposal.digest
                    && existing.ordinal == proposal.ordinal;
                if !acknowledges(binding, runtime_confirmed) {
                    return Ok(false);
                }
                if existing.pending {
                    let update = Update::doc(
                        existing.doc_id,
                        Input::new()
                            .set(fields::PENDING, false)
                            .set_null(fields::PAYLOAD)
                            .set_null(fields::PREPARED),
                    )
                    .render();
                    written(&txn.run(&update).await?, "update_ClientAuthoredSubmission")?;
                }
                Ok(true)
            })
        },
    )
    .await
}

pub async fn next(
    node: &Arc<defra_node::Node>,
    client: &str,
    runtime: &str,
    steer: Option<&str>,
) -> Result<Option<Record>, OutboxError> {
    let pending = pending(node, client, runtime).await?;
    let ids = pending
        .iter()
        .map(|record| record.submission_id.clone())
        .collect::<Vec<_>>();
    Ok(select(&ids, steer)
        .and_then(|id| pending.iter().find(|record| record.submission_id == id))
        .cloned())
}

pub async fn lookup(
    node: &Arc<defra_node::Node>,
    client: &str,
    runtime: &str,
    submission: &str,
) -> Result<Option<Record>, OutboxError> {
    let response = ConfigAccess::local_as(node.clone(), client.parse()?)
        .read(&bound_query(client, runtime, submission))
        .await?;
    let mut found = records(&response)?.into_iter();
    let record = found.next();
    if found.next().is_some() {
        return Err(OutboxError::Record);
    }
    Ok(record)
}

#[cfg(test)]
mod conformance;

#[cfg(test)]
mod native;
