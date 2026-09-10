use anyhow::Result;
use serde::Deserialize;

use super::serde_helpers;
use crate::defra_query::DEFRA_QUERY_TOOL_NAME;
use crate::meta_tools::META_TOOL_NAMES;
use crate::toolset::{
    CANCEL_PROCESS_TOOL_NAME, CANCEL_SUBAGENT_TOOL_NAME, CONTEXT_BUDGET_TOOL_NAME,
    LIST_PROCESSES_TOOL_NAME, LIST_SUBAGENTS_TOOL_NAME, READ_PROCESS_TOOL_NAME,
    READ_SUBAGENT_TOOL_NAME, SESSION_HISTORY_TOOL_NAME, SPAWN_PROCESS_TOOL_NAME,
    SPAWN_SUBAGENT_TOOL_NAME, STEER_SUBAGENT_TOOL_NAME, WAIT_PROCESS_TOOL_NAME,
    WAIT_SUBAGENT_TOOL_NAME,
};

/// One field of a [`WriteToolDecl`]: a named slot the bound write tool exposes,
/// and whether the agent must provide it.
///
/// `name` is trimmed at deserialization so the stored value, the runtime
/// [`crate::defra_write::BoundedWriteTool`], and `config validate` all agree on
/// the same canonical identifier (the field name is interpolated verbatim as a
/// GraphQL input key, so stray whitespace would otherwise corrupt the mutation).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(rename_all = "snake_case"))]
pub enum WriteToolFieldFill {
    Correlation,
    SourceField(String),
}

impl serde::Serialize for WriteToolFieldFill {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Correlation => serializer.serialize_str("correlation"),
            Self::SourceField(field) => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("source_field", field)?;
                map.end()
            }
        }
    }
}

impl<'de> serde::Deserialize<'de> for WriteToolFieldFill {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        match value {
            serde_json::Value::String(value) if value == "correlation" => Ok(Self::Correlation),
            serde_json::Value::Object(map) if map.len() == 1 => {
                let field = map
                    .get("source_field")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        serde::de::Error::custom(
                            "fill object must contain exactly one string source_field",
                        )
                    })?;
                crate::graphql::validate_graphql_name(field).map_err(serde::de::Error::custom)?;
                Ok(Self::SourceField(field.to_string()))
            }
            _ => Err(serde::de::Error::custom(
                "fill must be \"correlation\" or {\"source_field\":\"field\"}",
            )),
        }
    }
}

impl WriteToolFieldFill {
    /// Resolve this fill from the current tool-call runtime context.
    ///
    /// Shared by [`crate::defra_write::BoundedWriteTool`] and
    /// [`crate::defra_query::bounded::BoundedQueryTool`] so the correlation /
    /// source-field vocabulary cannot drift.
    pub fn resolve(&self, field_name: &str) -> Result<String> {
        let runtime = crate::tool_call_lifecycle::runtime::current_tool_runtime_context()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "runtime-filled field `{field_name}` requires an AgentRequest trigger context"
                )
            })?;
        match self {
            Self::Correlation => runtime
                .correlation
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "runtime-filled field `{field_name}` requires a non-empty correlation"
                    )
                }),
            Self::SourceField(source_field) => runtime
                .source_fields
                .get(source_field)
                .cloned()
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "runtime-filled field `{field_name}` requires source field `{source_field}` in trigger context"
                    )
                }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct WriteToolField {
    pub name: String,
    #[cfg_attr(feature = "typescript", ts(as = "Option<bool>", optional))]
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub fill: Option<WriteToolFieldFill>,
}

impl<'de> serde::Deserialize<'de> for WriteToolField {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        struct Raw {
            name: String,
            #[serde(default)]
            required: bool,
            #[serde(default)]
            fill: Option<WriteToolFieldFill>,
        }
        let raw = Raw::deserialize(deserializer)?;
        Ok(WriteToolField {
            name: raw.name.trim().to_string(),
            required: raw.required,
            fill: raw.fill,
        })
    }
}

/// A declarative, schema-bounded document-write tool. Each declaration becomes
/// one runtime `BoundedWriteTool` that writes exactly one validated document to
/// one collection. DatastoreToolSurface.entries owns these canonical declarations;
/// Tools.datastore selects the surfaces by their owner-scoped document IDs.
///
/// `tool_name` and `collection` are trimmed at deserialization (see
/// [`WriteToolField`] for the rationale); `description` is free text and is
/// preserved verbatim.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct WriteToolDecl {
    pub tool_name: String,
    pub collection: String,
    #[cfg_attr(feature = "typescript", ts(as = "Option<String>", optional))]
    pub description: String,
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<WriteToolField>>", optional))]
    pub fields: Vec<WriteToolField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub output_obligation: Option<WriteToolOutputObligation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum WriteToolOutputObligationScope {
    Request,
    Trigger,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct WriteToolOutputObligation {
    pub scope: WriteToolOutputObligationScope,
    #[serde(default = "default_minimum_writes")]
    #[cfg_attr(feature = "typescript", ts(as = "Option<usize>", optional))]
    pub minimum_writes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub expected_count_field: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputObligationDecision {
    Continue,
    Complete,
    Reject,
}

impl<'de> serde::Deserialize<'de> for WriteToolOutputObligation {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        struct Raw {
            scope: WriteToolOutputObligationScope,
            #[serde(default = "default_minimum_writes")]
            minimum_writes: usize,
            #[serde(default)]
            expected_count_field: Option<String>,
        }

        let raw = Raw::deserialize(deserializer)?;
        Ok(Self {
            scope: raw.scope,
            minimum_writes: raw.minimum_writes,
            expected_count_field: raw
                .expected_count_field
                .map(|field| field.trim().to_string())
                .filter(|field| !field.is_empty()),
        })
    }
}

impl WriteToolOutputObligation {
    pub fn applies_to(&self, has_automated_trigger_lineage: bool) -> bool {
        match self.scope {
            WriteToolOutputObligationScope::Request => true,
            WriteToolOutputObligationScope::Trigger => has_automated_trigger_lineage,
        }
    }

    pub fn decision(
        &self,
        completed_writes: usize,
        expected_writes: Option<usize>,
        count_valid: bool,
    ) -> OutputObligationDecision {
        if !count_valid {
            return OutputObligationDecision::Reject;
        }
        if let Some(expected_writes) = expected_writes {
            if expected_writes < self.minimum_writes || completed_writes > expected_writes {
                return OutputObligationDecision::Reject;
            }
            if completed_writes == expected_writes {
                return OutputObligationDecision::Complete;
            }
            return OutputObligationDecision::Continue;
        }
        if completed_writes >= self.minimum_writes {
            OutputObligationDecision::Complete
        } else {
            OutputObligationDecision::Continue
        }
    }
}

const fn default_minimum_writes() -> usize {
    1
}

impl<'de> serde::Deserialize<'de> for WriteToolDecl {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        struct Raw {
            tool_name: String,
            collection: String,
            #[serde(default)]
            description: String,
            #[serde(default)]
            fields: Vec<WriteToolField>,
            #[serde(default)]
            output_obligation: Option<WriteToolOutputObligation>,
        }
        let raw = Raw::deserialize(deserializer)?;
        Ok(WriteToolDecl {
            tool_name: raw.tool_name.trim().to_string(),
            collection: raw.collection.trim().to_string(),
            description: raw.description,
            fields: raw.fields,
            output_obligation: raw.output_obligation,
        })
    }
}

impl WriteToolDecl {
    /// Validate the declaration once for every registration and execution path.
    pub fn validate(&self) -> Result<()> {
        if self.tool_name.trim().is_empty() {
            anyhow::bail!("tool_name must be non-empty");
        }
        crate::graphql::validate_collection_identifier(&self.collection).map_err(|error| {
            anyhow::anyhow!("invalid collection {:?}: {error}", self.collection)
        })?;
        for (index, field) in self.fields.iter().enumerate() {
            if field.name.trim().is_empty() {
                anyhow::bail!("invalid field[{index}] name: empty name");
            }
            crate::graphql::validate_graphql_name(&field.name).map_err(|error| {
                anyhow::anyhow!("invalid field[{index}] name {:?}: {error}", field.name)
            })?;
            if field.name == "requester_did" && field.fill.is_none() {
                anyhow::bail!(
                    "field[{index}] requester_did is principal identity and must be runtime-filled"
                );
            }
        }
        Ok(())
    }

    /// Boolean form used by tool advertisement and registration filters.
    pub fn is_well_formed(&self) -> bool {
        self.validate().is_ok()
    }

    pub fn output_obligation_is_well_formed(&self) -> bool {
        self.output_obligation.as_ref().is_none_or(|obligation| {
            obligation.minimum_writes > 0
                && obligation.expected_count_field.as_ref().is_none_or(|name| {
                    self.fields
                        .iter()
                        .any(|field| field.name == *name && field.required && field.fill.is_none())
                })
        })
    }
}

pub(crate) fn validate_write_tool_declarations(
    decls: &[WriteToolDecl],
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
        decl.validate().map_err(|error| {
            anyhow::anyhow!(
                "write_tools[{i}] (tool {:?}) is malformed: {error}",
                decl.tool_name
            )
        })?;
        crate::mailbox::validate_mailbox_write_decl(decl).map_err(|error| {
            anyhow::anyhow!(
                "write_tools[{i}] (tool {:?}) violates mailbox registration policy: {error}",
                decl.tool_name
            )
        })?;
        if !decl.output_obligation_is_well_formed() {
            return Err(anyhow::anyhow!(
                "write_tools[{i}] (tool {:?}) output_obligation.minimum_writes must be greater than zero and output_obligation.expected_count_field, when present, must name a required model-provided field",
                decl.tool_name
            ));
        }
        reject_tool_name_surface_collisions(
            "write_tools",
            i,
            &decl.tool_name,
            "write tools",
            &cli_tool_names,
            &additional_tool_names,
        )?;
        let mut seen_field_names = std::collections::HashSet::new();
        for (j, field) in decl.fields.iter().enumerate() {
            if !seen_field_names.insert(field.name.trim()) {
                return Err(anyhow::anyhow!(
                    "write_tools[{i}] (tool {:?}) has a duplicate field name {:?}; each WriteToolField in a declaration must have a unique name",
                    decl.tool_name,
                    field.name.trim()
                ));
            }
            if field.fill.is_some() && field.required {
                return Err(anyhow::anyhow!(
                    "write_tools[{i}] (tool {:?}) field[{j}] {:?} is runtime-filled and cannot be required",
                    decl.tool_name,
                    field.name,
                ));
            }
            if let Some(WriteToolFieldFill::SourceField(source_field)) = &field.fill {
                crate::graphql::validate_graphql_name(source_field).map_err(|error| {
                    anyhow::anyhow!(
                        "write_tools[{i}] (tool {:?}) field[{j}] has invalid source_field {:?}: {error}",
                        decl.tool_name,
                        source_field,
                    )
                })?;
            }
        }
        if !seen_tool_names.insert(decl.tool_name.trim()) {
            return Err(anyhow::anyhow!(
                "write_tools has a duplicate tool_name {:?}; each declared write tool must have a unique tool_name",
                decl.tool_name.trim()
            ));
        }
    }
    Ok(())
}

/// True when `name` is already claimed by the built-in tool surface: the native
/// file/shell tools, the meta tools, the subagent/process control tools, or the
/// built-in singletons (including the durable goal tools).
///
/// A `write_tools` declaration whose `tool_name` collides with one of these
/// would be appended to the runtime tool vector under a name an existing
/// built-in already advertises: [`crate::tool_surface::ToolSurface::tool_names`]
/// dedupes the advertised list (so the model sees a single name) while
/// `build_tools` registers two `ToolDyn` impls and `BackgroundToolRegistry`
/// keys them by name with last-write-wins — silently shadowing the built-in.
/// The apply/ingest validators reject the collision instead.
pub fn is_reserved_builtin_tool_name(name: &str) -> bool {
    let name = name.trim();

    const SUBAGENT_TOOL_NAMES: &[&str] = &[
        SPAWN_SUBAGENT_TOOL_NAME,
        WAIT_SUBAGENT_TOOL_NAME,
        LIST_SUBAGENTS_TOOL_NAME,
        READ_SUBAGENT_TOOL_NAME,
        STEER_SUBAGENT_TOOL_NAME,
        CANCEL_SUBAGENT_TOOL_NAME,
        SPAWN_PROCESS_TOOL_NAME,
        WAIT_PROCESS_TOOL_NAME,
        LIST_PROCESSES_TOOL_NAME,
        READ_PROCESS_TOOL_NAME,
        CANCEL_PROCESS_TOOL_NAME,
    ];
    // `memory` is reserved unconditionally: the `agent-memory` feature gates the
    // tool's availability, not the legitimacy of the name as a write-tool id.
    const SINGLETON_TOOL_NAMES: &[&str] = &[
        DEFRA_QUERY_TOOL_NAME,
        CONTEXT_BUDGET_TOOL_NAME,
        SESSION_HISTORY_TOOL_NAME,
        crate::goal::CREATE_GOAL_TOOL_NAME,
        crate::goal::GET_GOAL_TOOL_NAME,
        crate::goal::UPDATE_GOAL_TOOL_NAME,
        "memory",
    ];

    crate::toolset::NativeTool::ALL_NAMES.contains(&name)
        || name == crate::toolset::lsp::LSP_TOOL_NAME
        || META_TOOL_NAMES.contains(&name)
        || SUBAGENT_TOOL_NAMES.contains(&name)
        || SINGLETON_TOOL_NAMES.contains(&name)
        || crate::self_config::SELF_CONFIG_TOOL_NAMES.contains(&name)
        || crate::graph_pipeline::GRAPH_PIPELINE_TOOL_NAMES.contains(&name)
}

/// Deserialize the `write_tools` field from either representation:
/// - a JSON array of [`WriteToolDecl`] objects (manifest / `config apply` input),
/// - a JSON array of strings, each the JSON serialization of one
///   [`WriteToolDecl`] (how DefraDB returns the `[String]` column),
/// - `null` / missing / empty string (→ `None`).
///
/// This mirrors how `subagent_targets` survives the GraphQL `[String]` round-trip
/// while keeping the manifest-facing shape a structured list of objects.
pub(crate) fn deserialize_optional_write_tools<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Vec<WriteToolDecl>>, D::Error>
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
        "write_tools must be a list of WriteToolDecl objects or JSON strings",
    )
    .map(Some)
    .map_err(D::Error::custom)
}

/// Reject a declared write/query tool name that collides with a built-in,
/// a `cli_tool_names` entry, or another runtime-provided tool.
pub(crate) fn reject_tool_name_surface_collisions(
    field: &str,
    index: usize,
    tool_name: &str,
    role: &str,
    cli_tool_names: &std::collections::HashSet<&str>,
    additional_tool_names: &std::collections::HashSet<&str>,
) -> Result<()> {
    let name = tool_name.trim();
    if is_reserved_builtin_tool_name(tool_name) {
        return Err(anyhow::anyhow!(
            "{field}[{index}] tool_name {name:?} collides with a built-in tool; declared {role} must use a unique name"
        ));
    }
    if cli_tool_names.contains(name) {
        return Err(anyhow::anyhow!(
            "{field}[{index}] tool_name {name:?} collides with a CLI entry in the same Tools document; each tool must have a unique name"
        ));
    }
    if additional_tool_names.contains(name) {
        return Err(anyhow::anyhow!(
            "{field}[{index}] tool_name {name:?} collides with another runtime-provided tool; each tool must have a unique name"
        ));
    }
    Ok(())
}
