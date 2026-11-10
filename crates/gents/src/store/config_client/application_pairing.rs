use serde_json::Value;

use super::{ConfigAccess, Result};
use crate::store::graphql::escape_graphql_string;

const OVERLAY: &str = gents_protocol::schemas::DATA_PLANE_PAIRING_DESIRED_NAME;

impl ConfigAccess {
    /// Every application-collection pairing overlay this access's actor can
    /// read.
    pub async fn list_application_pairings(&self) -> Result<Vec<Value>> {
        let response = self
            .execute(
                "{ DataPlanePairingDesired { _docID peer_id node_did actor_did collections template \
                 enrollment_member_did enrollment_sequence enrollment_network_id updated_at } }",
            )
            .await?;
        Ok(response["data"][OVERLAY]
            .as_array()
            .cloned()
            .unwrap_or_default())
    }

    /// Deletes the application pairing toward one peer as this access's actor
    /// and returns the deleted document IDs. The delete is the actor's own
    /// (client-signed on the HTTP path) and native document ACP decides it: the
    /// Node's user owner may remove any Agent's pairing through its Node parent
    /// (Lean `Identity.Configuration.pairing_overlay_delete_is_owner_scoped`).
    pub async fn remove_application_pairing(&self, peer_id: &str) -> Result<Vec<String>> {
        let query = format!(
            r#"{{ {OVERLAY}(filter: {{peer_id: {{_eq: "{}"}}}}) {{ _docID }} }}"#,
            escape_graphql_string(peer_id)
        );
        self.transact_write("p2p.remove_application_pairing", |txn| {
            let query = &query;
            Box::pin(async move {
                let found = txn.execute(query).await?;
                let ids = found["data"][OVERLAY]
                    .as_array()
                    .map(|rows| {
                        rows.iter()
                            .filter_map(|row| row["_docID"].as_str().map(str::to_owned))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                for id in &ids {
                    txn.delete_application_pairing(id).await?;
                }
                Ok::<_, super::ConfigClientError>(ids)
            })
        })
        .await
    }
}
