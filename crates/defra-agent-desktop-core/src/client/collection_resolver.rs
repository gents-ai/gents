use std::collections::HashMap;
use std::sync::RwLock;

use anyhow::{anyhow, Result};
use defra_agent_protocol::schemas::{ALL_COLLECTION_NAMES, RUNTIME_COLLECTION_NAMES};
use defra_node::EmbeddedNode;

/// Cache of `collection_id → static collection name`. The DefraDB Update
/// event carries only the stable `collection_id` string; consumers usually
/// want the human-readable name. Collection IDs never change for the
/// lifetime of a collection, so entries are never invalidated.
#[derive(Default)]
pub struct CollectionResolver {
    cache: RwLock<HashMap<String, &'static str>>,
}

impl CollectionResolver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve `collection_id` to its static collection name. On cache miss,
    /// rebuild the full id→name index by walking `ALL_COLLECTION_NAMES`.
    /// Returns `None` if the id does not match any known collection.
    pub async fn resolve(
        &self,
        node: &EmbeddedNode,
        collection_id: &str,
    ) -> Result<Option<&'static str>> {
        if let Some(name) = self
            .cache
            .read()
            .expect("collection resolver lock poisoned")
            .get(collection_id)
            .copied()
        {
            return Ok(Some(name));
        }

        for name in ALL_COLLECTION_NAMES.iter().chain(RUNTIME_COLLECTION_NAMES.iter()) {
            let collection = node
                .get_collection(name)
                .map_err(|e| anyhow!("get_collection({name}) failed: {e}"))?;
            if let Some(c) = collection {
                self.cache
                    .write()
                    .expect("collection resolver lock poisoned")
                    .insert(c.collection_id.clone(), *name);
            }
        }

        Ok(self
            .cache
            .read()
            .expect("collection resolver lock poisoned")
            .get(collection_id)
            .copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::schema::ensure_runtime_schemas;
    use defra_agent_protocol::schemas::{AGENT_MESSAGE_NAME, INFERENCE_BACKEND_NAME};
    use defra_node::NodeBuilder;
    use std::sync::Arc;

    #[tokio::test]
    async fn resolve_returns_name_for_known_collection_id() {
        let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
        ensure_runtime_schemas(node.as_ref()).await.expect("schemas");
        let resolver = CollectionResolver::new();

        let collection_id = node
            .get_collection(AGENT_MESSAGE_NAME)
            .expect("get_collection")
            .expect("collection exists")
            .collection_id;

        let name = resolver
            .resolve(node.as_ref(), &collection_id)
            .await
            .expect("resolve");
        assert_eq!(name, Some(AGENT_MESSAGE_NAME));
    }

    #[tokio::test]
    async fn resolve_returns_none_for_unknown_id() {
        let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
        ensure_runtime_schemas(node.as_ref()).await.expect("schemas");
        let resolver = CollectionResolver::new();

        let name = resolver
            .resolve(node.as_ref(), "does-not-exist")
            .await
            .expect("resolve");
        assert_eq!(name, None);
    }

    #[tokio::test]
    async fn resolve_returns_name_for_inference_backend() {
        let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
        ensure_runtime_schemas(node.as_ref()).await.expect("schemas");
        let resolver = CollectionResolver::new();

        let collection_id = node
            .get_collection(INFERENCE_BACKEND_NAME)
            .expect("get_collection")
            .expect("collection exists")
            .collection_id;

        let name = resolver
            .resolve(node.as_ref(), &collection_id)
            .await
            .expect("resolve");
        assert_eq!(name, Some(INFERENCE_BACKEND_NAME));
    }

    #[tokio::test]
    async fn resolve_caches_after_first_call() {
        let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
        ensure_runtime_schemas(node.as_ref()).await.expect("schemas");
        let resolver = CollectionResolver::new();

        let collection_id = node
            .get_collection(AGENT_MESSAGE_NAME)
            .expect("get_collection")
            .expect("collection")
            .collection_id;

        let _ = resolver.resolve(node.as_ref(), &collection_id).await.unwrap();
        let cache_size = resolver
            .cache
            .read()
            .expect("lock")
            .len();
        assert!(cache_size >= 1, "expected cache populated; got {cache_size}");
    }
}
