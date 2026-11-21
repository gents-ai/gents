use std::sync::{Arc, LazyLock};

use super::catalog::{BaselineCollection, Catalog, SchemaError, BASELINE};
use defra_node::Node;

use crate::config_client::{ConfigAccess, ConfigClientError};

static COLLECTIONS: LazyLock<Vec<BaselineCollection<'static>>> = LazyLock::new(|| {
    let baseline = &BASELINE;
    let sdl = baseline
        .iter()
        .map(|entry| entry.sdl)
        .collect::<Vec<_>>()
        .join("\n");
    let parsed = query::parse_sdl(&gents_protocol::schemas::HomePolicyIds::shared().render(&sdl))
        .expect("canonical journal catalog must parse");
    baseline
        .iter()
        .filter(|entry| {
            parsed.iter().any(|collection| {
                collection.name == entry.name
                    && collection.policy.as_ref().is_some_and(|policy| {
                        policy.id
                            == gents_protocol::schemas::HomePolicyIds::shared()
                                .id(gents_protocol::schemas::BuiltInPolicy::ClientJournal)
                            && policy.resource_name
                                == gents_protocol::schemas::CLIENT_JOURNAL_POLICY_RESOURCE
                    })
            })
        })
        .copied()
        .collect()
});

#[derive(Debug, thiserror::Error)]
pub enum JournalSchemaError {
    #[error("the client journal contains non-journal collection {collection}; choose a separate client history folder; the existing store is unchanged")]
    UnexpectedCollection { collection: String },
    #[error("reading the client journal schema")]
    Read(#[source] defra_node::Error),
    #[error("initializing the client journal schema")]
    Initialize(#[source] SchemaError),
    #[error(fmt = gents_protocol::error_chain::held)]
    Access(#[from] ConfigClientError),
}

pub async fn ensure_journal_schemas(node: Arc<Node>) -> Result<(), JournalSchemaError> {
    ConfigAccess::local(node)
        .run_local_owned(|node| async move {
            let existing = node
                .get_all_collection_versions()
                .await
                .map_err(JournalSchemaError::Read)?;
            for collection in existing {
                if !COLLECTIONS
                    .iter()
                    .any(|entry| entry.name == collection.name)
                {
                    return Err(JournalSchemaError::UnexpectedCollection {
                        collection: collection.name,
                    });
                }
            }
            let catalog = Catalog {
                baseline: COLLECTIONS.as_slice(),
                ordinary_indexes: &[],
            };
            super::catalog::ensure(node.as_ref(), &catalog)
                .await
                .map_err(JournalSchemaError::Initialize)
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::NodeIdentity as _;

    #[tokio::test]
    async fn private_journal_retains_submissions_without_a_runtime_schema_or_peer(
    ) -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let key =
            crate::identity::KeyIdentity::load_or_create(temp.path().join("client.key"), None)?;
        let data = temp.path().join("data");
        let open = || {
            Node::builder()
                .with_storage_backend(defra_node::StorageBackend::Regolith)
                .data_path(&data)
                .with_node_identity_did(key.did())
        };
        let node = Arc::new(open().build().await?);
        ensure_journal_schemas(node.clone()).await?;
        let collections = node.get_all_collection_versions().await?;
        assert!(!collections.is_empty());
        assert!(collections.iter().all(|collection| {
            !collection.is_branchable
                && collection.policy.as_ref().is_some_and(|policy| {
                    policy.id
                        == gents_protocol::schemas::HomePolicyIds::shared()
                            .id(gents_protocol::schemas::BuiltInPolicy::ClientJournal)
                        && policy.resource_name
                            == gents_protocol::schemas::CLIENT_JOURNAL_POLICY_RESOURCE
                })
        }));
        let access = ConfigAccess::local_as(node.clone(), identity::Did::new(key.did())?);
        access.write("test.journal.retain", r#"mutation {
            create_PendingChatSubmission(input: {request_id: "retained-goal", payload: "retained-client-intent"}) { _docID }
        }"#).await?;
        drop(access);
        node.shutdown().await?;
        drop(node);
        let node = Arc::new(open().build().await?);
        ensure_journal_schemas(node.clone()).await?;
        let access = ConfigAccess::local_as(node.clone(), identity::Did::new(key.did())?);
        let rows = access
            .read_view(&crate::views::journals::PendingSubmission {
                request_id: "retained-goal",
            })
            .await?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].payload, "retained-client-intent");
        drop(access);
        node.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn another_store_is_refused_before_registering_the_journal() -> anyhow::Result<()> {
        let node = Arc::new(crate::test_support::node_builder().build().await?);
        let access = ConfigAccess::local(node.clone());
        access
            .add_collection("type Unrelated { value: String }")
            .await?;
        assert!(matches!(ensure_journal_schemas(node.clone()).await,
            Err(JournalSchemaError::UnexpectedCollection { collection }) if collection == "Unrelated"));
        assert!(node
            .get_collection(gents_protocol::schemas::PENDING_CHAT_SUBMISSION_NAME)?
            .is_none());
        assert!(node.get_collection("Unrelated")?.is_some());
        node.shutdown().await?;
        Ok(())
    }
}
