use super::{ConfigAccess, ConfigClientError, TransactionFailure};
use crate::store::graphql::escape_graphql_string;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSelfConfigPermission {
    Grant,
    Revoke,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentSelfConfigPermissionError {
    #[error("invalid Agent DID")]
    InvalidAgentDid(#[source] identity::Error),
    #[error("the Agent is missing or not visible to this caller")]
    AgentUnavailable,
    #[error("Agent identity lookup returned an invalid response")]
    InvalidResponse,
    #[error(transparent)]
    Access(#[from] ConfigClientError),
}

impl TransactionFailure for AgentSelfConfigPermissionError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Access(error) => Some(error),
            _ => None,
        }
    }
}

impl ConfigAccess {
    /// Mutate the Agent's explicit native self-configuration grant as the
    /// authenticated caller. Tool availability does not imply this grant;
    /// DefraDB decides whether the caller may manage the relationship. The
    /// lookup and native grant are separate operations and cannot fence a
    /// concurrent replacement of the observed document.
    pub async fn set_agent_self_config_permission(
        &self,
        node_did: &str,
        agent_did: &str,
        permission: AgentSelfConfigPermission,
    ) -> Result<(), AgentSelfConfigPermissionError> {
        let agent = identity::Did::new(agent_did.to_owned())
            .map_err(AgentSelfConfigPermissionError::InvalidAgentDid)?;
        let query = format!(
            "{{ Agent(filter: {{ node_did: {{ _eq: \"{}\" }}, agent_did: {{ _eq: \"{}\" }} }}, limit: 2) {{ _docID }} }}",
            escape_graphql_string(node_did),
            escape_graphql_string(agent_did),
        );
        let response = self.execute(&query).await?;
        let rows = response
            .pointer("/data/Agent")
            .and_then(serde_json::Value::as_array)
            .ok_or(AgentSelfConfigPermissionError::InvalidResponse)?;
        let [row] = rows.as_slice() else {
            return Err(AgentSelfConfigPermissionError::AgentUnavailable);
        };
        let doc_id = row
            .get("_docID")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or(AgentSelfConfigPermissionError::InvalidResponse)?;
        match permission {
            AgentSelfConfigPermission::Grant => {
                self.add_actor_relationship("Agent", doc_id, "config_writer", &agent)
                    .await?
            }
            AgentSelfConfigPermission::Revoke => {
                self.delete_actor_relationship("Agent", doc_id, "config_writer", &agent)
                    .await?
            }
        }
        Ok(())
    }
}
