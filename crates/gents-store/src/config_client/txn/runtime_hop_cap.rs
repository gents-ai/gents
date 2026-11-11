use serde_json::Value;

use super::{ConfigApplyTxn, ConfigClientError};
use crate::graphql::escape_graphql_string;

#[derive(Debug, thiserror::Error)]
pub enum RuntimeHopCapError {
    #[error("the hop-cap read requires the bound runtime and a managed Agent transaction")]
    RuntimeScope,
    #[error(transparent)]
    Access(#[from] ConfigClientError),
    #[error("multiple Node rows matched {0}")]
    Ambiguous(String),
    #[error("Node.max_request_hop must be a nonnegative u32")]
    Decode,
}

pub(super) fn runtime_hop_cap_query(node_did: &str) -> String {
    format!(
        r#"{{ Node(filter: {{ node_did: {{ _eq: "{}" }} }}, limit: 2) {{ max_request_hop }} }}"#,
        escape_graphql_string(node_did)
    )
}

impl ConfigApplyTxn<'_> {
    /// The runtime Node's `max_request_hop` as the bound runtime reads it on
    /// this Agent transaction's snapshot. The Agent's own Node visibility does
    /// not decide the cap. `None` is a Node the runtime cannot read, which is
    /// unavailable rather than an unset cap.
    pub async fn runtime_max_request_hop(
        &self,
        node_did: &str,
    ) -> Result<Option<Option<u32>>, RuntimeHopCapError> {
        let response = self
            .execute_runtime_hop_cap_query(node_did)
            .await
            .map_err(|error| match error {
                ConfigClientError::EmbeddedAccessRequired
                | ConfigClientError::TransactionActorMismatch => RuntimeHopCapError::RuntimeScope,
                error => RuntimeHopCapError::Access(error),
            })?;
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data.get("Node"))
            .and_then(Value::as_array)
            .ok_or(RuntimeHopCapError::Decode)?;
        let row = match rows.as_slice() {
            [] => return Ok(None),
            [row] => row,
            _ => return Err(RuntimeHopCapError::Ambiguous(node_did.to_owned())),
        };
        match &row["max_request_hop"] {
            Value::Null => Ok(Some(None)),
            cap => cap
                .as_u64()
                .and_then(|cap| u32::try_from(cap).ok())
                .map(|cap| Some(Some(cap)))
                .ok_or(RuntimeHopCapError::Decode),
        }
    }
}
