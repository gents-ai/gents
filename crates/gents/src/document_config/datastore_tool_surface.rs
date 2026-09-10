//! Apply-owned `DatastoreToolSurface` documents.

use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use crate::graphql::escape_graphql_string;

use super::surface_tool::{deserialize_optional_surface_tools, SurfaceToolDecl};

/// Document-layer view of a `DatastoreToolSurface` row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct DatastoreToolSurfaceDocument {
    pub surface_id: String,
    pub agent_did: String,
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
    /// Canonical create/query tool declarations selected through Tools.datastore.
    #[serde(default, deserialize_with = "deserialize_optional_surface_tools")]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub entries: Option<Vec<SurfaceToolDecl>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub created_at: Option<String>,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tags: Vec<String>,
}

/// List canonical surface configurations owned by the selected principal.
pub async fn list_datastore_tool_surfaces(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<Vec<DatastoreToolSurfaceDocument>> {
    anyhow::ensure!(!agent_did.trim().is_empty(), "surface owner is required");
    let (fields, _) =
        crate::config_client::config_projection(crate::Collection::DatastoreToolSurface, None)?;
    let owner = escape_graphql_string(agent_did);
    let query = format!(
        "{{ DatastoreToolSurface(filter: {{ agent_did: {{ _eq: \"{owner}\" }} }}) {{ {} }} }}",
        fields.join(" "),
    );
    let response = node.execute(&query).await;
    anyhow::ensure!(
        !response.has_errors(),
        "list DatastoreToolSurface failed: {:?}",
        response.errors
    );
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("DatastoreToolSurface"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("surface query returned no row array"))?;
    let mut ids = std::collections::HashSet::new();
    rows.iter()
        .map(|row| {
            let surface: DatastoreToolSurfaceDocument = serde_json::from_value(row.clone())?;
            anyhow::ensure!(surface.agent_did == agent_did, "surface owner mismatch");
            anyhow::ensure!(
                ids.insert(surface.surface_id.clone()),
                "duplicate surface identity within owner"
            );
            Ok(surface)
        })
        .collect()
}
