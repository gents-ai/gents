use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::{
    build_query, discovery_payload, introspection_query, parse_collection_schema, CollectionScope,
    DefraQueryParams,
};
use crate::config_client::ConfigRead;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QueryParams {
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub options: Map<String, Value>,
}

impl From<DefraQueryParams> for QueryParams {
    fn from(args: DefraQueryParams) -> Self {
        let discovery = args.is_discovery();
        let mut options = Map::new();
        if !discovery {
            options.insert("fields".into(), json!(args.fields));
        }
        if let Some(filter) = args.filter {
            options.insert("filter".into(), filter);
        }
        if let Some(limit) = args.limit {
            options.insert("limit".into(), json!(limit));
        }
        Self {
            argv: vec![if discovery { "fields" } else { "find" }.into()],
            collection: Some(args.collection),
            options,
        }
    }
}

pub fn query_help(command: Option<&str>) -> Result<&'static str> {
    match command {
        None => Ok("query reads documents. argv: [fields], [find], [count], [explain], [help,COMMAND]. Supply collection. fields discovers names/types; find returns a bounded page; count aggregates all matching rows. Configuration uses config; schema definitions use schema."),
        Some("fields") => Ok("{argv:[\"fields\"],collection:\"Shipment\"}. Returns available field names and types; no options."),
        Some("find") => Ok("{argv:[\"find\"],collection:\"Shipment\",options:{fields:[\"reference\",\"status\"],filter:{status:{_eq:\"queued\"}},order:[{priority:\"ASC\"}],offset:0,limit:20}}. Fields are required. Default limit 50, maximum 1000; offset ≤100000. Orders use ASC/DESC; _docID is appended as a tie-breaker. Pagination observes the current datastore, not a retained snapshot. Filters use native DefraDB operators: _eq,_ne,_gt,_lt,_ge,_le,_in,_nin,_like; compose with _and,_or,_not. Relationship selections, fulltext/BM25 and vector search are not exposed by this command family or bounded query tools; use the authenticated native GraphQL interface for those shapes."),
        Some("explain") => Ok("{argv:[\"explain\"],collection:\"Shipment\",options:{fields:[\"reference\"],filter:{status:{_eq:\"queued\"}},limit:20,mode:\"simple\"}}. Uses the same fields/filter/order/offset/limit and scope as find. Default simple inspects the native plan without executing the query. Set mode:execute only when the user requests measured execution; this runs the bounded read and returns native execution metrics. Execution metrics describe native work, not a matching-row total; use count for that. No mutations. Index observations are native plan facts; recommendations for other workloads are inferences."),
        Some("count") => Ok("{argv:[\"count\"],collection:\"Shipment\",options:{filter:{status:{_eq:\"queued\"}}}}. Returns total_count from DefraDB COUNT over every matching row; no fields/limit/offset/order."),
        _ => bail!("unknown query command; call query with {{\"argv\":[\"help\"]}}"),
    }
}

pub fn build_paged_query(
    params: &DefraQueryParams,
    scope: &CollectionScope,
    order: Option<&Value>,
    offset: u32,
) -> Result<String> {
    ensure!(
        offset <= 100_000,
        "offset exceeds 100000; use a narrower filter"
    );
    let base = build_query(params, scope)?;
    let mut orders = Vec::new();
    if let Some(value) = order {
        let entries: Vec<&Value> = match value {
            Value::Object(_) => vec![value],
            Value::Array(entries) => entries.iter().collect(),
            _ => bail!("order must be an object or array of single-field objects"),
        };
        ensure!(
            !entries.is_empty() && entries.len() <= 16,
            "order must contain 1–16 fields"
        );
        for entry in entries {
            let obj = entry
                .as_object()
                .filter(|v| v.len() == 1)
                .context("each order entry must contain one field and ASC or DESC")?;
            let (field, direction) = obj.iter().next().unwrap();
            let direction = direction
                .as_str()
                .filter(|v| matches!(*v, "ASC" | "DESC"))
                .context("order direction must be ASC or DESC")?;
            let guard = DefraQueryParams {
                fields: vec![field.clone()],
                ..params.clone()
            };
            build_query(&guard, scope)?;
            orders.push((field.clone(), direction));
        }
    }
    if !orders.iter().any(|(field, _)| field == "_docID") {
        orders.push(("_docID".into(), "ASC"));
    }
    let rendered = orders
        .iter()
        .map(|(field, direction)| format!("{{ {field}: {direction} }}"))
        .collect::<Vec<_>>()
        .join(", ");
    let insertion = base
        .find(") {")
        .context("query renderer omitted argument boundary")?;
    let mut out = base;
    out.insert_str(
        insertion,
        &format!(", order: [{rendered}], offset: {offset}"),
    );
    Ok(out)
}

pub async fn execute_command(
    access: &dyn ConfigRead,
    args: &QueryParams,
    scope: &CollectionScope,
) -> Result<Value> {
    let mut argv = args.argv.iter().map(String::as_str).collect::<Vec<_>>();
    if argv.last() == Some(&"--help") {
        argv.pop();
        argv.insert(0, "help");
    }
    if argv.first() == Some(&"help") {
        ensure!(
            args.collection.is_none() && args.options.is_empty(),
            "help accepts argv only"
        );
        ensure!(argv.len() <= 2, "help accepts one command");
        return Ok(json!({"help":query_help(argv.get(1).copied())?}));
    }
    ensure!(
        argv.len() == 1,
        "use argv:[fields|find|count|explain]; call query with {{\"argv\":[\"help\"]}}"
    );
    let command = argv[0];
    query_help(Some(command))?;
    let collection = args
        .collection
        .as_deref()
        .context("collection is required; discover collections through schema collection list")?;
    super::render::validate_identifier(collection)?;
    scope.ensure_allowed(collection)?;
    let allowed: &[&str] = match command {
        "fields" => &[],
        "count" => &["filter"],
        "find" => &["fields", "filter", "limit", "offset", "order"],
        "explain" => &["fields", "filter", "limit", "offset", "order", "mode"],
        _ => unreachable!(),
    };
    for key in args.options.keys() {
        ensure!(
            allowed.contains(&key.as_str()),
            "unknown option {key:?}; accepted options: {allowed:?}"
        );
    }
    let schema_response = access
        .execute_read(&introspection_query(collection)?)
        .await?;
    let schema = parse_collection_schema(schema_response.get("data"))
        .with_context(|| super::unknown_collection_message(collection))?;
    if command == "fields" {
        return Ok(discovery_payload(collection, &schema));
    }
    let fields = if command == "count" {
        vec!["_docID".into()]
    } else {
        serde_json::from_value(args.options.get("fields").cloned().context(
            "options.fields is required; call query with argv:[fields] to discover names",
        )?)
        .context("options.fields must be an array of field names")?
    };
    let limit = args
        .options
        .get("limit")
        .map(|v| serde_json::from_value(v.clone()))
        .transpose()
        .context("limit must be an unsigned integer")?;
    let params = DefraQueryParams {
        collection: collection.into(),
        fields,
        filter: args.options.get("filter").cloned(),
        limit,
    };
    build_query(&params, scope)?;
    let query = if command == "count" {
        let filter = params
            .filter
            .as_ref()
            .filter(|v| !v.is_null())
            .map(super::render::render_filter)
            .transpose()?;
        let arguments = filter.map(|f| format!("filter: {f}")).unwrap_or_default();
        format!("{{ COUNT({collection}: {{ {arguments} }}) }}")
    } else {
        let offset = args
            .options
            .get("offset")
            .map(|v| serde_json::from_value(v.clone()))
            .transpose()
            .context("offset must be an unsigned integer")?
            .unwrap_or(0);
        build_paged_query(&params, scope, args.options.get("order"), offset)?
    };
    let mode = if command == "explain" {
        let mode = args
            .options
            .get("mode")
            .map(|v| v.as_str().context("mode must be simple or execute"))
            .transpose()?
            .unwrap_or("simple");
        ensure!(
            matches!(mode, "simple" | "execute"),
            "mode must be simple or execute; call query with argv:[help,explain]"
        );
        Some(mode)
    } else {
        None
    };
    let query = if let Some(mode) = mode {
        format!("query @explain(type: {mode}) {query}")
    } else {
        query
    };
    let response = access.execute_read(&query).await.map_err(|error| {
        let diagnostic =
            super::diagnose_failed_query(&params, Some(&schema), &format!("{error:#}"));
        error.context(format!(
            "{diagnostic}; recovery: {}",
            json!({"tool":"query","args":{"argv":["help",command]}})
        ))
    })?;
    if let Some(mode) = mode {
        let plan = response["data"]["explain"].clone();
        ensure!(
            plan.is_object(),
            "DefraDB returned no explain plan; call query with argv:[help,explain]"
        );
        ensure!(
            serde_json::to_vec(&plan)?.len() <= 65536,
            "native plan exceeds 65536 bytes; narrow the fields/filter and repeat query explain"
        );
        let mut observations = Vec::new();
        collect_plan_observations(&plan, &mut observations);
        let findings = if observations.is_empty() {
            vec!["Native plan returned; inspect plan for its operators.".to_string()]
        } else {
            observations
        };
        let next_call = if mode == "simple" {
            let mut measured = args.clone();
            measured.options.insert("mode".into(), json!("execute"));
            json!({"when":"Only when the user requests measured execution", "tool":"query","args":measured})
        } else {
            json!({"tool":"query","args":{"argv":["count"],"collection":collection,"options":{"filter":params.filter}}})
        };
        return Ok(
            json!({"findings":findings,"plan":plan,"next_call":next_call,"mode":mode,"collection":collection}),
        );
    }
    if command == "count" {
        return Ok(json!({"total_count":response["data"]["COUNT"],"collection":collection}));
    }
    let mut rows = response["data"][collection].clone();
    ensure!(rows.is_array(), "DefraDB returned no collection result");
    let returned_count = rows.as_array().unwrap().len();
    let total_bytes = serde_json::to_vec(&rows)?.len();
    let truncated = super::truncate_field_strings(&mut rows);
    Ok(
        json!({"results":rows,"returned_count":returned_count,"collection":collection,"truncated":truncated,"total_bytes":total_bytes}),
    )
}

fn collect_plan_observations(plan: &Value, out: &mut Vec<String>) {
    match plan {
        Value::Object(object) => {
            if let Some(scan) = object.get("scanNode").and_then(Value::as_object) {
                let collection = scan
                    .get("collectionName")
                    .and_then(Value::as_str)
                    .unwrap_or("collection");
                let fact = if let Some(index) = scan.get("indexName").and_then(Value::as_str) {
                    format!("{collection}: native scan uses index {index}.")
                } else {
                    format!("{collection}: native scan has no indexName; inspect filter/prefixes for its scan bounds.")
                };
                if !out.contains(&fact) {
                    out.push(fact);
                }
            }
            for value in object.values() {
                collect_plan_observations(value, out);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_plan_observations(value, out);
            }
        }
        _ => {}
    }
}

pub fn render_result(value: Value) -> Result<String> {
    Ok(serde_json::to_string(
        &crate::self_config::Ordered::reading_order(
            value,
            &[
                "findings",
                "plan",
                "next_call",
                "mode",
                "results",
                "total_count",
                "fields",
                "returned_count",
                "collection",
                "truncated",
                "total_bytes",
            ],
        ),
    )?)
}
