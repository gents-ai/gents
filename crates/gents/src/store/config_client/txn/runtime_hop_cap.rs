use serde_json::Value;

use super::{ConfigApplyTxn, ConfigClientError};
use crate::store::graphql::escape_graphql_string;

#[derive(Debug, thiserror::Error)]
pub(crate) enum RuntimeHopCapError {
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
    pub(crate) async fn runtime_max_request_hop(
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

#[cfg(test)]
mod tests {
    use anyhow::Context as _;

    use crate::store::config_client::{
        apply_desired_state_plan, read_desired_state_record_in_txn, ConfigAccess,
        DesiredStateApplyDocument, DesiredStateApplyPlan, NodePermission,
    };
    use crate::store::identity::NodeIdentity;

    async fn set_node_hop_cap(
        owner: &ConfigAccess,
        node_did: &str,
        cap: u32,
    ) -> anyhow::Result<()> {
        owner
            .transact_write("test.runtime_hop_cap.bound", |txn| {
                Box::pin(async move {
                    let (_, mut node) = read_desired_state_record_in_txn(
                        txn,
                        crate::Collection::Node,
                        node_did,
                        node_did,
                    )
                    .await?
                    .context("owned Node is unavailable")?;
                    node["max_request_hop"] = serde_json::json!(cap);
                    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                        collection: crate::Collection::Node,
                        add: node.clone(),
                        update: node,
                    }])?;
                    apply_desired_state_plan(txn, &plan).await
                })
            })
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn agent_transaction_reads_hidden_node_cap_as_runtime_and_fails_closed(
    ) -> anyhow::Result<()> {
        let (db, user, user_access, binding) =
            crate::test_support::signed_test_node("runtime-hop-cap").await;
        let runtime = db.node_identity.as_ref();
        let signer = crate::store::identity::resolve_agent_identity(runtime, &binding)?;
        let agent = ConfigAccess::local_managed_agent(
            db.node.clone(),
            identity::Did::new(&binding.agent_did)?,
            signer.as_ref(),
        )?;
        set_node_hop_cap(&user_access, runtime.did(), 1).await?;
        let observe = |node_did: String| {
            agent.transact_readonly("test.runtime_hop_cap.observe", move |txn| {
                let node_did = node_did.clone();
                Box::pin(async move {
                    Ok::<_, anyhow::Error>(txn.runtime_max_request_hop(&node_did).await?)
                })
            })
        };
        let hidden = agent.execute("{ Node { _docID } }").await?;
        assert!(hidden["data"]["Node"].as_array().is_some_and(Vec::is_empty));
        assert_eq!(observe(runtime.did().to_owned()).await?, Some(Some(1)));
        assert!(observe("did:key:foreign".to_owned()).await.is_err());

        let admin = ConfigAccess::local_as(db.node.clone(), identity::Did::new(runtime.did())?);
        admin
            .grant_node_permission(
                &identity::Did::new(user.did())?,
                NodePermission::DacRelationDelete,
            )
            .await?;
        let node = user_access.execute("{ Node { _docID } }").await?;
        let node_doc = node["data"]["Node"][0]["_docID"].as_str().unwrap();
        user_access
            .delete_actor_relationship(
                "Node",
                node_doc,
                "runtime_reader",
                &identity::Did::new(runtime.did())?,
            )
            .await?;
        assert_eq!(observe(runtime.did().to_owned()).await?, None);
        Ok(())
    }
}
