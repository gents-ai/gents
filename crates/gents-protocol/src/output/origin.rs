use super::{MessagePublication, TranscriptMessage};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ObservedMessage<'a> {
    pub doc_id: &'a str,
    pub message: &'a TranscriptMessage,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OriginError {
    Unavailable { doc_id: String },
    Denied { doc_id: String },
    Conflict { doc_id: String },
    ScopeMismatch { doc_id: String },
    InvalidOrigin { doc_id: String },
    MetadataMismatch { doc_id: String },
}

impl OriginError {
    pub fn is_incomplete(&self) -> bool {
        matches!(self, Self::Unavailable { .. })
    }
}

impl std::fmt::Display for OriginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "canonical header origin: {self:?}")
    }
}

impl std::error::Error for OriginError {}

pub fn validate_fork_metadata(
    child: ObservedMessage<'_>,
    origin: ObservedMessage<'_>,
) -> Result<(), OriginError> {
    let message = child.message;
    let original = origin.message;
    if !matches!(&message.publication,
        MessagePublication::Fork { origin_message_doc_id } if origin_message_doc_id == origin.doc_id)
    {
        return Err(OriginError::InvalidOrigin {
            doc_id: child.doc_id.into(),
        });
    }
    if message.node_did != original.node_did || message.requester_did != original.requester_did {
        return Err(OriginError::ScopeMismatch {
            doc_id: child.doc_id.into(),
        });
    }
    if message.request_doc_id.is_some()
        || message.sequence != original.sequence
        || message.role != original.role
        || message.native_id != original.native_id
        || message.native_request_id != original.native_request_id
        || message.outcome != original.outcome
        || message.blocks != original.blocks
        || message.created_at != original.created_at
    {
        return Err(OriginError::MetadataMismatch {
            doc_id: child.doc_id.into(),
        });
    }
    Ok(())
}

/// Missing facts are incomplete; known denial is never inferred from absence.
pub fn lookup_message<'a>(
    messages: &[ObservedMessage<'a>],
    denied: &[String],
    id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<ObservedMessage<'a>, OriginError> {
    if denied.iter().any(|denied| denied == id) {
        return Err(OriginError::Denied { doc_id: id.into() });
    }
    let mut matching = messages.iter().copied().filter(|row| row.doc_id == id);
    let row = matching
        .next()
        .ok_or_else(|| OriginError::Unavailable { doc_id: id.into() })?;
    if matching.any(|other| other != row)
        || messages.iter().any(|other| {
            *other != row
                && other.message.session_id == row.message.session_id
                && (other.message.message_key == row.message.message_key
                    || other.message.sequence == row.message.sequence)
        })
    {
        return Err(OriginError::Conflict { doc_id: id.into() });
    }
    if row.doc_id.trim().is_empty()
        || row.message.node_did != node_did
        || row.message.requester_did.as_deref() != requester_did
    {
        return Err(OriginError::ScopeMismatch { doc_id: id.into() });
    }
    Ok(row)
}

/// Validating fork metadata must not bypass the origin's publication/source
/// checks.
pub fn resolve_origin<'a>(
    messages: &[ObservedMessage<'a>],
    denied: &[String],
    root_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<ObservedMessage<'a>, OriginError> {
    let lookup = |id: &str| lookup_message(messages, denied, id, node_did, requester_did);
    let mut current = lookup(root_doc_id)?;
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(current.doc_id) {
            return Err(OriginError::InvalidOrigin {
                doc_id: current.doc_id.into(),
            });
        }
        let MessagePublication::Fork {
            origin_message_doc_id,
        } = &current.message.publication
        else {
            return Ok(current);
        };
        let origin = lookup(origin_message_doc_id)?;
        validate_fork_metadata(current, origin)?;
        current = origin;
    }
}
