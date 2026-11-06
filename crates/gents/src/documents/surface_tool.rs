//! Surface entries are either a create tool ([`WriteToolDecl`]) or a
//! single-collection query tool ([`QueryToolDecl`]). Stored under an `entries`
//! object envelope so DefraDB keeps the JSON scalar type.

use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};

use anyhow::{anyhow, bail, Result};
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

pub(crate) fn validate_datastore_surface(surface: &DatastoreToolSurfaceDocument) -> Result<()> {
    anyhow::ensure!(
        surface.enabled,
        "surface {:?} is disabled",
        surface.surface_id
    );
    let mut names = RapidHashSet::new();
    for entry in surface.entries.as_deref().unwrap_or(&[]) {
        entry.validate().map_err(|error| {
            anyhow!(
                "DatastoreToolSurface {} has a malformed entry: {error}",
                surface.surface_id
            )
        })?;
        anyhow::ensure!(
            names.insert(entry.tool_name()),
            "surface {:?} declares tool {:?} twice",
            surface.surface_id,
            entry.tool_name()
        );
    }
    Ok(())
}

pub(crate) fn confirm_visible_datastore_tool(
    surface: Option<&DatastoreToolSurfaceDocument>,
    expected: &SurfaceToolDecl,
) -> Result<()> {
    let surface = surface.ok_or_else(|| anyhow!("datastore surface is unavailable"))?;
    validate_datastore_surface(surface)?;
    anyhow::ensure!(
        surface
            .entries
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .any(|entry| entry == expected),
        "datastore tool declaration is no longer present"
    );
    Ok(())
}

pub(crate) async fn revalidate_visible_datastore_tool_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    surface_id: &str,
    expected: &SurfaceToolDecl,
) -> Result<()> {
    anyhow::ensure!(
        txn.actor_did().is_some(),
        "datastore tool actor is required"
    );
    let surface = crate::store::config_client::read_desired_state_document_in_txn(
        txn,
        crate::Collection::DatastoreToolSurface,
        owner,
        surface_id,
    )
    .await?
    .map(serde_json::from_value::<DatastoreToolSurfaceDocument>)
    .transpose()?;
    confirm_visible_datastore_tool(surface.as_ref(), expected)
}

#[derive(Clone)]
pub(crate) enum DatastoreSurfaceScope {
    SelectedAgent(String),
    VisibleCaller(String),
}

impl DatastoreSurfaceScope {
    pub(crate) async fn revalidate(
        &self,
        txn: &crate::store::config_client::ConfigApplyTxn<'_>,
        surface_id: &str,
        expected: &SurfaceToolDecl,
    ) -> Result<()> {
        match self {
            Self::SelectedAgent(owner) => {
                revalidate_datastore_tool_in_txn(txn, owner, surface_id, expected).await
            }
            Self::VisibleCaller(owner) => {
                revalidate_visible_datastore_tool_in_txn(txn, owner, surface_id, expected).await
            }
        }
    }
}

pub fn merge_datastore_tool_surfaces<'a>(
    selection: &Tools,
    surfaces: impl IntoIterator<Item = &'a DatastoreToolSurfaceDocument>,
) -> Result<MergedSurfaceTools> {
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
            bail!("duplicate DatastoreToolSurface {surface_id}");
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
            bail!(
                "Tools {} has an empty datastore_tool_surface_ids entry",
                selection.tools_id
            );
        }
        if !linked_ids.insert(surface_id) {
            bail!(
                "Tools {} lists DatastoreToolSurface {} more than once",
                selection.tools_id,
                surface_id
            );
        }
        let surface = surface_by_id
            .get(&(selection.node_did.as_str(), surface_id))
            .copied()
            .ok_or_else(|| {
                anyhow!(
                    "Tools {} references missing same-agent DatastoreToolSurface {}",
                    selection.tools_id,
                    surface_id
                )
            })?;
        validate_datastore_surface(surface)?;
        for entry in surface.entries.as_deref().unwrap_or(&[]) {
            if !seen.insert(entry.tool_name().to_string()) {
                bail!(
                    "duplicate tool_name {:?} after expanding DatastoreToolSurface {} for Tools {}",
                    entry.tool_name(),
                    surface_id,
                    selection.tools_id
                );
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

pub(crate) async fn revalidate_datastore_tool_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    surface_id: &str,
    expected: &SurfaceToolDecl,
) -> Result<()> {
    let actor = txn
        .actor_did()
        .ok_or_else(|| anyhow!("datastore tool actor is required"))?;
    let (_, tools) = super::load_agent_context_and_tools_in_txn(txn, owner, actor).await?;
    let tools = tools.ok_or_else(|| anyhow!("Agent has no selected Tools"))?;
    let ids = tools
        .datastore
        .as_ref()
        .and_then(|datastore| datastore.datastore_tool_surface_ids.as_deref())
        .unwrap_or(&[]);
    let borrowed = ids.iter().map(String::as_str).collect::<Vec<_>>();
    let rows = crate::store::config_client::read_desired_state_records_in_txn(
        txn,
        crate::Collection::DatastoreToolSurface,
        owner,
        &borrowed,
    )
    .await?;
    let surfaces = rows
        .into_values()
        .map(|(_, value)| serde_json::from_value(value))
        .collect::<std::result::Result<Vec<DatastoreToolSurfaceDocument>, _>>()?;
    confirm_cached_datastore_tool(&tools, &surfaces, surface_id, expected)
}

#[derive(Clone)]
pub(crate) struct DatastoreToolSource {
    pub(crate) owner: String,
    pub(crate) surface_id: String,
    pub(crate) declaration: SurfaceToolDecl,
}

impl DatastoreToolSource {
    pub(crate) async fn revalidate(
        &self,
        txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    ) -> Result<()> {
        revalidate_datastore_tool_in_txn(txn, &self.owner, &self.surface_id, &self.declaration)
            .await
    }
}

pub(super) fn confirm_cached_datastore_tool(
    tools: &Tools,
    surfaces: &[DatastoreToolSurfaceDocument],
    surface_id: &str,
    expected: &SurfaceToolDecl,
) -> Result<()> {
    let merged = merge_datastore_tool_surfaces(tools, surfaces)?;
    let matching_source = merged
        .surface_of_tool
        .get(expected.tool_name())
        .is_some_and(|id| id == surface_id);
    let matching_declaration = match expected {
        SurfaceToolDecl::Create(decl) => merged.write_tools.contains(decl),
        SurfaceToolDecl::Query(decl) => merged.query_tools.contains(decl),
    };
    anyhow::ensure!(
        matching_source && matching_declaration,
        "datastore tool source or declaration is no longer selected"
    );
    Ok(())
}

/// One bound read tool: one collection, a fixed projection, optional filter
/// fills. The model never names the collection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct QueryToolDecl {
    pub tool_name: String,
    pub collection: String,
    #[cfg_attr(feature = "typescript", ts(as = "Option<String>", optional))]
    #[schemars(default)]
    pub description: String,
    /// Projection allowlist; omit `fields` at call time to return all of these.
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional))]
    #[schemars(default)]
    pub fields: Vec<String>,
    #[cfg_attr(
        feature = "typescript",
        ts(as = "Option<Vec<WriteToolField>>", optional)
    )]
    #[schemars(default)]
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

/// The authored shape the `Deserialize` impl below accepts. It trims `kind`
/// with `str::trim`: a create entry's `kind` is absent, null, blank, `create`
/// or `write`, and a query entry's is `query` or `read`. The canonical values
/// stay an enum, and the padded spellings are one extra pattern member.
impl schemars::JsonSchema for SurfaceToolDecl {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "SurfaceToolDecl".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let create = generator.subschema_for::<WriteToolDecl>();
        let query = generator.subschema_for::<QueryToolDecl>();
        let space = trimmed_class();
        let padded_create = super::noted(
            serde_json::json!({
                "type": "string",
                "pattern": format!("^{space}*(create|write)?{space}*$")
            }),
            "Surrounding whitespace is trimmed; blank means create.",
        );
        let padded_query = super::noted(
            serde_json::json!({
                "type": "string",
                "pattern": format!("^{space}*(query|read){space}*$")
            }),
            "Surrounding whitespace is trimmed.",
        );
        schemars::json_schema!({
            "anyOf": [
                {
                    "allOf": [create],
                    "properties": { "kind": { "anyOf": [
                        { "enum": ["create", "write", null] },
                        padded_create
                    ] } }
                },
                {
                    "allOf": [query],
                    "properties": { "kind": { "anyOf": [
                        { "enum": ["query", "read"] },
                        padded_query
                    ] } },
                    "required": ["kind"]
                }
            ]
        })
    }
}

/// The characters `str::trim` removes, as an ECMA-262 class: spelled out
/// because regex engines disagree on what `\s` covers.
fn trimmed_class() -> String {
    let mut class = String::from("[");
    let mut spaces = ('\0'..=char::MAX).filter(|c| c.is_whitespace()).peekable();
    while let Some(first) = spaces.next() {
        let mut last = first;
        while let Some(&next) = spaces.peek() {
            if u32::from(next) != u32::from(last) + 1 {
                break;
            }
            last = next;
            spaces.next();
        }
        class.push_str(&format!("\\u{:04X}", u32::from(first)));
        if last != first {
            class.push_str(&format!("-\\u{:04X}", u32::from(last)));
        }
    }
    class.push(']');
    class
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

/// Schema for [`deserialize_optional_surface_tools`]: entries as a list, as
/// the stored `{"entries": ...}` envelope, or null; an entry may also be the
/// JSON text of one declaration.
pub(crate) fn surface_tools_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let entry = generator.subschema_for::<SurfaceToolDecl>();
    let entries = schemars::json_schema!({
        "anyOf": [
            { "type": "array", "items": { "anyOf": [entry, { "type": "string" }] } },
            { "type": "string" },
            { "type": "null" }
        ]
    });
    schemars::json_schema!({
        "anyOf": [
            entries.clone(),
            {
                "type": "object",
                "properties": { "entries": entries },
                "required": ["entries"],
                "additionalProperties": false
            }
        ]
    })
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
) -> Result<()> {
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
        crate::store::graphql::validate_collection_identifier(&decl.collection).map_err(
            |error| {
                anyhow::anyhow!(
                    "query_tools[{i}] (tool {:?}) has invalid collection {:?}: {error}",
                    decl.tool_name,
                    decl.collection
                )
            },
        )?;
        reject_protected_collection_name(&decl.collection)?;
        let mut seen_field_names = rapidhash::RapidHashSet::default();
        for (j, field) in decl.fields.iter().enumerate() {
            crate::store::graphql::validate_graphql_name(field).map_err(|error| {
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
        let mut seen_filter_names = rapidhash::RapidHashSet::default();
        for (j, field) in decl.filter_fields.iter().enumerate() {
            if field.name.trim().is_empty() {
                return Err(anyhow::anyhow!(
                    "query_tools[{i}] (tool {:?}) has a filter_fields[{j}] with an empty name",
                    decl.tool_name
                ));
            }
            crate::store::graphql::validate_graphql_name(&field.name).map_err(|error| {
                anyhow::anyhow!(
                    "query_tools[{i}] (tool {:?}) filter_fields[{j}] {:?} is not a GraphQL name: {error}",
                    decl.tool_name,
                    field.name
                )
            })?;
            if matches!(
                field.name.trim(),
                "fields" | "limit" | "offset" | "order" | "field_page"
            ) {
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
                crate::store::graphql::validate_graphql_name(source_field).map_err(|error| {
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
