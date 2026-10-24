//! Surface entries are either a create tool ([`WriteToolDecl`]) or a
//! single-collection query tool ([`QueryToolDecl`]). Stored under an `entries`
//! object envelope so DefraDB keeps the JSON scalar type.

use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};

use super::error::DocumentConfigError;
use serde::{Deserialize, Serialize};

use super::datastore_tool_surface::DatastoreToolSurfaceDocument;
use super::serde_helpers;
use super::tools::Tools;
use super::write_tool::{
    reject_protected_collection_name, reject_tool_name_surface_collisions,
    validate_write_tool_declarations, WriteToolDecl, WriteToolField, WriteToolFieldFill,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergedSurfaceTools {
    pub write_tools: Vec<WriteToolDecl>,
    pub query_tools: Vec<QueryToolDecl>,
    pub surface_of_tool: std::collections::BTreeMap<String, String>,
}

pub fn merge_datastore_tool_surfaces<'a>(
    selection: &Tools,
    surfaces: impl IntoIterator<Item = &'a DatastoreToolSurfaceDocument>,
) -> Result<MergedSurfaceTools, DocumentConfigError> {
    let mut surface_by_id = RapidHashMap::new();
    for surface in surfaces {
        let surface_id = surface.surface_id.as_str();
        if surface_id.trim().is_empty() {
            continue;
        }
        if surface_by_id
            .insert((surface.node_did.as_str(), surface_id), surface)
            .is_some()
        {
            return Err(DocumentConfigError::DuplicateDatastoreToolSurface {
                surface_id: surface_id.to_owned(),
            });
        }
    }

    let mut write_tools = Vec::new();
    let mut query_tools = Vec::new();
    let mut surface_of_tool = std::collections::BTreeMap::new();
    let mut seen = RapidHashSet::new();
    let surface_ids = selection
        .datastore
        .as_ref()
        .and_then(|datastore| datastore.datastore_tool_surface_ids.as_deref())
        .unwrap_or(&[]);
    let mut linked_ids = RapidHashSet::new();
    for surface_id in surface_ids {
        let surface_id = surface_id.as_str();
        if surface_id.trim().is_empty() {
            return Err(DocumentConfigError::ToolsEmptySurfaceId {
                tools_id: selection.tools_id.clone(),
            });
        }
        if !linked_ids.insert(surface_id) {
            return Err(DocumentConfigError::ToolsDuplicateSurfaceLink {
                tools_id: selection.tools_id.clone(),
                surface_id: surface_id.to_owned(),
            });
        }
        let surface = surface_by_id
            .get(&(selection.node_did.as_str(), surface_id))
            .copied()
            .ok_or_else(|| DocumentConfigError::ToolsMissingSurface {
                tools_id: selection.tools_id.clone(),
                surface_id: surface_id.to_owned(),
            })?;
        if !surface.enabled {
            return Err(DocumentConfigError::ToolsDisabledSurface {
                tools_id: selection.tools_id.clone(),
                surface_id: surface_id.to_owned(),
            });
        }
        for entry in surface.entries.as_deref().unwrap_or(&[]) {
            entry
                .validate()
                .map_err(|error| DocumentConfigError::SurfaceEntryMalformed {
                    surface_id: surface_id.to_owned(),
                    error: Box::new(error),
                })?;
            if !seen.insert(entry.tool_name().to_string()) {
                return Err(DocumentConfigError::DuplicateSurfaceToolName {
                    tool_name: entry.tool_name().to_owned(),
                    surface_id: surface_id.to_owned(),
                    tools_id: selection.tools_id.clone(),
                });
            }
            surface_of_tool.insert(entry.tool_name().to_string(), surface_id.to_owned());
            match entry {
                SurfaceToolDecl::Create(decl) => write_tools.push(decl.clone()),
                SurfaceToolDecl::Query(decl) => query_tools.push(decl.clone()),
            }
        }
    }

    Ok(MergedSurfaceTools {
        write_tools,
        query_tools,
        surface_of_tool,
    })
}

/// One bound read tool: one collection, a fixed projection, optional filter
/// fills. The model never names the collection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct QueryToolDecl {
    pub tool_name: String,
    pub collection: String,
    #[cfg_attr(feature = "typescript", ts(as = "Option<String>", optional))]
    pub description: String,
    /// Projection allowlist; omit `fields` at call time to return all of these.
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional))]
    pub fields: Vec<String>,
    #[cfg_attr(
        feature = "typescript",
        ts(as = "Option<Vec<WriteToolField>>", optional)
    )]
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
            tool_name: raw.tool_name,
            collection: raw.collection,
            description: raw.description,
            fields: raw.fields,
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

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(tag = "kind", rename_all = "snake_case"))]
pub enum SurfaceToolDecl {
    #[cfg_attr(feature = "typescript", ts(untagged))]
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

    pub fn validate(&self) -> Result<(), DocumentConfigError> {
        match self {
            Self::Create(decl) => {
                decl.validate()?;
                if !decl.output_obligation_is_well_formed() {
                    return Err(DocumentConfigError::OutputObligationMalformed {
                        tool_name: decl.tool_name.clone(),
                    });
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
    let value = match value {
        Some(Value::Object(mut envelope))
            if envelope.len() == 1 && envelope.contains_key("entries") =>
        {
            envelope.remove("entries").or(Some(Value::Object(envelope)))
        }
        value => value,
    };
    serde_helpers::deserialize_dual_shape(
        value,
        "DatastoreToolSurface.entries must be a list of create/query tool objects or JSON strings",
    )
    .map(Some)
    .map_err(D::Error::custom)
}

pub(crate) fn serialize_optional_surface_tools<S>(
    value: &Option<Vec<SurfaceToolDecl>>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    #[derive(Serialize)]
    struct StoredSurfaceTools<'a> {
        entries: &'a [SurfaceToolDecl],
    }

    match value {
        Some(entries) => StoredSurfaceTools { entries }.serialize(serializer),
        None => serializer.serialize_none(),
    }
}

pub(crate) fn validate_query_tool_declarations(
    decls: &[QueryToolDecl],
    cli_tool_names: &[String],
    additional_tool_names: &[String],
) -> Result<(), DocumentConfigError> {
    let cli_tool_names = cli_tool_names
        .iter()
        .map(|name| name.trim())
        .collect::<rapidhash::RapidHashSet<_>>();
    let additional_tool_names = additional_tool_names
        .iter()
        .map(|name| name.trim())
        .collect::<rapidhash::RapidHashSet<_>>();
    let mut seen_tool_names = rapidhash::RapidHashSet::default();
    for (i, decl) in decls.iter().enumerate() {
        if !decl.is_well_formed() {
            return Err(DocumentConfigError::QueryToolMalformed {
                index: i,
                tool_name: decl.tool_name.clone(),
                collection: decl.collection.clone(),
                fields: decl.fields.len(),
            });
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
            DocumentConfigError::QueryToolInvalidCollection {
                index: i,
                tool_name: decl.tool_name.clone(),
                collection: decl.collection.clone(),
                error,
            }
        })?;
        reject_protected_collection_name(&decl.collection)?;
        let mut seen_field_names = rapidhash::RapidHashSet::default();
        for (j, field) in decl.fields.iter().enumerate() {
            crate::graphql::validate_graphql_name(field).map_err(|error| {
                DocumentConfigError::QueryToolFieldNotName {
                    index: i,
                    tool_name: decl.tool_name.clone(),
                    field_index: j,
                    field: field.clone(),
                    error,
                }
            })?;
            if !seen_field_names.insert(field.as_str()) {
                return Err(DocumentConfigError::QueryToolDuplicateField {
                    index: i,
                    tool_name: decl.tool_name.clone(),
                    field: field.clone(),
                });
            }
        }
        let mut seen_filter_names = rapidhash::RapidHashSet::default();
        for (j, field) in decl.filter_fields.iter().enumerate() {
            if field.name.trim().is_empty() {
                return Err(DocumentConfigError::QueryToolFilterBlank {
                    index: i,
                    tool_name: decl.tool_name.clone(),
                    field_index: j,
                });
            }
            crate::graphql::validate_graphql_name(&field.name).map_err(|error| {
                DocumentConfigError::QueryToolFilterNotName {
                    index: i,
                    tool_name: decl.tool_name.clone(),
                    field_index: j,
                    name: field.name.clone(),
                    error,
                }
            })?;
            if matches!(
                field.name.trim(),
                "fields" | "limit" | "offset" | "order" | "field_page"
            ) {
                return Err(DocumentConfigError::QueryToolFilterReserved {
                    index: i,
                    tool_name: decl.tool_name.clone(),
                    field_index: j,
                    name: field.name.trim().to_owned(),
                });
            }
            if !seen_filter_names.insert(field.name.trim()) {
                return Err(DocumentConfigError::QueryToolDuplicateFilter {
                    index: i,
                    tool_name: decl.tool_name.clone(),
                    name: field.name.trim().to_owned(),
                });
            }
            if field.fill.is_some() && field.required {
                return Err(DocumentConfigError::QueryToolFilterRequiredFill {
                    index: i,
                    tool_name: decl.tool_name.clone(),
                    field_index: j,
                    name: field.name.clone(),
                });
            }
            if let Some(WriteToolFieldFill::SourceField(source_field)) = &field.fill {
                crate::graphql::validate_graphql_name(source_field).map_err(|error| {
                    DocumentConfigError::QueryToolSourceField {
                        index: i,
                        tool_name: decl.tool_name.clone(),
                        field_index: j,
                        source_field: source_field.clone(),
                        error,
                    }
                })?;
            }
        }
        if !seen_tool_names.insert(decl.tool_name.trim()) {
            return Err(DocumentConfigError::QueryToolDuplicateName {
                tool_name: decl.tool_name.trim().to_owned(),
            });
        }
    }
    Ok(())
}

pub(crate) fn validate_surface_tool_names(
    write_tools: &[WriteToolDecl],
    query_tools: &[QueryToolDecl],
    cli_tool_names: &[String],
    additional_tool_names: &[String],
) -> anyhow::Result<()> {
    validate_write_tool_declarations(write_tools, cli_tool_names, additional_tool_names)?;
    let mut extra = additional_tool_names.to_vec();
    extra.extend(write_tools.iter().map(|decl| decl.tool_name.clone()));
    validate_query_tool_declarations(query_tools, cli_tool_names, &extra)?;
    Ok(())
}
