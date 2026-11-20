use serde::{Deserialize, Serialize};

use crate::{
    goal_control::{GoalOperation, GoalRevision},
    operations::request::MessageSendInput,
    request_input::RequestInput,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthoredGoal {
    pub node_did: String,
    pub command_id: String,
    pub session_id: String,
    pub session_doc_id: Option<String>,
    pub expected: Option<GoalRevision>,
    pub operation: GoalOperation,
    pub agent_did: Option<String>,
    pub input: RequestInput,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "input",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum AuthoredSubmission {
    Chat(Box<MessageSendInput>),
    Goal(Box<AuthoredGoal>),
}

impl AuthoredSubmission {
    pub fn node_did(&self) -> &str {
        match self {
            Self::Chat(input) => &input.node_did,
            Self::Goal(input) => &input.node_did,
        }
    }

    pub fn submission_id(&self) -> Option<&str> {
        match self {
            Self::Chat(input) => input.request_id.as_deref(),
            Self::Goal(input) => Some(&input.command_id),
        }
    }

    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::Chat(input) => input.session_id.as_deref(),
            Self::Goal(input) => Some(&input.session_id),
        }
    }
}

pub fn authored_session_id(submission: &str, session: Option<&str>) -> String {
    session.unwrap_or(submission).to_owned()
}

#[cfg(test)]
mod conformance {
    #[test]
    fn authored_session_binding_matches_model() {
        for case in gents_lean_vocab::families::client_delegation_outbox_session::cases() {
            case.check(super::authored_session_id(
                &case.input.submission,
                case.input.session.as_deref(),
            ));
        }
    }
}
