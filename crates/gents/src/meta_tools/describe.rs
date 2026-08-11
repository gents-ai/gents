use crate::llm::tool::Tool;
use crate::llm::tool::ToolDefinition;
use rmcp::model::{ListToolsResult, Tool as McpTool};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::HashSet;

use crate::truncation::{truncate_text, TruncationLimits, TruncationMode};

use super::shared::{
    enforce_health_gate, lookup_service, MetaToolContext, MetaToolError, StructuredToolError,
};

const RAW_SCHEMA_MAX_BYTES: usize = 16_000;
const RAW_SCHEMA_MAX_LINES: usize = 2_000;

#[derive(Debug, Deserialize)]
pub struct DescribeToolArgs {
    service_id: String,
    tool_name: String,
    #[serde(default)]
    raw_schema: bool,
}

#[derive(Clone)]
pub struct DescribeToolTool {
    ctx: MetaToolContext,
}

impl DescribeToolTool {
    pub(crate) fn new(ctx: MetaToolContext) -> Self {
        Self { ctx }
    }
}

impl Tool for DescribeToolTool {
    const NAME: &'static str = "describe_tool";

    type Error = MetaToolError;
    type Args = DescribeToolArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Get a compact input contract for a specific MCP tool on a data service. \
                Use this before call_tool to understand required arguments, optional arguments, \
                defaults, constraints, examples, and unknown-field behavior. This does not \
                describe native direct tools such as file or bash tools; use their direct tool \
                definitions instead. Set raw_schema=true only when you need the exact JSON Schema."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "service_id": {
                        "type": "string",
                        "description": "The service_id of the MCP data service; not a native tool namespace."
                    },
                    "tool_name": {
                        "type": "string",
                        "description": "The name of the tool to describe."
                    },
                    "raw_schema": {
                        "type": "boolean",
                        "default": false,
                        "description": "When true, return the exact raw JSON Schema instead of the compact contract."
                    }
                },
                "required": ["service_id", "tool_name"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        if let Some(error) = self
            .ctx
            .blocked_service_error(&args.service_id, &args.tool_name)
        {
            return Err(MetaToolError::structured(error));
        }

        if let Err(error) = enforce_health_gate(&self.ctx.health, &args.service_id).await {
            return Err(MetaToolError::structured(
                StructuredToolError::service_unavailable(
                    &args.service_id,
                    &args.tool_name,
                    format!(
                        "service '{}' is currently unavailable: {error:#}",
                        args.service_id
                    ),
                    true,
                ),
            ));
        }

        let service = match lookup_service(&self.ctx, &args.service_id).await {
            Ok(service) => service,
            Err(error) => {
                return Err(MetaToolError::structured(
                    StructuredToolError::service_unavailable(
                        &args.service_id,
                        &args.tool_name,
                        format!(
                            "service '{}' is not available for describe_tool: {error:#}",
                            args.service_id
                        ),
                        false,
                    ),
                ));
            }
        };

        let list_result = match self
            .ctx
            .mcp_pool
            .list_tools_with_agent_did(
                &args.service_id,
                &service.endpoint,
                service.outbound_agent_did(&self.ctx),
            )
            .await
        {
            Ok(result) => result,
            Err(error) => {
                return Err(MetaToolError::structured(
                    StructuredToolError::service_unavailable(
                        &args.service_id,
                        &args.tool_name,
                        format!(
                            "service '{}' could not list tools for describe_tool: {error:#}",
                            args.service_id
                        ),
                        true,
                    ),
                ));
            }
        };

        match describe_tool_result(
            &args.service_id,
            &args.tool_name,
            args.raw_schema,
            &list_result,
        ) {
            Ok(result) => Ok(result),
            Err(error) => Err(MetaToolError::structured(error)),
        }
    }

    fn into_dyn_error(error: Self::Error) -> crate::llm::tool::ToolError {
        error.into_dispatch_error()
    }
}

fn describe_tool_result(
    service_id: &str,
    requested_tool_name: &str,
    raw_schema: bool,
    list_result: &ListToolsResult,
) -> Result<String, StructuredToolError> {
    let tool = list_result
        .tools
        .iter()
        .find(|tool| tool.name == requested_tool_name);

    match tool {
        Some(tool) => Ok(format_tool_description(service_id, tool, raw_schema)),
        None => {
            let available_tools = list_result
                .tools
                .iter()
                .map(|tool| tool.name.to_string())
                .collect::<Vec<_>>();
            Err(StructuredToolError::describe_tool_not_found(
                service_id,
                requested_tool_name,
                available_tools,
            ))
        }
    }
}

fn format_tool_description(service_id: &str, tool: &McpTool, raw_schema: bool) -> String {
    let desc = tool.description.as_deref().unwrap_or("(no description)");
    let schema_json = serde_json::to_string_pretty(tool.input_schema.as_ref()).unwrap_or_default();

    if raw_schema {
        let limits = TruncationLimits {
            max_bytes: RAW_SCHEMA_MAX_BYTES,
            max_lines: RAW_SCHEMA_MAX_LINES,
        };
        let (capped_schema, _trigger, _was_truncated) =
            truncate_text(&schema_json, TruncationMode::Head, &limits);
        return format!(
            "## {name}\n{desc}\n\nRaw input schema:\n```json\n{capped_schema}\n```",
            name = tool.name,
        );
    }

    format_tool_contract(service_id, tool, desc, &schema_json)
}

fn format_tool_contract(service_id: &str, tool: &McpTool, desc: &str, schema_json: &str) -> String {
    let schema = tool.input_schema.as_ref();
    let fields = collect_argument_fields(schema);
    let top_level_fields = fields
        .iter()
        .filter(|field| field.depth == 0)
        .collect::<Vec<_>>();
    let nested_fields = fields
        .iter()
        .filter(|field| field.depth > 0)
        .collect::<Vec<_>>();
    let required_fields = top_level_fields
        .iter()
        .filter(|field| field.required)
        .copied()
        .collect::<Vec<_>>();
    let optional_fields = top_level_fields
        .iter()
        .filter(|field| !field.required)
        .copied()
        .collect::<Vec<_>>();

    let mut out = String::new();
    out.push_str(&format!("## {}\n", tool.name));
    out.push_str(&format!("Purpose: {}\n\n", compact_text(desc, 360)));
    out.push_str("Input contract:\n");
    out.push_str("- Argument object: `/arguments`\n");
    out.push_str(&format!(
        "- Unknown top-level fields: {}\n",
        additional_properties_behavior(schema)
    ));
    out.push_str("- Raw schema: call `describe_tool` with `raw_schema: true`.\n\n");

    if top_level_fields.is_empty() {
        out.push_str("Arguments: no named object fields are advertised by this schema.\n\n");
    } else {
        push_field_section(&mut out, "Required arguments", &required_fields, false);
        push_field_section(&mut out, "Optional arguments", &optional_fields, false);
    }

    if !nested_fields.is_empty() {
        push_field_section(&mut out, "Nested fields", &nested_fields, true);
    }

    if let Some(example) = example_arguments(schema) {
        out.push_str("Example `call_tool.arguments`:\n```json\n");
        out.push_str(&example);
        out.push_str("\n```\n\n");
    }

    let mistakes = common_mistakes(service_id, tool, schema, &fields);
    if !mistakes.is_empty() {
        out.push_str("Common mistakes:\n");
        for mistake in mistakes {
            out.push_str(&format!("- {mistake}\n"));
        }
        out.push('\n');
    }

    let safety_notes = safety_notes(tool);
    if !safety_notes.is_empty() {
        out.push_str("Safety notes:\n");
        for note in safety_notes {
            out.push_str(&format!("- {note}\n"));
        }
        out.push('\n');
    }

    let compact_len = out.len();
    out.push_str(&format!(
        "Size: compact contract is {} chars; raw schema is {} chars.\n",
        compact_len,
        schema_json.len()
    ));

    out
}

#[derive(Debug, Clone)]
struct ArgumentField {
    path: String,
    depth: usize,
    required: bool,
    type_summary: String,
    details: Vec<String>,
    description: Option<String>,
}

fn collect_argument_fields(schema: &Map<String, Value>) -> Vec<ArgumentField> {
    let mut fields = Vec::new();
    collect_object_fields(schema, "/arguments", 0, &mut fields);
    fields
}

fn collect_object_fields(
    schema: &Map<String, Value>,
    base_path: &str,
    depth: usize,
    fields: &mut Vec<ArgumentField>,
) {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return;
    };
    let required_names = required_names(schema);

    for (name, field_schema) in properties {
        let path = format!("{base_path}/{}", json_pointer_segment(name));
        let required = required_names.contains(name.as_str());
        fields.push(argument_field(&path, depth, required, field_schema));
        collect_nested_fields(field_schema, &path, depth + 1, fields);
    }
}

fn collect_nested_fields(
    schema: &Value,
    path: &str,
    depth: usize,
    fields: &mut Vec<ArgumentField>,
) {
    if let Some(object_schema) = schema.as_object() {
        collect_object_fields(object_schema, path, depth, fields);
        if let Some(items) = object_schema.get("items") {
            collect_array_item_fields(items, path, depth, fields);
        }
    }
}

fn collect_array_item_fields(
    items: &Value,
    path: &str,
    depth: usize,
    fields: &mut Vec<ArgumentField>,
) {
    let item_path = format!("{path}[]");
    if let Some(item_schema) = items.as_object() {
        if item_schema.get("properties").is_some() {
            collect_object_fields(item_schema, &item_path, depth, fields);
        } else if let Some(nested_items) = item_schema.get("items") {
            collect_array_item_fields(nested_items, &item_path, depth, fields);
        }
    }
}

fn argument_field(path: &str, depth: usize, required: bool, schema: &Value) -> ArgumentField {
    let schema_object = schema.as_object();
    let mut details = Vec::new();

    if let Some(object) = schema_object {
        push_known_constraint(&mut details, object, "default");
        push_known_constraint(&mut details, object, "const");
        push_enum_constraint(&mut details, object);
        push_known_constraint(&mut details, object, "format");
        push_known_constraint(&mut details, object, "pattern");
        push_known_constraint(&mut details, object, "minimum");
        push_known_constraint(&mut details, object, "maximum");
        push_known_constraint(&mut details, object, "exclusiveMinimum");
        push_known_constraint(&mut details, object, "exclusiveMaximum");
        push_known_constraint(&mut details, object, "minLength");
        push_known_constraint(&mut details, object, "maxLength");
        push_known_constraint(&mut details, object, "minItems");
        push_known_constraint(&mut details, object, "maxItems");
        push_known_constraint(&mut details, object, "uniqueItems");
        push_examples(&mut details, object);
        if is_object_like(object) {
            details.push(format!(
                "unknown nested fields: {}",
                additional_properties_behavior(object)
            ));
        }
    }

    ArgumentField {
        path: path.to_string(),
        depth,
        required,
        type_summary: type_summary(schema),
        details,
        description: schema_description(schema),
    }
}

fn push_field_section(out: &mut String, heading: &str, fields: &[&ArgumentField], nested: bool) {
    out.push_str(&format!("{heading}:\n"));
    if fields.is_empty() {
        out.push_str("- none\n\n");
        return;
    }

    for field in fields {
        let mut annotations = vec![field.type_summary.clone()];
        if nested {
            annotations.push(if field.required {
                "required".to_string()
            } else {
                "optional".to_string()
            });
        }
        annotations.extend(field.details.iter().cloned());
        let suffix = field
            .description
            .as_deref()
            .map(|description| format!(" - {}", compact_text(description, 180)))
            .unwrap_or_default();
        out.push_str(&format!(
            "- `{}` ({}){}\n",
            field.path,
            annotations.join("; "),
            suffix
        ));
    }
    out.push('\n');
}

fn required_names(schema: &Map<String, Value>) -> HashSet<&str> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

fn type_summary(schema: &Value) -> String {
    let Some(object) = schema.as_object() else {
        return "any".to_string();
    };

    if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
        return format!("ref {reference}");
    }

    if let Some(types) = object.get("type") {
        match types {
            Value::String(kind) => return type_summary_for_kind(kind, object),
            Value::Array(kinds) => {
                let rendered = kinds
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|kind| type_summary_for_kind(kind, object))
                    .collect::<Vec<_>>();
                if !rendered.is_empty() {
                    return rendered.join(" or ");
                }
            }
            _ => {}
        }
    }

    if object.get("properties").is_some() {
        return "object".to_string();
    }
    if object.get("items").is_some() {
        return type_summary_for_kind("array", object);
    }
    if let Some(options) = object.get("oneOf").and_then(Value::as_array) {
        return options_summary("one of", options);
    }
    if let Some(options) = object.get("anyOf").and_then(Value::as_array) {
        return options_summary("any of", options);
    }
    if object.get("enum").is_some() {
        return "enum".to_string();
    }

    "any".to_string()
}

fn type_summary_for_kind(kind: &str, schema: &Map<String, Value>) -> String {
    if kind == "array" {
        let item_type = schema
            .get("items")
            .map(type_summary)
            .unwrap_or_else(|| "any".to_string());
        return format!("array<{item_type}>");
    }
    kind.to_string()
}

fn options_summary(label: &str, options: &[Value]) -> String {
    let rendered = options.iter().map(type_summary).collect::<Vec<_>>();
    if rendered.is_empty() {
        label.to_string()
    } else {
        format!("{label}: {}", rendered.join(" | "))
    }
}

fn schema_description(schema: &Value) -> Option<String> {
    schema
        .as_object()
        .and_then(|object| object.get("description"))
        .and_then(Value::as_str)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn additional_properties_behavior(schema: &Map<String, Value>) -> String {
    match schema.get("additionalProperties") {
        Some(Value::Bool(false)) => "rejected (`additionalProperties: false`)".to_string(),
        Some(Value::Bool(true)) => "allowed (`additionalProperties: true`)".to_string(),
        Some(Value::Object(_)) => "allowed if they match the nested schema".to_string(),
        Some(_) => "specified by schema but not a simple boolean".to_string(),
        None => "not specified by schema".to_string(),
    }
}

fn is_object_like(schema: &Map<String, Value>) -> bool {
    schema
        .get("type")
        .and_then(Value::as_str)
        .map(|kind| kind == "object")
        .unwrap_or_else(|| schema.get("properties").is_some())
}

fn push_known_constraint(details: &mut Vec<String>, schema: &Map<String, Value>, key: &str) {
    if let Some(value) = schema.get(key) {
        details.push(format!("{key}: {}", compact_json(value)));
    }
}

fn push_enum_constraint(details: &mut Vec<String>, schema: &Map<String, Value>) {
    let Some(values) = schema.get("enum").and_then(Value::as_array) else {
        return;
    };
    let mut rendered = values.iter().take(6).map(compact_json).collect::<Vec<_>>();
    if values.len() > rendered.len() {
        rendered.push(format!("... {} more", values.len() - rendered.len()));
    }
    details.push(format!("enum: {}", rendered.join(", ")));
}

fn push_examples(details: &mut Vec<String>, schema: &Map<String, Value>) {
    if let Some(example) = schema.get("example") {
        details.push(format!("example: {}", compact_json(example)));
        return;
    }
    let Some(examples) = schema.get("examples").and_then(Value::as_array) else {
        return;
    };
    let rendered = examples
        .iter()
        .take(2)
        .map(compact_json)
        .collect::<Vec<_>>();
    if !rendered.is_empty() {
        details.push(format!("examples: {}", rendered.join(", ")));
    }
}

fn example_arguments(schema: &Map<String, Value>) -> Option<String> {
    let value = example_for_object(schema, true);
    serde_json::to_string_pretty(&value).ok()
}

fn example_for_object(schema: &Map<String, Value>, required_only: bool) -> Value {
    let mut object = Map::new();
    let required = required_names(schema);
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Value::Object(object);
    };

    for (name, field_schema) in properties {
        let field_is_required = required.contains(name.as_str());
        let include = field_is_required
            || (!required_only
                && field_schema
                    .as_object()
                    .and_then(|field| field.get("default"))
                    .is_some());
        if include {
            object.insert(name.clone(), example_for_schema(field_schema));
        }
    }

    Value::Object(object)
}

fn example_for_schema(schema: &Value) -> Value {
    let Some(object) = schema.as_object() else {
        return Value::Null;
    };
    if let Some(value) = object.get("default") {
        return value.clone();
    }
    if let Some(value) = object.get("const") {
        return value.clone();
    }
    if let Some(value) = object
        .get("examples")
        .and_then(Value::as_array)
        .and_then(|values| values.first())
    {
        return value.clone();
    }
    if let Some(value) = object.get("example") {
        return value.clone();
    }
    if let Some(value) = object
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|values| values.first())
    {
        return value.clone();
    }

    match type_summary(schema).as_str() {
        "boolean" => Value::Bool(true),
        "integer" => Value::from(1),
        "number" => Value::from(1.0),
        "object" => example_for_object(object, true),
        kind if kind.starts_with("array<") => {
            let min_items = object.get("minItems").and_then(Value::as_u64).unwrap_or(0);
            if min_items == 0 {
                Value::Array(Vec::new())
            } else {
                Value::Array(vec![object
                    .get("items")
                    .map(example_for_schema)
                    .unwrap_or(Value::Null)])
            }
        }
        _ => Value::String("string".to_string()),
    }
}

fn common_mistakes(
    service_id: &str,
    tool: &McpTool,
    schema: &Map<String, Value>,
    fields: &[ArgumentField],
) -> Vec<String> {
    let mut mistakes = vec![
        "Put these fields inside `call_tool.arguments`; do not pass them beside `service_id` or `tool_name`.".to_string(),
        "Use the exact field names and JSON types shown above.".to_string(),
    ];

    if schema.get("additionalProperties").and_then(Value::as_bool) == Some(false) {
        mistakes.push(
            "Do not add unlisted top-level fields; this schema rejects unknown arguments."
                .to_string(),
        );
    }
    if fields
        .iter()
        .any(|field| field.type_summary.starts_with("array<"))
    {
        mistakes.push(
            "Use JSON arrays for array fields; do not pass comma-separated strings.".to_string(),
        );
    }
    if service_id.contains("coding-session-store")
        || tool.name.as_ref().contains("coding_")
        || tool.name.as_ref().contains("coding")
    {
        mistakes.push(
            "For coding-session-store tools, do not invent generic fields like `limit`; use `top_n`, `repo_name`, or `path_contains` only when listed."
                .to_string(),
        );
    }
    if service_id.contains("x-data")
        || tool.name.as_ref().contains("search")
        || tool.name.as_ref().contains("x_")
    {
        mistakes.push(
            "For search tools, put the search text in `query` only when that field is listed; keep result limits within the documented constraints."
                .to_string(),
        );
    }

    mistakes
}

fn safety_notes(tool: &McpTool) -> Vec<String> {
    let name = tool.name.as_ref().to_lowercase();
    let description = tool.description.as_deref().unwrap_or("").to_lowercase();
    let combined = format!("{name} {description}");

    if combined.contains("service_status") || combined.contains("status") {
        return vec![
            "Observability/status-style tool; use it to check availability before depending on a service."
                .to_string(),
        ];
    }

    if [
        "write", "delete", "remove", "update", "create", "insert", "patch", "save", "upload",
        "execute", "run",
    ]
    .iter()
    .any(|needle| combined.contains(needle))
    {
        return vec![
            "This appears write-capable or execution-capable; verify target identifiers and payloads before calling."
                .to_string(),
        ];
    }

    if ["search", "list", "read", "overview", "lookup", "get"]
        .iter()
        .any(|needle| combined.contains(needle))
    {
        return vec![
            "This appears read-only/lookup-style; inspect returned data before using it for follow-up writes."
                .to_string(),
        ];
    }

    Vec::new()
}

fn json_pointer_segment(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

fn compact_json(value: &Value) -> String {
    let rendered = serde_json::to_string(value).unwrap_or_else(|_| "<unrenderable>".to_string());
    compact_text(&rendered, 120)
}

fn compact_text(value: &str, max_chars: usize) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= max_chars {
        return compact;
    }
    let mut truncated = compact
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    truncated.push_str("...");
    truncated
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rmcp::model::{ListToolsResult, Tool as McpTool};
    use serde_json::{json, Value};

    use super::*;
    use crate::health_checker::ServiceHealthMap;
    use crate::mcp_pool::McpPool;

    fn x_data_search_tool() -> McpTool {
        let schema = json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search text.",
                    "examples": ["gents"]
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum results to return.",
                    "default": 10,
                    "minimum": 1,
                    "maximum": 100
                }
            },
            "required": ["query"]
        })
        .as_object()
        .expect("schema object")
        .clone();

        McpTool::new("search_posts", "Search x-data posts.", Arc::new(schema))
    }

    #[test]
    fn missing_x_data_tool_returns_structured_envelope_with_available_tools() {
        let list_result = ListToolsResult::with_all_items(vec![x_data_search_tool()]);
        let error = describe_tool_result("x-data", "search_post", false, &list_result)
            .expect_err("missing tool should return envelope");

        assert_eq!(error.failure_class, "tool_not_found");
        assert_eq!(error.path, "/tool_name");
        assert!(error.retryable);
        assert_eq!(error.service_id, "x-data");
        assert_eq!(error.tool_name, "search_post");
        assert_eq!(error.requested_tool_name.as_deref(), Some("search_post"));
        assert_eq!(
            error.available_tools,
            Some(vec!["search_posts".to_string()])
        );

        let value: Value = serde_json::from_str(&error.to_result_text()).expect("structured json");
        assert_eq!(value["ok"], false);
        assert_eq!(value["failure_class"], "tool_not_found");
        assert_eq!(value["requested_tool_name"], "search_post");
        assert_eq!(value["available_tools"], json!(["search_posts"]));
        assert!(value["message"]
            .as_str()
            .expect("message")
            .contains("available tools: search_posts"));
    }

    #[test]
    fn default_describe_tool_output_is_compact_contract() {
        let list_result = ListToolsResult::with_all_items(vec![x_data_search_tool()]);
        let output = describe_tool_result("x-data", "search_posts", false, &list_result)
            .expect("known tool should describe");

        assert!(output.starts_with("## search_posts\nPurpose: Search x-data posts."));
        assert!(output.contains("Input contract:"));
        assert!(output.contains("Required arguments:\n- `/arguments/query` (string; examples: \"gents\") - Search text."));
        assert!(output.contains("Optional arguments:\n- `/arguments/limit` (integer; default: 10; minimum: 1; maximum: 100) - Maximum results to return."));
        assert!(
            output.contains("Unknown top-level fields: rejected (`additionalProperties: false`)")
        );
        assert!(output.contains("Raw schema: call `describe_tool` with `raw_schema: true`."));
        assert!(output.contains("Example `call_tool.arguments`:"));
        assert!(output.contains("\"query\": \"gents\""));
        assert!(!output.contains("Input schema:\n```json"));
    }

    #[test]
    fn raw_schema_can_still_be_requested() {
        let expected_schema = json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search text.",
                    "examples": ["gents"]
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum results to return.",
                    "default": 10,
                    "minimum": 1,
                    "maximum": 100
                }
            },
            "required": ["query"]
        });
        let list_result = ListToolsResult::with_all_items(vec![x_data_search_tool()]);
        let output = describe_tool_result("x-data", "search_posts", true, &list_result)
            .expect("known tool should describe raw schema");

        assert!(output
            .starts_with("## search_posts\nSearch x-data posts.\n\nRaw input schema:\n```json\n"));
        assert!(output.ends_with("\n```"));
        let schema_text = output
            .split("```json\n")
            .nth(1)
            .and_then(|text| text.strip_suffix("\n```"))
            .expect("schema block");
        let schema: Value = serde_json::from_str(schema_text).expect("schema json");
        assert_eq!(schema, expected_schema);
    }

    #[test]
    fn nested_object_and_array_fields_render_compact_paths() {
        let schema = json!({
            "type": "object",
            "properties": {
                "query": { "type": "string" },
                "filter": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "repo_name": { "type": "string" },
                        "states": {
                            "type": "array",
                            "items": {
                                "type": "string",
                                "enum": ["open", "closed"]
                            }
                        }
                    },
                    "required": ["repo_name"]
                },
                "sort": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "field": { "type": "string" },
                            "direction": {
                                "type": "string",
                                "enum": ["asc", "desc"]
                            }
                        },
                        "required": ["field"]
                    }
                }
            },
            "required": ["query", "filter"]
        })
        .as_object()
        .expect("schema object")
        .clone();
        let tool = McpTool::new(
            "search_coding_notes",
            "Search coding notes.",
            Arc::new(schema),
        );
        let list_result = ListToolsResult::with_all_items(vec![tool]);

        let output = describe_tool_result(
            "coding-session-store",
            "search_coding_notes",
            false,
            &list_result,
        )
        .expect("known tool should describe");

        assert!(output.contains("- `/arguments/query` (string)"));
        assert!(output.contains("- `/arguments/filter` (object; unknown nested fields: rejected (`additionalProperties: false`))"));
        assert!(output.contains("- `/arguments/filter/repo_name` (string; required)"));
        assert!(output.contains("- `/arguments/filter/states` (array<string>; optional)"));
        assert!(output.contains("- `/arguments/sort[]/field` (string; required)"));
        assert!(output.contains(
            "- `/arguments/sort[]/direction` (string; optional; enum: \"asc\", \"desc\")"
        ));
        assert!(output.contains("do not invent generic fields like `limit`"));
    }

    #[test]
    fn compact_contract_is_smaller_than_representative_raw_schema() {
        let properties = (0..40)
            .map(|i| {
                (
                    format!("optional_field_{i}"),
                    json!({
                        "type": "string",
                        "description": format!("Optional tuning field {i} with a deliberately verbose description that would be expensive in raw JSON Schema."),
                        "default": format!("value-{i}")
                    }),
                )
            })
            .chain(std::iter::once((
                "query".to_string(),
                json!({
                    "type": "string",
                    "description": "Search text.",
                    "examples": ["gents agent schema navigation"]
                }),
            )))
            .collect::<Map<String, Value>>();
        let mut schema = Map::new();
        schema.insert("type".to_string(), json!("object"));
        schema.insert("additionalProperties".to_string(), json!(false));
        schema.insert("properties".to_string(), Value::Object(properties));
        schema.insert("required".to_string(), json!(["query"]));
        let raw_schema = serde_json::to_string_pretty(&schema).expect("raw schema");
        let tool = McpTool::new("search_posts", "Search x-data posts.", Arc::new(schema));
        let contract = format_tool_description("x-data", &tool, false);

        assert!(
            contract.len() < raw_schema.len(),
            "compact contract should be smaller than raw schema: contract={}, raw={}",
            contract.len(),
            raw_schema.len()
        );
    }

    #[tokio::test]
    async fn disallowed_service_returns_before_registry_lookup() {
        let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        let tool = DescribeToolTool::new(MetaToolContext {
            node,
            mcp_pool: McpPool::new(),
            health: ServiceHealthMap::new(),
            local_hostname: "studio-1".to_string(),
            local_subnet: None,
            agent_did: "did:key:z-test-agent".to_string(),
            allowed_mcp_service_ids: vec!["x-data".to_string()],
        });

        let error = tool
            .call(DescribeToolArgs {
                service_id: "observability-mcp".to_string(),
                tool_name: "query_metrics".to_string(),
                raw_schema: false,
            })
            .await
            .expect_err("disallowed service should return a typed failure");
        let value: Value = serde_json::from_str(&error.to_string()).expect("structured json");

        assert_eq!(value["ok"], false);
        assert_eq!(value["failure_class"], "tool_not_allowed");
        assert_eq!(value["path"], "/service_id");
        assert_eq!(value["retryable"], false);
        assert_eq!(value["service_id"], "observability-mcp");
        assert_eq!(value["requested_tool_name"], "query_metrics");
        assert_eq!(value["allowed_mcp_service_ids"], json!(["x-data"]));
    }

    #[tokio::test]
    async fn missing_service_returns_structured_envelope() {
        let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let tool = DescribeToolTool::new(MetaToolContext {
            node,
            mcp_pool: McpPool::new(),
            health: ServiceHealthMap::new(),
            local_hostname: "studio-1".to_string(),
            local_subnet: None,
            agent_did: "did:key:z-test-agent".to_string(),
            allowed_mcp_service_ids: Vec::new(),
        });

        let error = tool
            .call(DescribeToolArgs {
                service_id: "missing-service".to_string(),
                tool_name: "search_posts".to_string(),
                raw_schema: false,
            })
            .await
            .expect_err("missing service should return a typed failure");
        let value: Value = serde_json::from_str(&error.to_string()).expect("structured json");

        assert_eq!(value["ok"], false);
        assert_eq!(value["failure_class"], "service_unavailable");
        assert_eq!(value["retryable"], false);
        assert_eq!(value["service_id"], "missing-service");
        assert_eq!(value["requested_tool_name"], "search_posts");
        assert_eq!(value["path"], "/service_id");
    }

    /// Build a schema so large that `raw_schema: true` would exceed
    /// `RAW_SCHEMA_MAX_BYTES` and confirm the output is capped with an honest
    /// truncation marker.
    #[test]
    fn raw_schema_oversized_is_capped_with_truncation_marker() {
        // Build a schema with many verbose properties so the pretty-printed JSON
        // exceeds 16 000 bytes.
        let properties: Map<String, Value> = (0..200)
            .map(|i| {
                (
                    format!("field_{i:03}"),
                    json!({
                        "type": "string",
                        "description": format!(
                            "A deliberately verbose description for field {i} that adds bytes to the \
                             raw JSON Schema output to ensure we exceed the RAW_SCHEMA_MAX_BYTES cap."
                        ),
                        "default": format!("default-value-for-field-{i}")
                    }),
                )
            })
            .collect();

        let mut schema_map = Map::new();
        schema_map.insert("type".to_string(), json!("object"));
        schema_map.insert("properties".to_string(), Value::Object(properties));
        let raw_schema_json =
            serde_json::to_string_pretty(&Value::Object(schema_map.clone())).unwrap();

        assert!(
            raw_schema_json.len() > RAW_SCHEMA_MAX_BYTES,
            "test precondition: raw schema must exceed cap ({} bytes)",
            raw_schema_json.len()
        );

        let tool = McpTool::new(
            "big_tool",
            "A tool with a huge schema.",
            Arc::new(schema_map),
        );
        let list_result = ListToolsResult::with_all_items(vec![tool]);
        let output = describe_tool_result("x-data", "big_tool", true, &list_result)
            .expect("known tool should describe");

        // Output must be shorter than the raw pretty-printed schema + header overhead.
        let header = "## big_tool\nA tool with a huge schema.\n\nRaw input schema:\n```json\n";
        let body = output
            .strip_prefix(header)
            .and_then(|s| s.strip_suffix("\n```"))
            .expect("output must have schema block wrapper");

        assert!(
            body.len() < raw_schema_json.len(),
            "capped schema body should be smaller than original: capped={}, original={}",
            body.len(),
            raw_schema_json.len()
        );
        // Slack: body should be within 2x the cap (marker overhead).
        assert!(
            body.len() < RAW_SCHEMA_MAX_BYTES * 2,
            "capped body should be near the cap, got {}",
            body.len()
        );
        // Honest marker must be present.
        assert!(
            body.contains("[Showing lines"),
            "honest truncation marker must be present"
        );
        assert!(
            body.contains("bytes total"),
            "total byte count must be reported"
        );
    }

    /// A small schema under the cap must be returned verbatim (no truncation marker).
    #[test]
    fn raw_schema_under_cap_passes_through_verbatim() {
        let list_result = ListToolsResult::with_all_items(vec![x_data_search_tool()]);
        let output = describe_tool_result("x-data", "search_posts", true, &list_result)
            .expect("known tool should describe");

        // The schema from x_data_search_tool is small; no truncation marker expected.
        assert!(
            !output.contains("[Showing lines"),
            "small schema must not carry a truncation marker"
        );
    }
}
