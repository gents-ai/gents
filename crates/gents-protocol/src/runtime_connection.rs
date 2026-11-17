use serde::{Deserialize, Serialize};

use crate::client_access::ClientAccessRequest;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConnection {
    pub node_did: String,
    pub label: String,
    pub graphql: String,
    pub local_node_home: Option<String>,
    pub access_request: Option<ClientAccessRequest>,
    pub access_state: Option<crate::client_access::ClientAccessState>,
}

impl RuntimeConnection {
    pub fn refusal(&self, selected: &str) -> Option<ConnectionRefusalCause> {
        (self.node_did != selected || !self.validate())
            .then_some(ConnectionRefusalCause::NoEndpoint)
    }

    pub fn validate(&self) -> bool {
        !self.node_did.is_empty()
            && !self.label.trim().is_empty()
            && url::Url::parse(&self.graphql).is_ok_and(|url| {
                matches!(url.scheme(), "http" | "https")
                    && url.host_str().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none()
                    && url.path().ends_with("/api/v0/graphql")
            })
            && self
                .access_request
                .as_ref()
                .is_none_or(|request| request.node_did == self.node_did)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionRefusalCause {
    NoRoute,
    NoEndpoint,
}

impl ConnectionRefusalCause {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoRoute => "no_route",
            Self::NoEndpoint => "no_endpoint",
        }
    }
}

impl std::fmt::Display for ConnectionRefusalCause {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod conformance {
    #[test]
    fn refusal_tokens_follow_the_model() {
        gents_lean_vocab::families::client_chat_readiness_cause_token::replay::<
            super::ConnectionRefusalCause,
        >(|input| input.cause.as_str().to_owned());
    }
}
