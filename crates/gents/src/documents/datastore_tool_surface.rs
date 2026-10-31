use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::store::config_client::ConfigAccess;
use crate::store::graphql::escape_graphql_string;

use super::surface_tool::{
    deserialize_optional_surface_tools, serialize_optional_surface_tools, SurfaceToolDecl,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct DatastoreToolSurfaceDocument {
    pub surface_id: String,
    pub node_did: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub display_name: Option<String>,
    #[serde(
        default = "super::serde_helpers::default_enabled",
        deserialize_with = "super::serde_helpers::deserialize_enabled",
        skip_serializing_if = "super::serde_helpers::is_enabled"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<bool>", optional = nullable))]
    pub enabled: bool,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_surface_tools",
        serialize_with = "serialize_optional_surface_tools"
    )]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub entries: Option<Vec<SurfaceToolDecl>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub created_at: Option<String>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tags: Vec<String>,
}

/// Through the embedded node or a served node's GraphQL endpoint.
pub async fn list_datastore_tool_surfaces(
    access: &ConfigAccess,
    node_did: &str,
) -> Result<Vec<DatastoreToolSurfaceDocument>> {
    anyhow::ensure!(!node_did.trim().is_empty(), "surface owner is required");
    let (fields, _) = crate::store::config_client::config_projection(
        crate::Collection::DatastoreToolSurface,
        None,
    )?;
    let owner = escape_graphql_string(node_did);
    let query = format!(
        "{{ DatastoreToolSurface(filter: {{ node_did: {{ _eq: \"{owner}\" }} }}) {{ {} }} }}",
        fields.join(" "),
    );
    let response = access.execute(&query).await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get("DatastoreToolSurface"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("surface query returned no row array"))?;
    let mut ids = rapidhash::RapidHashSet::default();
    rows.iter()
        .map(|row| {
            let surface: DatastoreToolSurfaceDocument = serde_json::from_value(row.clone())?;
            anyhow::ensure!(surface.node_did == node_did, "surface owner mismatch");
            anyhow::ensure!(
                ids.insert(surface.surface_id.clone()),
                "duplicate surface identity within owner"
            );
            Ok(surface)
        })
        .collect()
}
