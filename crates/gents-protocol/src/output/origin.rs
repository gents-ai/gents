//! Pure header-origin checks from CanonicalOutput/Hydration.lean.
//!
//! The caller supplies authorized observations, not a whole parent-session
//! subscription. This validates immutable identity and ownership labels; ACP
//! authorization and payload reconstruction remain their existing owners.

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

/// Validate one exact edge. Forking may change identity, key and session, but
/// not native order/content or creation provenance, and drops live membership.
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
    if message.agent_did != original.agent_did || message.requester_did != original.requester_did {
        return Err(OriginError::ScopeMismatch {
            doc_id: child.doc_id.into(),
        });
    }
    if message.request_doc_id.is_some()
        || message.sequence != original.sequence
        || message.role != original.role
        || message.native_id != original.native_id
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

/// Resolve one immutable header, checking physical-ID and session/key/sequence
/// twins. Exact repeated observations are harmless.
/// Missing facts are incomplete; known denial is never inferred from absence.
pub fn lookup_message<'a>(
    messages: &[ObservedMessage<'a>],
    denied: &[String],
    id: &str,
    agent_did: &str,
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
        || row.message.agent_did != agent_did
        || row.message.requester_did.as_deref() != requester_did
    {
        return Err(OriginError::ScopeMismatch { doc_id: id.into() });
    }
    Ok(row)
}

/// Follow only the supplied, authorized exact origin references. Each lookup
/// shares the same physical/logical conflict checks as an ordinary header read.
/// The ultimate origin still needs ordinary payload reconstruction: validating
/// fork metadata must not bypass the origin's publication/source checks.
pub fn resolve_origin<'a>(
    messages: &[ObservedMessage<'a>],
    denied: &[String],
    root_doc_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
) -> Result<ObservedMessage<'a>, OriginError> {
    let lookup = |id: &str| lookup_message(messages, denied, id, agent_did, requester_did);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::{MessageRole, OutputOutcome};

    fn header(session: &str) -> TranscriptMessage {
        TranscriptMessage {
            message_key: format!("{session}:0"),
            session_id: session.into(),
            agent_did: "agent".into(),
            requester_did: Some("requester".into()),
            request_doc_id: Some("request".into()),
            publication: MessagePublication::RequestExecution {
                execution_generation: "generation".into(),
            },
            outcome: OutputOutcome::Complete,
            sequence: 0,
            role: MessageRole::Assistant,
            native_id: None,
            blocks: vec![],
            created_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn fork(session: &str, origin: &str) -> TranscriptMessage {
        TranscriptMessage {
            request_doc_id: None,
            publication: MessagePublication::Fork {
                origin_message_doc_id: origin.into(),
            },
            ..header(session)
        }
    }

    fn observed<'a>(doc_id: &'a str, message: &'a TranscriptMessage) -> ObservedMessage<'a> {
        ObservedMessage { doc_id, message }
    }

    #[test]
    fn exact_header_lookup_rejects_key_twin_even_at_another_sequence() {
        let first = header("session");
        let mut twin = first.clone();
        twin.sequence = 1;
        let facts = [observed("first", &first), observed("twin", &twin)];
        assert_eq!(
            lookup_message(&facts, &[], "first", "agent", Some("requester")),
            Err(OriginError::Conflict {
                doc_id: "first".into()
            })
        );
    }

    #[test]
    fn header_lookup_does_not_misclassify_missing_origin_as_missing_header() {
        let child = fork("child", "not-yet-replicated");
        let facts = [observed("child", &child)];
        assert_eq!(
            lookup_message(&facts, &[], "child", "agent", Some("requester")),
            Ok(facts[0])
        );
        assert_eq!(
            resolve_origin(&facts, &[], "child", "agent", Some("requester")),
            Err(OriginError::Unavailable {
                doc_id: "not-yet-replicated".into()
            })
        );
    }

    #[test]
    fn exact_replay_and_nested_forks_resolve_origin() {
        let original = header("parent");
        let child = fork("child", "original");
        let grandchild = fork("grandchild", "child");
        let rows = [
            observed("child", &child),
            observed("original", &original),
            observed("grandchild", &grandchild),
            observed("child", &child),
        ];
        assert_eq!(
            resolve_origin(&rows, &[], "grandchild", "agent", Some("requester")).unwrap(),
            observed("original", &original)
        );
    }

    #[test]
    fn missing_and_denied_origin_are_distinct() {
        let child = fork("child", "original");
        let rows = [observed("child", &child)];
        assert_eq!(
            resolve_origin(&rows, &[], "child", "agent", Some("requester")),
            Err(OriginError::Unavailable {
                doc_id: "original".into()
            })
        );
        assert_eq!(
            resolve_origin(
                &rows,
                &["original".into()],
                "child",
                "agent",
                Some("requester")
            ),
            Err(OriginError::Denied {
                doc_id: "original".into()
            })
        );
    }

    #[test]
    fn fork_cannot_change_sequence_content_or_tenancy() {
        let original = header("parent");
        for change in 0..5 {
            let mut child = fork("child", "original");
            match change {
                0 => child.sequence = 1,
                1 => child.native_id = Some("invented".into()),
                2 => child.request_doc_id = Some("invented".into()),
                3 => child.agent_did = "foreign".into(),
                _ => child.requester_did = None,
            }
            assert!(resolve_origin(
                &[observed("child", &child), observed("original", &original)],
                &[],
                "child",
                "agent",
                Some("requester")
            )
            .is_err());
        }
    }

    #[test]
    fn cycles_fail_instead_of_looping() {
        let child = fork("child", "parent");
        let parent = fork("parent", "child");
        assert!(matches!(
            resolve_origin(
                &[observed("child", &child), observed("parent", &parent)],
                &[],
                "child",
                "agent",
                Some("requester")
            ),
            Err(OriginError::InvalidOrigin { .. })
        ));
    }

    #[test]
    fn physical_identity_and_sequence_twins_are_not_overwritten() {
        let original = header("parent");
        let mut conflicting = original.clone();
        conflicting.native_id = Some("different".into());
        for twin_id in ["original", "other-document"] {
            assert!(matches!(
                resolve_origin(
                    &[
                        observed("original", &original),
                        observed(twin_id, &conflicting)
                    ],
                    &[],
                    "original",
                    "agent",
                    Some("requester")
                ),
                Err(OriginError::Conflict { .. })
            ));
        }
    }
}
