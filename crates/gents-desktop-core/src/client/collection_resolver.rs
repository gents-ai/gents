use rapidhash::RapidHashMap;
use std::sync::RwLock;

use defra_node::EmbeddedNode;
use gents_protocol::schemas::{
    ALL_COLLECTION_NAMES, INFERENCE_CALL_NAME, RUNTIME_COLLECTION_NAMES,
};

#[derive(Debug, thiserror::Error)]
pub enum CollectionResolverError {
    #[error("get_collection({name}) failed: {error}")]
    GetCollection {
        name: &'static str,
        error: defra_node::Error,
    },
}

type Result<T, E = CollectionResolverError> = std::result::Result<T, E>;

/// Collection IDs never change for the lifetime of a collection, so entries
/// are never invalidated. Unknown IDs may become available through schema
/// replication; only known ignored collections can be negatively cached.
#[derive(Default)]
pub struct CollectionResolver {
    cache: RwLock<RapidHashMap<String, Option<&'static str>>>,
}

impl CollectionResolver {
    pub fn new() -> Self {
        Self::default()
    }

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
            return Ok(name);
        }

        for name in ALL_COLLECTION_NAMES
            .iter()
            .chain(RUNTIME_COLLECTION_NAMES.iter())
        {
            let collection = node
                .get_collection(name)
                .map_err(|error| CollectionResolverError::GetCollection { name, error })?;
            if let Some(c) = collection {
                self.cache
                    .write()
                    .expect("collection resolver lock poisoned")
                    .insert(
                        c.collection_id.clone(),
                        (*name != INFERENCE_CALL_NAME).then_some(*name),
                    );
            }
        }

        Ok(self
            .cache
            .read()
            .expect("collection resolver lock poisoned")
            .get(collection_id)
            .copied()
            .flatten())
    }
}
