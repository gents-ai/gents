use serde_json::Value;

use super::apply_txn::TxnBackend;
use super::{graphql, ConfigApplyTxn, ConfigClientError};
use crate::config_client::Result;

impl ConfigApplyTxn<'_> {
    /// Read only account-selection metadata as this transaction's bound
    /// runtime. Native document ACP still filters and authorizes the read.
    /// The credential owner supplies `query`; store holds no OAuthCredential
    /// selection of its own.
    pub async fn execute_runtime_account_metadata_query(
        &self,
        node_did: &str,
        query: &str,
    ) -> Result<defra_node::QueryResponse> {
        self.execute_runtime_metadata_query(
            node_did,
            gents_protocol::schemas::OAUTH_CREDENTIAL_NAME,
            query,
        )
        .await
    }

    /// Read only the runtime Node's hop cap as this transaction's bound
    /// runtime. Native document ACP still filters the Node row.
    pub(super) async fn execute_runtime_hop_cap_query(
        &self,
        node_did: &str,
    ) -> Result<defra_node::QueryResponse> {
        self.execute_runtime_metadata_query(
            node_did,
            crate::collection::Collection::Node.graphql_type(),
            &super::runtime_hop_cap::runtime_hop_cap_query(node_did),
        )
        .await
    }

    async fn execute_runtime_metadata_query(
        &self,
        node_did: &str,
        collection: &str,
        query: &str,
    ) -> Result<defra_node::QueryResponse> {
        let TxnBackend::Embedded {
            node,
            agent_signer: Some(agent_signer),
            ..
        } = &self.backend
        else {
            return Err(ConfigClientError::EmbeddedAccessRequired);
        };
        if node.node_identity_did() != Some(node_did)
            || agent_signer.node_did() != node_did
            || self.actor_did() != Some(agent_signer.did())
        {
            return Err(ConfigClientError::TransactionActorMismatch);
        }
        let runtime =
            identity::Did::new(node_did).map_err(ConfigClientError::RelationshipTargetIdentity)?;
        graphql::ensure_query_document(query)?;
        let mut response = self
            .execute_local_response_with_actor(query, None, Some(runtime.clone()), None)
            .await?;
        let data = response
            .data
            .as_mut()
            .and_then(Value::as_object_mut)
            .ok_or(ConfigClientError::InvalidOwnerObservation)?;
        if data.len() != 1 {
            return Err(ConfigClientError::InvalidOwnerObservation);
        }
        let rows = data
            .get_mut(collection)
            .and_then(Value::as_array_mut)
            .ok_or(ConfigClientError::InvalidOwnerObservation)?;
        if rows.is_empty() {
            return Ok(response);
        }
        let doc_ids = rows
            .iter()
            .map(|row| {
                row["_docID"]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or(ConfigClientError::InvalidOwnerObservation)
            })
            .collect::<Result<Vec<_>>>()?;
        let decisions = self.runtime_may_use(&runtime, collection, &doc_ids).await?;
        if decisions.len() != doc_ids.len() {
            return Err(ConfigClientError::InvalidOwnerObservation);
        }
        let mut decisions = decisions.into_iter();
        rows.retain(|_| decisions.next() == Some(true));
        Ok(response)
    }
}
