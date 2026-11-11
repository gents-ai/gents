use anyhow::Result;

use crate::PortSpec;

/// A plugin writes only a pack's own collections: never the runtime's
/// documents and never a protected collection.
pub fn writable_output_collection(collection: &str) -> Result<()> {
    gents_store::graphql::validate_collection_identifier(collection)?;
    anyhow::ensure!(
        !gents_protocol::schemas::ALL_COLLECTION_NAMES.contains(&collection),
        "a plugin cannot write {collection}, which belongs to the runtime"
    );
    crate::reject_protected_collection_name(collection)
}

pub fn validate_handler(
    plugin: &str,
    digest: &str,
    correlation_field: Option<&str>,
    outputs: &[PortSpec],
) -> Result<()> {
    crate::plugin_coordinate::parse_coordinate(plugin)?;
    let hex = digest.strip_prefix("sha256:").unwrap_or_default();
    anyhow::ensure!(
        hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()),
        "callback plugin {plugin} pins {digest:?}, which is not sha256:<64 hex>"
    );
    if let Some(field) = correlation_field {
        gents_store::graphql::validate_graphql_name(field)?;
    }
    let mut names = std::collections::BTreeSet::new();
    for output in outputs {
        writable_output_collection(&output.collection)?;
        gents_store::graphql::validate_graphql_name(&output.correlation_field)?;
        anyhow::ensure!(
            names.insert(output.name.as_str()),
            "callback plugin {plugin} declares output {:?} twice",
            output.name
        );
        anyhow::ensure!(
            correlation_field.is_some(),
            "callback plugin {plugin} writes {} with a correlation field but reads none from its source",
            output.collection
        );
    }
    Ok(())
}
