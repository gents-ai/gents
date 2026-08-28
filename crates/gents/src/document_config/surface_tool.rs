//! Surface entries are either a create tool ([`WriteToolDecl`]) or a
//! single-collection query tool ([`QueryToolDecl`]).
//!
//! Stored in `DatastoreToolSurface.entries` as JSON strings. Existing create
//! entries have no `kind` and stay [`SurfaceToolDecl::Create`]. Query entries
//! set `"kind": "query"`.

use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};

use super::datastore_tool_surface::DatastoreToolSurfaceDocument;
use super::serde_helpers;
use super::tool_selection::{
    reject_tool_name_surface_collisions, validate_write_tool_declarations, ToolSelectionDocument,
    WriteToolDecl, WriteToolField, WriteToolFieldFill,
};

/// Create and query tools after expanding linked [`DatastoreToolSurfaceDocument`]s.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergedSurfaceTools {
    pub write_tools: Vec<WriteToolDecl>,
    pub query_tools: Vec<QueryToolDecl>,
}

/// Merge inline write declarations with a selection's linked datastore surfaces.
///
/// The operation fails closed when a selected surface is missing, disabled,
/// foreign-owned, malformed, duplicated, or introduces a tool-name collision.
pub fn merge_datastore_tool_surfaces<'a>(
    selection: &ToolSelectionDocument,
    surfaces: impl IntoIterator<Item = &'a DatastoreToolSurfaceDocument>,
) -> Result<MergedSurfaceTools> {
    let mut surface_by_id = HashMap::new();
    for surface in surfaces {
        let surface_id = surface.surface_id.trim();
        if surface_id.is_empty() {
            continue;
        }
        if surface_by_id.insert(surface_id, surface).is_some() {
            bail!("duplicate DatastoreToolSurface {surface_id}");
        }
    }

    let mut write_tools = selection.write_tools.clone().unwrap_or_default();
    let mut query_tools = Vec::new();
    let mut seen = HashSet::new();
    for decl in &write_tools {
        if !seen.insert(decl.tool_name.clone()) {
            bail!(
                "ToolSelection {} has duplicate write_tools tool_name {:?}",
                selection.selection_id,
                decl.tool_name
            );
        }
    }

    let surface_ids = selection
        .datastore_tool_surface_ids
        .as_deref()
        .unwrap_or(&[]);
    let mut linked_ids = HashSet::new();
    for surface_id in surface_ids {
        let surface_id = surface_id.trim();
        if surface_id.is_empty() {
            bail!(
                "ToolSelection {} has an empty datastore_tool_surface_ids entry",
                selection.selection_id
            );
        }
        if !linked_ids.insert(surface_id) {
            bail!(
                "ToolSelection {} lists DatastoreToolSurface {} more than once",
                selection.selection_id,
                surface_id
            );
        }
        let surface = surface_by_id.get(surface_id).copied().ok_or_else(|| {
            anyhow!(
                "ToolSelection {} references missing DatastoreToolSurface {}",
                selection.selection_id,
                surface_id
            )
        })?;
        if surface.agent_did.trim() != selection.agent_did.trim() {
            bail!(
                "ToolSelection {} references DatastoreToolSurface {} owned by a different agent",
                selection.selection_id,
                surface_id
            );
        }
        if !surface.enabled {
            bail!(
                "ToolSelection {} references disabled DatastoreToolSurface {}",
                selection.selection_id,
                surface_id
            );
        }
        for entry in surface.entries.as_deref().unwrap_or(&[]) {
            entry.validate().map_err(|error| {
                anyhow!("DatastoreToolSurface {surface_id} has a malformed entry: {error}")
            })?;
            if !seen.insert(entry.tool_name().to_string()) {
                bail!(
                    "duplicate tool_name {:?} after expanding DatastoreToolSurface {} for ToolSelection {}",
                    entry.tool_name(),
                    surface_id,
                    selection.selection_id
                );
            }
            match entry {
                SurfaceToolDecl::Create(decl) => write_tools.push(decl.clone()),
                SurfaceToolDecl::Query(decl) => query_tools.push(decl.clone()),
            }
        }
    }

    Ok(MergedSurfaceTools {
        write_tools,
        query_tools,
    })
}

/// One bound read tool: one collection, a fixed projection, optional filter
/// fills. The model never names the collection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QueryToolDecl {
    pub tool_name: String,
    pub collection: String,
    pub description: String,
    /// Projection allowlist. Omit `fields` at call time to return all of these.
    pub fields: Vec<String>,
    /// Filter slots. Runtime-filled entries are hidden from the model and
    /// applied as `_eq` clauses; the rest are optional/required string args.
    pub filter_fields: Vec<WriteToolField>,
}

impl<'de> Deserialize<'de> for QueryToolDecl {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default)]
            kind: Option<String>,
            tool_name: String,
            collection: String,
            #[serde(default)]
            description: String,
            #[serde(default)]
            fields: Vec<String>,
            #[serde(default)]
            filter_fields: Vec<WriteToolField>,
        }
        let raw = Raw::deserialize(deserializer)?;
        if let Some(kind) = raw.kind.as_deref() {
            let kind = kind.trim();
            if !kind.is_empty() && !matches!(kind, "query" | "read") {
                return Err(serde::de::Error::custom(format!(
                    "query tool kind must be \"query\" (or \"read\"), got {kind:?}"
                )));
            }
        }
        Ok(Self {
            tool_name: raw.tool_name.trim().to_string(),
            collection: raw.collection.trim().to_string(),
            description: raw.description,
            fields: raw
                .fields
                .into_iter()
                .map(|field| field.trim().to_string())
                .filter(|field| !field.is_empty())
                .collect(),
            filter_fields: raw.filter_fields,
        })
    }
}

impl QueryToolDecl {
    pub fn is_well_formed(&self) -> bool {
        !self.tool_name.trim().is_empty()
            && !self.collection.trim().is_empty()
            && !self.fields.is_empty()
    }
}

/// One `DatastoreToolSurface` entry: create (the original write decl) or query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceToolDecl {
    Create(WriteToolDecl),
    Query(QueryToolDecl),
}

impl SurfaceToolDecl {
    pub fn tool_name(&self) -> &str {
        match self {
            Self::Create(decl) => decl.tool_name.as_str(),
            Self::Query(decl) => decl.tool_name.as_str(),
        }
    }

    pub fn collection(&self) -> &str {
        match self {
            Self::Create(decl) => decl.collection.as_str(),
            Self::Query(decl) => decl.collection.as_str(),
        }
    }

    pub fn is_well_formed(&self) -> bool {
        self.validate().is_ok()
    }

    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Create(decl) => {
                decl.validate()?;
                if !decl.output_obligation_is_well_formed() {
                    anyhow::bail!(
                        "entry {:?} output_obligation.minimum_writes must be greater than zero and output_obligation.expected_count_field, when present, must name a required model-provided field",
                        decl.tool_name,
                    );
                }
                Ok(())
            }
            Self::Query(decl) => {
                validate_query_tool_declarations(std::slice::from_ref(decl), &[], &[])
            }
        }
    }
}

impl Serialize for SurfaceToolDecl {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Create(decl) => decl.serialize(serializer),
            Self::Query(decl) => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(6))?;
                map.serialize_entry("kind", "query")?;
                map.serialize_entry("tool_name", &decl.tool_name)?;
                map.serialize_entry("collection", &decl.collection)?;
                map.serialize_entry("description", &decl.description)?;
                map.serialize_entry("fields", &decl.fields)?;
                map.serialize_entry("filter_fields", &decl.filter_fields)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for SurfaceToolDecl {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let kind = value
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|kind| !kind.is_empty())
            .unwrap_or("create");
        match kind {
            "query" | "read" => Ok(Self::Query(
                serde_json::from_value(value).map_err(serde::de::Error::custom)?,
            )),
            "create" | "write" => Ok(Self::Create(
                serde_json::from_value(value).map_err(serde::de::Error::custom)?,
            )),
            other => Err(serde::de::Error::custom(format!(
                "surface entry kind must be \"create\" or \"query\", got {other:?}"
            ))),
        }
    }
}

pub(crate) fn deserialize_optional_surface_tools<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Vec<SurfaceToolDecl>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;
    use serde_json::Value;

    let value = Option::<Value>::deserialize(deserializer)?;
    if matches!(value, None | Some(Value::Null)) {
        return Ok(None);
    }
    serde_helpers::deserialize_dual_shape(
        value,
        "DatastoreToolSurface.entries must be a list of create/query tool objects or JSON strings",
    )
    .map(Some)
    .map_err(D::Error::custom)
}

pub(crate) fn validate_query_tool_declarations(
    decls: &[QueryToolDecl],
    cli_tool_names: &[String],
    additional_tool_names: &[String],
) -> Result<()> {
    let cli_tool_names = cli_tool_names
        .iter()
        .map(|name| name.trim())
        .collect::<std::collections::HashSet<_>>();
    let additional_tool_names = additional_tool_names
        .iter()
        .map(|name| name.trim())
        .collect::<std::collections::HashSet<_>>();
    let mut seen_tool_names = std::collections::HashSet::new();
    for (i, decl) in decls.iter().enumerate() {
        if !decl.is_well_formed() {
            return Err(anyhow::anyhow!(
                "query_tools[{i}] is malformed (tool_name, collection, and at least one projection field are required): tool_name={:?}, collection={:?}, fields={}",
                decl.tool_name,
                decl.collection,
                decl.fields.len()
            ));
        }
        reject_tool_name_surface_collisions(
            "query_tools",
            i,
            &decl.tool_name,
            "query tools",
            &cli_tool_names,
            &additional_tool_names,
        )?;
        crate::graphql::validate_collection_identifier(&decl.collection).map_err(|error| {
            anyhow::anyhow!(
                "query_tools[{i}] (tool {:?}) has invalid collection {:?}: {error}",
                decl.tool_name,
                decl.collection
            )
        })?;
        let mut seen_field_names = std::collections::HashSet::new();
        for (j, field) in decl.fields.iter().enumerate() {
            crate::graphql::validate_graphql_name(field).map_err(|error| {
                anyhow::anyhow!(
                    "query_tools[{i}] (tool {:?}) fields[{j}] {:?} is not a GraphQL name: {error}",
                    decl.tool_name,
                    field
                )
            })?;
            if !seen_field_names.insert(field.as_str()) {
                return Err(anyhow::anyhow!(
                    "query_tools[{i}] (tool {:?}) has a duplicate projection field {:?}; each field must be unique",
                    decl.tool_name,
                    field
                ));
            }
        }
        let mut seen_filter_names = std::collections::HashSet::new();
        for (j, field) in decl.filter_fields.iter().enumerate() {
            if field.name.trim().is_empty() {
                return Err(anyhow::anyhow!(
                    "query_tools[{i}] (tool {:?}) has a filter_fields[{j}] with an empty name",
                    decl.tool_name
                ));
            }
            crate::graphql::validate_graphql_name(&field.name).map_err(|error| {
                anyhow::anyhow!(
                    "query_tools[{i}] (tool {:?}) filter_fields[{j}] {:?} is not a GraphQL name: {error}",
                    decl.tool_name,
                    field.name
                )
            })?;
            if matches!(field.name.trim(), "fields" | "limit") {
                return Err(anyhow::anyhow!(
                    "query_tools[{i}] (tool {:?}) filter_fields[{j}] {:?} collides with a reserved query argument",
                    decl.tool_name,
                    field.name.trim()
                ));
            }
            if !seen_filter_names.insert(field.name.trim()) {
                return Err(anyhow::anyhow!(
                    "query_tools[{i}] (tool {:?}) has a duplicate filter field {:?}; each filter_fields name must be unique",
                    decl.tool_name,
                    field.name.trim()
                ));
            }
            if field.fill.is_some() && field.required {
                return Err(anyhow::anyhow!(
                    "query_tools[{i}] (tool {:?}) filter_fields[{j}] {:?} is runtime-filled and cannot be required",
                    decl.tool_name,
                    field.name
                ));
            }
            if let Some(WriteToolFieldFill::SourceField(source_field)) = &field.fill {
                crate::graphql::validate_graphql_name(source_field).map_err(|error| {
                    anyhow::anyhow!(
                        "query_tools[{i}] (tool {:?}) filter_fields[{j}] has invalid source_field {:?}: {error}",
                        decl.tool_name,
                        source_field
                    )
                })?;
            }
        }
        if !seen_tool_names.insert(decl.tool_name.trim()) {
            return Err(anyhow::anyhow!(
                "query_tools has a duplicate tool_name {:?}; each declared query tool must have a unique tool_name",
                decl.tool_name.trim()
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_surface_tool_names(
    write_tools: &[WriteToolDecl],
    query_tools: &[QueryToolDecl],
    cli_tool_names: &[String],
    additional_tool_names: &[String],
) -> Result<()> {
    validate_write_tool_declarations(write_tools, cli_tool_names, additional_tool_names)?;
    let mut extra = additional_tool_names.to_vec();
    extra.extend(write_tools.iter().map(|decl| decl.tool_name.clone()));
    validate_query_tool_declarations(query_tools, cli_tool_names, &extra)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_entry_without_kind_round_trips() {
        let decl = WriteToolDecl {
            tool_name: "write_finding".into(),
            collection: "Finding".into(),
            description: "create one finding".into(),
            fields: vec![WriteToolField {
                name: "title".into(),
                required: true,
                fill: None,
            }],
            output_obligation: None,
        };
        let json = serde_json::to_value(SurfaceToolDecl::Create(decl.clone())).unwrap();
        assert!(json.get("kind").is_none());
        let parsed: SurfaceToolDecl = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, SurfaceToolDecl::Create(decl));
    }

    #[test]
    fn query_entry_requires_kind_and_projection() {
        let decl = QueryToolDecl {
            tool_name: "query_finding".into(),
            collection: "Finding".into(),
            description: "load findings".into(),
            fields: vec!["finding_id".into(), "title".into()],
            filter_fields: vec![WriteToolField {
                name: "run_id".into(),
                required: false,
                fill: Some(WriteToolFieldFill::Correlation),
            }],
        };
        let json = serde_json::to_value(SurfaceToolDecl::Query(decl.clone())).unwrap();
        assert_eq!(json["kind"], "query");
        let parsed: SurfaceToolDecl = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, SurfaceToolDecl::Query(decl));
    }

    #[test]
    fn query_decl_rejects_empty_projection() {
        let decl = QueryToolDecl {
            tool_name: "query_finding".into(),
            collection: "Finding".into(),
            description: String::new(),
            fields: Vec::new(),
            filter_fields: Vec::new(),
        };
        assert!(!decl.is_well_formed());
        let err = validate_query_tool_declarations(&[decl], &[], &[]).unwrap_err();
        assert!(err.to_string().contains("at least one projection"));
    }

    #[test]
    fn query_decl_rejects_reserved_filter_names() {
        let decl = QueryToolDecl {
            tool_name: "query_finding".into(),
            collection: "Finding".into(),
            description: String::new(),
            fields: vec!["title".into()],
            filter_fields: vec![WriteToolField {
                name: "limit".into(),
                required: true,
                fill: None,
            }],
        };
        let err = validate_query_tool_declarations(&[decl], &[], &[]).unwrap_err();
        assert!(err.to_string().contains("reserved query argument"));
    }
}
