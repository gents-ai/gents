use gents_protocol::goal_control::{SessionGoalControl, SessionGoalControlReceipt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Intent {
    pub client_did: String,
    pub node_did: String,
    pub command_id: String,
    pub session_id: String,
    pub authored_hash: String,
}

impl Intent {
    pub(crate) fn new(
        client_did: String,
        node_did: String,
        command_id: String,
        session_id: String,
        authored: &[u8],
    ) -> Self {
        Self {
            client_did,
            node_did,
            command_id,
            session_id,
            authored_hash: format!("sha256:{:x}", Sha256::digest(authored)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(rename = "GoalSubmissionReference"))]
pub struct Reference {
    pub command_id: String,
    pub command_digest: String,
    pub initial_request_id: Option<String>,
    pub requester_did: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    pub intent: Intent,
    pub reference: Reference,
    pub prepared: Option<SessionGoalControl>,
    pub receipt: Option<SessionGoalControlReceipt>,
    pub acknowledged: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Decision {
    Refused,
    Unconfirmed,
    Pending(Reference),
    Confirmed(Reference, Box<SessionGoalControlReceipt>),
}

impl Record {
    /// Preparation has already passed the native Goal signing owner. A proposal
    /// carries no publication authority until its journal reservation commits.
    pub(crate) fn proposal(intent: Intent, prepared: SessionGoalControl) -> anyhow::Result<Self> {
        prepared.validate_shape()?;
        let reference = Reference {
            command_id: prepared.command_id.clone(),
            command_digest: prepared.computed_digest()?,
            initial_request_id: prepared
                .initial_request
                .as_ref()
                .map(|row| row.request_id.clone()),
            requester_did: prepared.requester_did.clone(),
        };
        let record = Self {
            intent,
            reference,
            prepared: Some(prepared),
            receipt: None,
            acknowledged: false,
        };
        anyhow::ensure!(
            record.valid(),
            "Goal caller preparation has a different binding"
        );
        Ok(record)
    }

    fn receipt_bound(&self, receipt: &SessionGoalControlReceipt) -> bool {
        receipt.command_id == self.reference.command_id
            && receipt.command_digest == self.reference.command_digest
            && receipt.requester_did == self.reference.requester_did
            && receipt.node_did == self.intent.node_did
            && receipt.session_id == self.intent.session_id
    }

    fn valid(&self) -> bool {
        if [
            &self.intent.client_did,
            &self.intent.node_did,
            &self.intent.command_id,
            &self.intent.session_id,
            &self.reference.requester_did,
        ]
        .iter()
        .any(|value| value.trim().is_empty())
            || !valid_digest(&self.intent.authored_hash)
            || !valid_digest(&self.reference.command_digest)
            || self.intent.command_id != self.reference.command_id
            || self.prepared.is_some() == self.acknowledged
            || self.acknowledged && self.receipt.is_none()
            || self
                .receipt
                .as_ref()
                .is_some_and(|receipt| !self.receipt_bound(receipt))
        {
            return false;
        }
        self.prepared.as_ref().is_none_or(|prepared| {
            prepared.validate_shape().is_ok()
                && prepared.computed_digest().ok().as_ref() == Some(&self.reference.command_digest)
                && prepared.command_id == self.intent.command_id
                && prepared.node_did == self.intent.node_did
                && prepared.session_id == self.intent.session_id
                && prepared.requester_did == self.reference.requester_did
                && prepared.initial_request.as_ref().map(|row| &row.request_id)
                    == self.reference.initial_request_id.as_ref()
        })
    }

    pub(crate) fn decision(&self) -> Decision {
        if !self.valid() {
            Decision::Refused
        } else if let Some(receipt) = &self.receipt {
            Decision::Confirmed(self.reference.clone(), Box::new(receipt.clone()))
        } else {
            Decision::Pending(self.reference.clone())
        }
    }
}

fn valid_digest(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

pub(crate) fn query(records: &[Record], intent: &Intent) -> Decision {
    let mut matching = records
        .iter()
        .filter(|record| record.intent.command_id == intent.command_id);
    match (matching.next(), matching.next()) {
        (None, _) => Decision::Unconfirmed,
        (Some(record), None) if record.intent == *intent => record.decision(),
        _ => Decision::Refused,
    }
}

pub(crate) fn reserve(
    records: &[Record],
    proposal: &Record,
    durable: bool,
) -> (Option<Record>, Decision) {
    match query(records, &proposal.intent) {
        Decision::Unconfirmed if durable && proposal.valid() => {
            (Some(proposal.clone()), proposal.decision())
        }
        Decision::Unconfirmed => (None, Decision::Refused),
        existing => (None, existing),
    }
}

pub(crate) fn resume(records: &[Record], intent: &Intent) -> (Decision, Option<Reference>) {
    let decision = query(records, intent);
    let publication = match &decision {
        Decision::Pending(reference) => Some(reference.clone()),
        _ => None,
    };
    (decision, publication)
}

/// Receipt verification is supplied by the existing signed Goal receipt owner;
/// transport loss, current Goal state and elapsed time supply no such fact.
pub(crate) fn observe(
    records: &mut [Record],
    intent: &Intent,
    receipt: &SessionGoalControlReceipt,
    verified: bool,
) {
    if !verified || !matches!(query(records, intent), Decision::Pending(_)) {
        return;
    }
    if let Some(record) = records.iter_mut().find(|record| record.intent == *intent) {
        if record.receipt_bound(receipt) {
            record.receipt = Some(receipt.clone());
        }
    }
}

pub(crate) fn acknowledge(records: &mut [Record], intent: &Intent, reference: &Reference) {
    if !matches!(query(records, intent), Decision::Confirmed(ref original, _) if original == reference)
    {
        return;
    }
    if let Some(record) = records.iter_mut().find(|record| record.intent == *intent) {
        record.prepared = None;
        record.acknowledged = true;
    }
}

#[cfg(test)]
mod tests;
