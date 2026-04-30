use anyhow::{Context, Result};
use defra_agent_protocol::schemas::{
    ALL, ALL_COLLECTION_NAMES, BRANCHABLE_COLLECTION_NAMES, RUNTIME_ALL, RUNTIME_COLLECTION_NAMES,
};
use defra_node::EmbeddedNode;

async fn ensure_schema_set(node: &EmbeddedNode, schemas: &[&str]) -> Result<()> {
    for sdl in schemas {
        match node.add_schema(sdl).await {
            Ok(()) => {}
            Err(error) => {
                if error.to_string().contains("already exists") {
                    tracing::debug!(
                        schema = %sdl.lines().next().unwrap_or(""),
                        "schema already exists"
                    );
                } else {
                    return Err(error);
                }
            }
        }
    }

    Ok(())
}

pub async fn ensure_runtime_schemas(node: &EmbeddedNode) -> Result<()> {
    ensure_schema_set(node, RUNTIME_ALL).await?;
    ensure_schemas(node).await
}

pub async fn ensure_schemas(node: &EmbeddedNode) -> Result<()> {
    ensure_schema_set(node, ALL).await
}

pub async fn subscribe_all_collections(node: &EmbeddedNode) -> Result<()> {
    let p2p = node.p2p().context("desktop node missing P2P support")?;

    for name in subscribed_collection_names() {
        match p2p.add_collections(vec![name.to_owned()]).await {
            Ok(()) => {}
            Err(error) => {
                if error.to_string().contains("already") {
                    tracing::debug!(collection = name, "collection already subscribed");
                } else {
                    return Err(error.into());
                }
            }
        }
    }

    Ok(())
}

pub fn subscribed_collection_names() -> Vec<&'static str> {
    RUNTIME_COLLECTION_NAMES
        .iter()
        .chain(ALL_COLLECTION_NAMES.iter())
        .copied()
        .collect()
}

pub fn branchable_collection_names() -> Vec<&'static str> {
    BRANCHABLE_COLLECTION_NAMES.to_vec()
}
