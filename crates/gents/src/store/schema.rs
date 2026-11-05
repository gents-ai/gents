pub mod migration;

use anyhow::Result;
use defra_node::Node as EmbeddedNode;

pub use gents_protocol::schemas::{
    AGENT as AGENT_SCHEMA, AGENT_MESSAGE as AGENT_MESSAGE_SCHEMA,
    AGENT_OUTPUT_SEGMENT as AGENT_OUTPUT_SEGMENT_SCHEMA, AGENT_REQUEST as AGENT_REQUEST_SCHEMA,
    AGENT_SESSION as AGENT_SESSION_SCHEMA, AGENT_TOOL_CALL as AGENT_TOOL_CALL_SCHEMA,
    COMPACTION_ENTRY as COMPACTION_ENTRY_SCHEMA,
    DATA_PLANE_PAIRING_DESIRED as DATA_PLANE_PAIRING_DESIRED_SCHEMA, GOAL as GOAL_SCHEMA,
    INFERENCE_BACKEND as INFERENCE_BACKEND_SCHEMA, INFERENCE_CALL as INFERENCE_CALL_SCHEMA,
    INFERENCE_PROFILE as INFERENCE_PROFILE_SCHEMA, MAILBOX_ITEM as MAILBOX_ITEM_SCHEMA,
    NODE as NODE_SCHEMA, NODE_RUNTIME as NODE_RUNTIME_SCHEMA,
    OAUTH_CREDENTIAL as OAUTH_CREDENTIAL_SCHEMA,
    PEER_PAIRING_APPLIED as PEER_PAIRING_APPLIED_SCHEMA,
    PEER_PAIRING_DESIRED as PEER_PAIRING_DESIRED_SCHEMA,
    PROJECTION_ACP_BINDING as PROJECTION_ACP_BINDING_SCHEMA, RUNTIME_ALL,
    SCHEDULE as SCHEDULE_SCHEMA, TASK as TASK_SCHEMA, TOOLS as TOOLS_SCHEMA,
    TOOL_SERVICE_HEALTH_STATE as TOOL_SERVICE_HEALTH_STATE_SCHEMA,
    TOOL_SERVICE_REGISTRY as TOOL_SERVICE_REGISTRY_SCHEMA,
};

#[derive(Debug, thiserror::Error)]
pub enum LegacyBackendPreflightError {
    #[error("legacy InferenceBackend documents may contain credentials in their history; they must remain inaccessible to Agents")]
    LegacyDocuments,
    #[error("cannot establish complete visibility of the legacy InferenceBackend collection")]
    VisibilityUnproven,
    #[error("cannot inspect the legacy InferenceBackend collection")]
    Collection(#[source] defra_node::Error),
    #[error("cannot inspect legacy InferenceBackend documents")]
    Query(#[source] crate::store::config_client::ConfigClientError),
    #[error(
        "Agent admission is unavailable until the legacy backend lineage is durably locked down"
    )]
    LockdownUnavailable,
}

/// Admission stays closed until native storage can atomically prove empty
/// legacy history and durably deny old-lineage reads, writes and recreation.
pub fn require_legacy_backend_lockdown() -> Result<(), LegacyBackendPreflightError> {
    Err(LegacyBackendPreflightError::LockdownUnavailable)
}

/// Diagnostic only: this separate schema observation and scan cannot fence a
/// concurrent policy change or prevent later writes and is not an admission gate.
/// An ungoverned native collection does not filter document IDs. A governed
/// collection may filter them, so an empty result there cannot prove absence.
/// Block every legacy document because a current value cannot establish that
/// no earlier signed block contains an inline secret.
pub async fn preflight_legacy_backend_history(
    node: &EmbeddedNode,
) -> Result<(), LegacyBackendPreflightError> {
    let Some(collection) = node
        .get_collection(gents_protocol::schemas::INFERENCE_BACKEND_NAME)
        .map_err(LegacyBackendPreflightError::Collection)?
    else {
        return Ok(());
    };
    if collection.policy.is_some() {
        return Err(LegacyBackendPreflightError::VisibilityUnproven);
    }
    let response = crate::store::graphql::graphql_with_transaction_retry(
        node,
        "{ InferenceBackend(limit: 1) { _docID } }",
        "schema.legacy_backend_preflight",
    )
    .await
    .map_err(LegacyBackendPreflightError::Query)?;
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("InferenceBackend"))
        .and_then(serde_json::Value::as_array)
        .ok_or(LegacyBackendPreflightError::VisibilityUnproven)?;
    if rows.is_empty() {
        Ok(())
    } else {
        Err(LegacyBackendPreflightError::LegacyDocuments)
    }
}

pub async fn ensure_runtime_schemas(node: &EmbeddedNode) -> Result<()> {
    if let Some(access) = crate::store::config_client::ConfigAccess::claimed_for_node(node) {
        return Ok(access
            .run_local_owned(|node| async move {
                crate::store::schema::migration::ensure_migrations_borrowed(node.as_ref()).await
            })
            .await?);
    }
    Ok(crate::store::schema::migration::ensure_migrations_borrowed(node).await?)
}

#[cfg(test)]
mod legacy_backend_tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn admission_stays_closed_when_native_lockdown_is_unavailable() {
        let modeled = crate::lean_vocab_test::lean_identity_household_cases()
            .iter()
            .find(|case| case.name == "configuration-explicit-grants")
            .unwrap();
        assert_eq!(
            require_legacy_backend_lockdown().is_ok(),
            modeled.legacy_backend_lockdown_unavailable
        );
    }

    #[tokio::test]
    async fn legacy_history_preflight_leaves_existing_document_untouched() -> anyhow::Result<()> {
        let node = Arc::new(crate::test_support::node_builder().build().await?);
        let access = crate::store::config_client::ConfigAccess::local(node.clone());
        let unbound = gents_protocol::schemas::INFERENCE_BACKEND.replace(
            &format!(
                " @policy(id: \"{}\", resource: \"secret_configuration\")",
                gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID
            ),
            "",
        );
        access.add_collection(&unbound).await?;
        access
            .write(
                "legacy_fixture",
                r#"mutation { create_InferenceBackend(input: {
            node_did: "did:key:legacy", backend_id: "legacy", name: "legacy",
            provider_kind: "OpenAiCompatible", endpoint: "https://provider.example",
            auth: {kind: "api_key", key: "legacy-secret"}
        }) { _docID } }"#,
            )
            .await?;
        assert!(matches!(
            preflight_legacy_backend_history(&node).await,
            Err(LegacyBackendPreflightError::LegacyDocuments)
        ));
        assert!(ensure_runtime_schemas(&node).await.is_err());
        assert!(node
            .get_collection(gents_protocol::schemas::NODE_NAME)?
            .is_none());
        let result = access
            .execute("{ InferenceBackend { backend_id auth } }")
            .await?;
        let rows = gents_protocol::graphql::graphql_rows_from_response(&result, "InferenceBackend");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["auth"]["key"], "legacy-secret");
        Ok(())
    }

    #[tokio::test]
    async fn unbound_legacy_schema_is_refused_even_when_no_rows_are_visible() -> anyhow::Result<()>
    {
        let node = crate::test_support::node_builder().build().await?;
        let unbound = gents_protocol::schemas::INFERENCE_BACKEND.replace(
            &format!(
                " @policy(id: \"{}\", resource: \"secret_configuration\")",
                gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID
            ),
            "",
        );
        node.add_collection(&unbound).await?;
        let previous = node
            .get_collection(gents_protocol::schemas::INFERENCE_BACKEND_NAME)?
            .expect("unbound legacy schema");
        assert!(preflight_legacy_backend_history(&node).await.is_ok());
        assert!(ensure_runtime_schemas(&node).await.is_err());
        assert!(node
            .get_collection(gents_protocol::schemas::NODE_NAME)?
            .is_none());
        let retained = node
            .get_collection(gents_protocol::schemas::INFERENCE_BACKEND_NAME)?
            .expect("legacy schema is retained");
        assert_eq!(retained.version_id, previous.version_id);
        assert!(retained.policy.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn fresh_baseline_installs_private_legacy_schema() -> anyhow::Result<()> {
        let node = Arc::new(crate::test_support::node_builder().build().await?);
        ensure_runtime_schemas(&node).await?;
        let collection = node
            .get_collection(gents_protocol::schemas::INFERENCE_BACKEND_NAME)?
            .expect("fresh legacy schema");
        let binding = !collection.is_branchable
            && collection.policy.as_ref().is_some_and(|policy| {
                policy.id == gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID
                    && policy.resource_name == "secret_configuration"
            });
        let modeled = crate::lean_vocab_test::lean_identity_household_cases()
            .iter()
            .find(|case| case.name == "configuration-explicit-grants")
            .unwrap();
        let case = modeled
            .fresh_backend_admission_cases
            .iter()
            .find(|case| {
                case["origin"] == true && case["binding"] == true && case["intact"] == true
            })
            .unwrap();
        assert_eq!(binding, case["allowed"].as_bool().unwrap());
        Ok(())
    }

    #[tokio::test]
    async fn private_legacy_schema_retains_binding_after_reopen() -> anyhow::Result<()> {
        use crate::store::identity::NodeIdentity;
        let directory = tempfile::tempdir()?;
        let key = directory.path().join("node.key");
        let did = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
            Ok(crate::KeyIdentity::load_or_create(key, None)?
                .did()
                .to_owned())
        })
        .await??;
        let data = directory.path().join("data");
        let open = || {
            defra_node::Node::builder()
                .data_path(&data)
                .with_storage_backend(defra_node::StorageBackend::Regolith)
                .with_node_identity_did(did.clone())
        };
        let node = open().build().await?;
        ensure_runtime_schemas(&node).await?;
        let original = node
            .get_collection(gents_protocol::schemas::INFERENCE_BACKEND_NAME)?
            .expect("fresh legacy schema");
        node.shutdown().await?;
        drop(node);
        let reopened = open().build().await?;
        ensure_runtime_schemas(&reopened).await?;
        let current = reopened
            .get_collection(gents_protocol::schemas::INFERENCE_BACKEND_NAME)?
            .expect("reopened legacy schema");
        assert_eq!(current.collection_id, original.collection_id);
        assert_eq!(current.version_id, original.version_id);
        assert_eq!(current.policy, original.policy);
        assert!(!current.is_branchable);
        reopened.shutdown().await?;
        Ok(())
    }
}
