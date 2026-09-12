//! Shared embedded-node collection token resolution for P2P admin adapters.

use crate::defra_node::EmbeddedNode;

use super::{RemoteP2pAdminError, RemoteP2pAdminResult};

pub fn resolve_embedded_collection_id(
    node: &EmbeddedNode,
    name: &str,
) -> RemoteP2pAdminResult<Option<String>> {
    match node.get_collection(name) {
        Ok(Some(definition)) => Ok(Some(definition.collection_id)),
        Ok(None) => Ok(None),
        Err(error) => Err(RemoteP2pAdminError::LocalError(format!(
            "resolve_collection_id({name}): {error}"
        ))),
    }
}

pub fn resolve_embedded_collection_name(
    node: &EmbeddedNode,
    token: &str,
) -> RemoteP2pAdminResult<Option<String>> {
    match node.get_collection(token) {
        Ok(Some(definition)) => return Ok(Some(definition.name)),
        Ok(None) => {}
        Err(error) => {
            return Err(RemoteP2pAdminError::LocalError(format!(
                "resolve_collection_name({token}) as name: {error}"
            )))
        }
    }

    let names = node.list_collections().map_err(|error| {
        RemoteP2pAdminError::LocalError(format!("list_collections for id {token}: {error}"))
    })?;
    for name in names {
        match node.get_collection(&name) {
            Ok(Some(definition)) if definition.collection_id == token => {
                return Ok(Some(definition.name));
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    collection_name = %name,
                    %error,
                    "resolve_collection_name failed to fetch a collection definition"
                );
            }
        }
    }
    Ok(None)
}
