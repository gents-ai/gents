use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{SubmittedRequest, UnresolvedSubmission};

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// The native caller supplies the canonical request identity and hashes its
/// complete authored command. This is equality within that identity, never
/// content-based lookup or authority to submit a different signed request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Intent {
    pub client_did: String,
    pub node_did: String,
    pub request_id: String,
    pub envelope_hash: String,
}

impl Intent {
    pub fn new(client_did: String, node_did: String, request_id: String, command: &[u8]) -> Self {
        Self {
            client_did,
            node_did,
            request_id,
            envelope_hash: digest(command),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Reference {
    pub request: SubmittedRequest,
    pub signed_payload_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Outcome {
    Unresolved,
    Accepted,
    NotDelivered,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    pub intent: Intent,
    pub reference: Reference,
    pub outcome: Outcome,
    pub pending: Option<UnresolvedSubmission>,
    pub acknowledged: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Decision {
    Refused,
    Reconcile(Reference),
    Settled(Reference, Outcome),
}

fn valid_digest(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

impl Record {
    /// A proposal has no delivery authority. Only a successful journal
    /// transaction may supply `durable=true` to the preparation decision.
    pub fn proposal(intent: Intent, pending: UnresolvedSubmission) -> Self {
        Self {
            reference: Reference {
                request: pending.result.clone(),
                signed_payload_hash: digest(pending.mutation.as_bytes()),
            },
            intent,
            outcome: Outcome::Unresolved,
            pending: Some(pending),
            acknowledged: false,
        }
    }

    fn bound(&self) -> bool {
        if self.intent.client_did.is_empty()
            || self.intent.request_id.is_empty()
            || self.reference.request.session_id.is_empty()
            || !valid_digest(&self.intent.envelope_hash)
            || !valid_digest(&self.reference.signed_payload_hash)
            || self.intent.request_id != self.reference.request.request_id
            || self.intent.node_did != self.reference.request.node_did
            || self.pending.is_some() == self.acknowledged
        {
            return false;
        }
        self.pending.as_ref().is_none_or(|pending| {
            pending.validate_binding(&self.intent.request_id).is_ok()
                && pending.result == self.reference.request
                && digest(pending.mutation.as_bytes()) == self.reference.signed_payload_hash
        })
    }

    fn result(&self) -> Decision {
        if !self.bound() {
            return Decision::Refused;
        }
        match self.outcome {
            Outcome::Unresolved if self.pending.is_some() => {
                Decision::Reconcile(self.reference.clone())
            }
            Outcome::Unresolved => Decision::Refused,
            outcome => Decision::Settled(self.reference.clone(), outcome),
        }
    }
}

/// Returned proposals remain staged data until the existing journal's atomic,
/// durable reservation succeeds. Matching callers reuse the stored result;
/// a later allocator proposal never replaces their original signed bytes.
pub(crate) fn prepare(
    records: &[Record],
    proposal: &Record,
    durable: bool,
) -> (Option<Record>, Decision) {
    let mut matching = records.iter().filter(|record| {
        record.intent.client_did == proposal.intent.client_did
            && record.intent.request_id == proposal.intent.request_id
    });
    if let Some(record) = matching.next() {
        return (
            None,
            if matching.next().is_none() && record.intent == proposal.intent {
                record.result()
            } else {
                Decision::Refused
            },
        );
    }
    if !durable
        || !proposal.bound()
        || records.iter().any(|record| {
            record.reference.request.request_id == proposal.reference.request.request_id
        })
    {
        return (None, Decision::Refused);
    }
    (Some(proposal.clone()), proposal.result())
}

fn unique(records: &[Record], client_did: &str, request_id: &str) -> Option<usize> {
    let mut matching = records.iter().enumerate().filter(|(_, record)| {
        record.intent.client_did == client_did && record.intent.request_id == request_id
    });
    let (index, _) = matching.next()?;
    matching.next().is_none().then_some(index)
}

/// The outcome must come from the existing signed owner-receipt reconciler;
/// transport absence and elapsed time are not non-delivery observations.
pub(crate) fn observe(
    records: &mut [Record],
    client_did: &str,
    request_id: &str,
    reference: &Reference,
    outcome: Outcome,
) {
    let Some(index) = unique(records, client_did, request_id) else {
        return;
    };
    let record = &mut records[index];
    if record.bound()
        && record.reference == *reference
        && record.outcome == Outcome::Unresolved
        && outcome != Outcome::Unresolved
    {
        record.outcome = outcome;
    }
}

/// Acknowledgement retires replay bytes, not the canonical request binding.
/// A stale restored caller must still receive the same confirmed outcome.
pub(crate) fn acknowledge(
    records: &mut [Record],
    client_did: &str,
    request_id: &str,
    reference: &Reference,
) {
    let Some(index) = unique(records, client_did, request_id) else {
        return;
    };
    let record = &mut records[index];
    if record.bound() && record.reference == *reference && record.outcome != Outcome::Unresolved {
        record.pending = None;
        record.acknowledged = true;
    }
}

#[cfg(test)]
mod tests;
