use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    enrollment::canonical_domain_payload, goal::GoalSnapshot, request_admission::AgentRequestCreate,
};

pub const SESSION_GOAL_CONTROL_VERSION: u8 = 1;

#[derive(Debug, thiserror::Error)]
pub enum GoalControlError {
    #[error("unsupported goal observation version")]
    ObservationUnsupportedVersion,
    #[error("goal observation scope differs from its session")]
    ObservationScopeDiffers,
    #[error("invalid goal observation signature length")]
    ObservationInvalidSignatureLength,
    #[error("observed goal belongs to another session")]
    ObservedGoalForeignSession,
    #[error("observed goal has no document revision")]
    ObservedGoalNoRevision,
    #[error("unsupported goal control version")]
    UnsupportedVersion,
    #[error("empty goal control identity")]
    EmptyIdentity,
    #[error("empty goal control peer identity")]
    EmptyRequesterPeer,
    #[error(transparent)]
    Timestamp(#[from] chrono::ParseError),
    #[error("goal control expiry must follow issuance")]
    ExpiryNotAfterIssue,
    #[error("invalid goal control signature length")]
    InvalidSignatureLength,
    #[error("goal creation requires its signed first request")]
    CreateWithoutInitialRequest,
    #[error("empty goal control session document")]
    EmptySessionDocument,
    #[error("goal control lacks its session document")]
    MissingSessionDocument,
    #[error("goal control lacks its observed revision")]
    MissingObservedRevision,
    #[error("empty goal control revision")]
    EmptyRevision,
    #[error(transparent)]
    Serialize(#[from] serde_json::Error),
    #[error("unsupported goal control receipt version")]
    ReceiptUnsupportedVersion,
    #[error("goal control receipt differs from its command")]
    ReceiptDiffers,
    #[error("invalid goal control receipt signature length")]
    ReceiptInvalidSignatureLength,
    #[error("goal control receipt has no goal identity")]
    ReceiptNoGoal,
    #[error("empty resumed request identity")]
    ReceiptEmptyRequest,
    #[error("goal control receipt publishes an unexpected request")]
    ReceiptUnexpectedRequest,
    #[error("goal control receipt names another initial request")]
    ReceiptAnotherInitialRequest,
}

type Result<T, E = GoalControlError> = std::result::Result<T, E>;

/// Clients must observe this capability on the selected serving runtime before
/// removing a goal or replacing a terminal assignment with a fresh signed start.
pub const GOAL_STOP_CONTRACT_VERSION: u8 = 3;
pub const STATUS_GOAL_STOP_CONTRACT_FIELD: &str = "goal_stop_contract_version";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct GoalRevision {
    pub document: String,
    pub composite: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct ObservedGoal {
    pub revision: GoalRevision,
    /// Physical attribution stays in the signed read envelope rather than tool-facing Goal text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignment_root_request_doc_id: Option<String>,
    pub snapshot: GoalSnapshot,
}

/// A runtime-signed read projection. The Goal document remains authoritative;
/// session writers cannot forge goal state by changing this observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct SessionGoalObservation {
    pub version: u8,
    pub node_did: String,
    pub session_id: String,
    pub session_doc_id: String,
    pub requester_did: Option<String>,
    pub observed_at: String,
    pub goal: Option<ObservedGoal>,
    pub signature: Vec<u8>,
}

impl SessionGoalObservation {
    pub fn signing_payload(&self) -> Result<Vec<u8>> {
        let mut unsigned = self.clone();
        unsigned.signature.clear();
        signed_payload("gents-session-goal-observation-v1", &unsigned)
    }

    pub fn validate_scope(
        &self,
        node_did: &str,
        session_id: &str,
        session_doc_id: &str,
        requester_did: Option<&str>,
    ) -> Result<()> {
        if self.version != SESSION_GOAL_CONTROL_VERSION {
            return Err(GoalControlError::ObservationUnsupportedVersion);
        }
        if !(self.node_did == node_did
            && self.session_id == session_id
            && self.session_doc_id == session_doc_id
            && self.requester_did.as_deref() == requester_did)
        {
            return Err(GoalControlError::ObservationScopeDiffers);
        }
        chrono::DateTime::parse_from_rfc3339(&self.observed_at)?;
        if self.signature.len() != 64 {
            return Err(GoalControlError::ObservationInvalidSignatureLength);
        }
        if let Some(goal) = &self.goal {
            if goal.snapshot.node_did != node_did || goal.snapshot.session_id != session_id {
                return Err(GoalControlError::ObservedGoalForeignSession);
            }
            if goal.revision.document.is_empty() || goal.revision.composite.is_empty() {
                return Err(GoalControlError::ObservedGoalNoRevision);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum GoalOperation {
    Create {
        objective: String,
        token_budget: Option<i64>,
    },
    Edit {
        objective: Option<String>,
        token_budget: Option<i64>,
    },
    Pause {},
    Resume {},
    Stop {},
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionGoalControl {
    pub version: u8,
    pub command_id: String,
    pub requester_did: String,
    pub requester_peer_id: Option<String>,
    pub node_did: String,
    pub session_id: String,
    pub session_doc_id: Option<String>,
    pub issued_at: String,
    pub expires_at: String,
    pub expected: Option<GoalRevision>,
    pub operation: GoalOperation,
    /// Creation carries the author's signed first request. The existing goal
    /// submission owner commits it with the Goal and creation claim.
    pub initial_request: Option<Box<AgentRequestCreate>>,
    pub signature: Vec<u8>,
}

impl SessionGoalControl {
    pub fn signing_payload(&self) -> Result<Vec<u8>> {
        let mut unsigned = self.clone();
        unsigned.signature.clear();
        signed_payload("gents-session-goal-control-v1", &unsigned)
    }

    pub fn computed_digest(&self) -> Result<String> {
        let payload = signed_payload("gents-session-goal-control-digest-v1", self)?;
        Ok(format!("sha256:{:x}", Sha256::digest(payload)))
    }

    pub fn validate_shape(&self) -> Result<()> {
        if self.version != SESSION_GOAL_CONTROL_VERSION {
            return Err(GoalControlError::UnsupportedVersion);
        }
        for value in [
            &self.command_id,
            &self.requester_did,
            &self.node_did,
            &self.session_id,
        ] {
            if value.trim().is_empty() {
                return Err(GoalControlError::EmptyIdentity);
            }
        }
        if self
            .requester_peer_id
            .as_ref()
            .is_some_and(|peer| peer.trim().is_empty())
        {
            return Err(GoalControlError::EmptyRequesterPeer);
        }
        let issued = chrono::DateTime::parse_from_rfc3339(&self.issued_at)?;
        let expires = chrono::DateTime::parse_from_rfc3339(&self.expires_at)?;
        if expires <= issued {
            return Err(GoalControlError::ExpiryNotAfterIssue);
        }
        if self.signature.len() != 64 {
            return Err(GoalControlError::InvalidSignatureLength);
        }
        if matches!(self.operation, GoalOperation::Create { .. }) != self.initial_request.is_some()
        {
            return Err(GoalControlError::CreateWithoutInitialRequest);
        }
        if self
            .session_doc_id
            .as_ref()
            .is_some_and(|id| id.trim().is_empty())
        {
            return Err(GoalControlError::EmptySessionDocument);
        }
        if !matches!(self.operation, GoalOperation::Create { .. }) || self.expected.is_some() {
            if self.session_doc_id.is_none() {
                return Err(GoalControlError::MissingSessionDocument);
            }
            let revision = self
                .expected
                .as_ref()
                .ok_or(GoalControlError::MissingObservedRevision)?;
            if revision.document.trim().is_empty() || revision.composite.trim().is_empty() {
                return Err(GoalControlError::EmptyRevision);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum GoalControlOutcome {
    Applied {
        goal_id: String,
        request_id: Option<String>,
    },
    Rejected {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct SessionGoalControlReceipt {
    pub version: u8,
    pub command_id: String,
    pub command_digest: String,
    pub requester_did: String,
    pub node_did: String,
    pub session_id: String,
    pub outcome: GoalControlOutcome,
    pub processed_at: String,
    pub signature: Vec<u8>,
}

impl SessionGoalControlReceipt {
    pub fn signing_payload(&self) -> Result<Vec<u8>> {
        let mut unsigned = self.clone();
        unsigned.signature.clear();
        signed_payload("gents-session-goal-control-receipt-v1", &unsigned)
    }

    pub fn validate_for(&self, command: &SessionGoalControl) -> Result<()> {
        if self.version != SESSION_GOAL_CONTROL_VERSION {
            return Err(GoalControlError::ReceiptUnsupportedVersion);
        }
        if !(self.command_id == command.command_id
            && self.command_digest == command.computed_digest()?
            && self.requester_did == command.requester_did
            && self.node_did == command.node_did
            && self.session_id == command.session_id)
        {
            return Err(GoalControlError::ReceiptDiffers);
        }
        chrono::DateTime::parse_from_rfc3339(&self.processed_at)?;
        if self.signature.len() != 64 {
            return Err(GoalControlError::ReceiptInvalidSignatureLength);
        }
        if let GoalControlOutcome::Applied {
            goal_id,
            request_id,
        } = &self.outcome
        {
            if goal_id.trim().is_empty() {
                return Err(GoalControlError::ReceiptNoGoal);
            }
            if request_id.as_ref().is_some_and(|id| id.trim().is_empty()) {
                return Err(GoalControlError::ReceiptEmptyRequest);
            }
            if request_id.is_some()
                && !matches!(
                    command.operation,
                    GoalOperation::Create { .. } | GoalOperation::Resume {}
                )
            {
                return Err(GoalControlError::ReceiptUnexpectedRequest);
            }
            if let Some(initial) = command.initial_request.as_deref() {
                if request_id.as_deref() != Some(initial.request_id.as_str()) {
                    return Err(GoalControlError::ReceiptAnotherInitialRequest);
                }
            }
        }
        Ok(())
    }
}

fn signed_payload(domain: &str, value: &impl Serialize) -> Result<Vec<u8>> {
    let json = serde_json::to_string(value)?;
    Ok(canonical_domain_payload(domain, [json.as_str()]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn command() -> SessionGoalControl {
        SessionGoalControl {
            version: SESSION_GOAL_CONTROL_VERSION,
            command_id: "command".into(),
            requester_did: "requester".into(),
            requester_peer_id: Some("peer".into()),
            node_did: "node".into(),
            session_id: "session".into(),
            session_doc_id: Some("session-document".into()),
            issued_at: "2026-10-17T00:00:00Z".into(),
            expires_at: "2026-10-17T00:01:00Z".into(),
            expected: Some(GoalRevision {
                document: "goal".into(),
                composite: "commit".into(),
            }),
            operation: GoalOperation::Pause {},
            initial_request: None,
            signature: vec![0; 64],
        }
    }

    #[test]
    fn control_signatures_cover_the_full_scope_and_observed_revision() {
        let command = command();
        command.validate_shape().unwrap();
        let payload = command.signing_payload().unwrap();
        let original = serde_json::to_value(&command).unwrap();
        for field in [
            "command_id",
            "requester_did",
            "requester_peer_id",
            "node_did",
            "session_id",
            "session_doc_id",
            "issued_at",
            "expires_at",
        ] {
            let mut changed = original.clone();
            changed[field] = json!(format!("{}-changed", original[field].as_str().unwrap()));
            let changed: SessionGoalControl = serde_json::from_value(changed).unwrap();
            assert_ne!(changed.signing_payload().unwrap(), payload, "{field}");
        }
        for field in ["document", "composite"] {
            let mut changed = original.clone();
            changed["expected"][field] = json!("changed");
            let changed: SessionGoalControl = serde_json::from_value(changed).unwrap();
            assert_ne!(changed.signing_payload().unwrap(), payload, "{field}");
        }
        let mut changed = command.clone();
        changed.operation = GoalOperation::Resume {};
        assert_ne!(changed.signing_payload().unwrap(), payload);
        changed = command.clone();
        changed.signature[0] = 1;
        assert_eq!(changed.signing_payload().unwrap(), payload);
        assert_ne!(
            changed.computed_digest().unwrap(),
            command.computed_digest().unwrap()
        );
    }

    #[test]
    fn unknown_fields_cannot_extend_signed_commands() {
        let original = serde_json::to_value(command()).unwrap();
        let mut changed = original.clone();
        changed["override"] = json!(true);
        assert!(serde_json::from_value::<SessionGoalControl>(changed).is_err());
        let mut changed = original;
        changed["operation"]["request_id"] = json!("foreign-request");
        assert!(serde_json::from_value::<SessionGoalControl>(changed).is_err());
    }

    #[test]
    fn observation_scope_and_signature_domain_are_distinct_from_commands() {
        let observation = SessionGoalObservation {
            version: SESSION_GOAL_CONTROL_VERSION,
            node_did: "node".into(),
            session_id: "session".into(),
            session_doc_id: "session-document".into(),
            requester_did: Some("requester".into()),
            observed_at: "2026-10-17T00:00:00Z".into(),
            goal: None,
            signature: vec![0; 64],
        };
        observation
            .validate_scope("node", "session", "session-document", Some("requester"))
            .unwrap();
        assert!(observation
            .validate_scope("node", "session", "recreated-session", Some("requester"))
            .is_err());
        for (node, session, requester) in [
            ("foreign-node", "session", Some("requester")),
            ("node", "foreign-session", Some("requester")),
            ("node", "session", Some("foreign-requester")),
            ("node", "session", None),
        ] {
            assert!(observation
                .validate_scope(node, session, "session-document", requester)
                .is_err());
        }
        assert_ne!(
            observation.signing_payload().unwrap(),
            command().signing_payload().unwrap()
        );
        let mut encoded: Value = serde_json::to_value(observation).unwrap();
        encoded["override"] = json!(true);
        assert!(serde_json::from_value::<SessionGoalObservation>(encoded).is_err());
    }
}
