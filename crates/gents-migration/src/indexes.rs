use defra_node::EmbeddedNode;
use schema::{IndexKind, OrderedIndexDescription};

use crate::registry::{Registry, CLIENT_AUTHORED_COLLECTIONS};
use crate::Result;

/// Ordinary index metadata is local and excluded from collection identity.
/// Client-authored collections must reconcile it through the same installation
/// owner on both peers, without introducing a collection migration lineage.
pub(crate) async fn ensure_indexes(node: &EmbeddedNode, registry: &Registry<'_>) -> Result<()> {
    for baseline in registry.baseline {
        if !CLIENT_AUTHORED_COLLECTIONS.contains(&baseline.name) {
            continue;
        }
        for collection in query::parse_sdl(baseline.sdl).map_err(anyhow::Error::new)? {
            for index in collection.indexes {
                let index = index.normalized();
                if index.kind
                    != Some(IndexKind::Ordered(OrderedIndexDescription {
                        unique: false,
                    }))
                {
                    continue;
                }
                node.ensure_ordinary_index(&collection.name, &index.name, index.fields)
                    .await?;
            }
        }
    }
    Ok(())
}
