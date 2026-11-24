use anyhow::{anyhow, bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::relation::{self, ResolveError, SelectionRefusal};
use super::schema::discovery_payload;
use super::{build_query, CollectionScope, DefraQueryParams};
use gents_store::config_client::schema_introspection::validate_identifier;
use gents_store::config_client::{ConfigClientError, ConfigRead, RerunNote, TransactionFailure};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(crate) struct CollectionDiscoveryError(String);

#[derive(Debug, thiserror::Error)]
pub(crate) enum QueryCommandError {
    #[error(transparent)]
    Discovery(#[from] CollectionDiscoveryError),
    #[error(transparent)]
    Refused(#[from] SelectionRefusal),
    #[error(fmt = gents_protocol::error_chain::held)]
    Access(#[from] ConfigClientError),
    #[error(fmt = gents_protocol::error_chain::held)]
    Other(#[from] anyhow::Error),
}

impl QueryCommandError {
    /// A transparent variant hides the error it wraps from `source()`, so the
    /// erased form is the wrapped error itself, which callers that classify an
    /// `anyhow` chain still find.
    pub(super) fn erased(self) -> anyhow::Error {
        match self {
            Self::Discovery(error) => error.into(),
            Self::Refused(error) => error.into(),
            Self::Access(error) => error.into(),
            Self::Other(error) => error,
        }
    }
}

impl TransactionFailure for QueryCommandError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Access(error) => Some(error),
            Self::Other(error) => TransactionFailure::config_client_error(error),
            Self::Discovery(_) | Self::Refused(_) => None,
        }
    }

    fn rerun_requested(&self) -> Option<&RerunNote> {
        match self {
            Self::Access(error) => TransactionFailure::rerun_requested(error),
            Self::Other(error) => TransactionFailure::rerun_requested(error),
            Self::Discovery(_) | Self::Refused(_) => None,
        }
    }
}

impl From<ResolveError> for QueryCommandError {
    fn from(error: ResolveError) -> Self {
        match error {
            ResolveError::Refused(refusal) => Self::Refused(refusal),
            ResolveError::Other(error) => Self::Other(error),
        }
    }
}

pub(super) const COLLECTION_GUIDANCE: &str = "collection names a GraphQL type, not a datastore surface or tool. Discover names with schema collection list; inspect a surface with config datastore get SURFACE_ID";

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

/// The words a model reaches for when it wants to change a document.
const WRITE_VERBS: [&str; 7] = [
    "write", "create", "insert", "update", "delete", "mutate", "add",
];

/// The write verb `args` asks `query` for, which only reads.
pub(super) fn write_verb(args: &QueryParams) -> Option<&str> {
    args.argv
        .first()
        .map(String::as_str)
        .filter(|verb| WRITE_VERBS.contains(verb))
}

const COMMANDS: [&str; 5] = ["fields", "find", "count", "search", "explain"];

pub fn query_help(command: Option<&str>) -> Result<&'static str> {
    match command {
        None => Ok("query reads documents. argv: [fields], [find], [count], [search], [explain], [help,COMMAND]. Supply collection. fields lists names/types; find returns a bounded page; count totals matching rows; search ranks by BM25 keywords or vector similarity. Configuration uses config; definitions use schema."),
        Some("fields") => Ok("{argv:[\"fields\"],collection:\"Shipment\"}. Returns available field names and types; no options."),
        Some("find") => Ok("{argv:[\"find\"],collection:\"Shipment\",options:{fields:[\"reference\",\"status\",\"carrier.name\"],filter:{status:{_eq:\"queued\"}},order:[{priority:\"ASC\"}],offset:0,limit:20}}. Fields are required; relation.field reads a related document's field, one relationship deep. A list relationship shows up to 20 per row; truncated_relations names rows with more. Grants and ACP apply to related documents. Default limit 50, maximum 1000; offset ≤100000. Orders use ASC/DESC; _docID is appended as a tie-breaker. Pagination observes the current datastore, not a retained snapshot. Filters use native DefraDB operators: _eq,_neq,_gt,_geq,_lt,_leq,_in,_nin,_like,_nlike,_ilike,_nilike; compose with _and,_or,_not; cross a relationship as {carrier:{name:{_eq:\"Ada\"}}}. A repeated key keeps only the last, so join conditions on one field with _and. Use search for BM25 keywords or vector similarity. Truncated strings: help field_page. Provenance: help signer."),
        Some("field_page") => Ok("{argv:[\"find\"],collection:\"Shipment\",options:{fields:[\"notes\"],field_page:{doc_id:\"DOC_ID\",field:\"notes\",offset_bytes:0}}} reads a truncated string; field_recovery names the call. Later pages require expected_hash from value_hash; offsets are UTF-8 byte boundaries. Changed values require restarting at zero. Empty values and offset at end return a complete empty page. Field must remain in fields; existing filters and authorization apply. Not combinable with offset, order or signer."),
        Some("signer") => Ok("Set options.signer:true on find to add _signer:{created,latest,writers,unsigned}, the document's provenance from its verified version signatures: the DIDs behind its first and newest versions (null when unsigned or ambiguous), every DID that signed any version (an edit in between shows here even when created and latest agree), and unsigned:true when some version cannot be attributed (unsigned, or missing from the readable history). Not combinable with field_page."),
        Some("search") => Ok("Ranks rows, highest _score first. Keywords: {argv:[\"search\"],collection:\"Reports\",options:{fields:[\"duty\"],search_fields:[\"duty\"],text:\"crew duty\",limit:5}} scores BM25 over String fields; zero-score rows may follow matches. Vectors: options:{fields:[\"sku\"],vector_field:\"coordinates\",vector:[0.1,0.2,0.3]} scores by the field's vector index metric. Default limit 10, maximum 1000. fields, filter and order work as in find. Indexes: schema help indexes fulltext | vector."),
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
        .rfind(") {")
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
    execute_typed_command(access, args, scope)
        .await
        .map_err(QueryCommandError::erased)
}

pub(super) async fn execute_typed_command(
    access: &dyn ConfigRead,
    args: &QueryParams,
    scope: &CollectionScope,
) -> std::result::Result<Value, QueryCommandError> {
    let mut argv = args.argv.iter().map(String::as_str).collect::<Vec<_>>();
    if argv.last() == Some(&"--help") {
        argv.pop();
        argv.insert(0, "help");
    }
    if argv.first() == Some(&"help") {
        if args.collection.is_some() || !args.options.is_empty() {
            return Err(anyhow!("help accepts argv only").into());
        }
        if argv.len() > 2 {
            return Err(anyhow!("help accepts one command").into());
        }
        return Ok(json!({"help":query_help(argv.get(1).copied())?}));
    }
    if argv.len() != 1 {
        return Err(anyhow!(
            "use argv:[fields|find|count|search|explain]; call query with {{\"argv\":[\"help\"]}}"
        )
        .into());
    }
    let command = argv[0];
    if !COMMANDS.contains(&command) {
        return Err(
            anyhow!("unknown query command; call query with {{\"argv\":[\"help\"]}}").into(),
        );
    }
    let collection = args.collection.as_deref().ok_or_else(|| {
        CollectionDiscoveryError(format!("collection is required; {COLLECTION_GUIDANCE}"))
    })?;
    validate_identifier(collection)
        .map_err(|error| CollectionDiscoveryError(format!("{error}; {COLLECTION_GUIDANCE}")))?;
    scope.ensure_allowed(collection)?;
    let allowed: &[&str] = match command {
        "fields" => &[],
        "count" => &["filter"],
        "find" => &[
            "fields",
            "filter",
            "limit",
            "offset",
            "order",
            "field_page",
            "signer",
        ],
        "search" => &[
            "fields",
            "filter",
            "limit",
            "order",
            "search_fields",
            "text",
            "vector_field",
            "vector",
        ],
        "explain" => &["fields", "filter", "limit", "offset", "order", "mode"],
        _ => unreachable!(),
    };
    for key in args.options.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(anyhow!("unknown option {key:?}; accepted options: {allowed:?}").into());
        }
    }
    let observed = relation::observe(access, collection)
        .await?
        .ok_or_else(|| CollectionDiscoveryError(super::unknown_collection_message(collection)))?;
    if command == "fields" {
        return Ok(discovery_payload(&observed));
    }
    let requested: Vec<String> = if command == "count" {
        vec!["_docID".into()]
    } else {
        serde_json::from_value(args.options.get("fields").cloned().context(
            "options.fields is required; call query with argv:[fields] to discover names",
        )?)
        .context("options.fields must be an array of field names")?
    };
    let (fields, relations) = relation::resolve(access, &observed, &requested, scope).await?;
    let limit = args
        .options
        .get("limit")
        .map(|v| serde_json::from_value(v.clone()))
        .transpose()
        .context("limit must be an unsigned integer")?;
    let mut params = DefraQueryParams {
        collection: collection.into(),
        fields,
        filter: args.options.get("filter").cloned(),
        limit,
    };
    if !relations.is_empty() && !params.fields.iter().any(|field| field == "_docID") {
        params.fields.push("_docID".into());
    }
    build_query(&params, scope)?;
    if let Some(filter) = params.filter.as_ref() {
        super::native_filter::validate_filter(access, collection, filter, scope).await?;
    }
    let field_page: Option<super::field_page::FieldPage> = args
        .options
        .get("field_page")
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .context("invalid field_page")?;
    let signer = match args.options.get("signer") {
        None | Some(Value::Bool(false)) => false,
        Some(Value::Bool(true)) => true,
        Some(_) => return Err(anyhow!("signer must be true or false").into()),
    };
    let selected_fields = params.fields.clone();
    if let Some(page) = &field_page {
        if !relations.is_empty() {
            return Err(anyhow!(
                "field_page reads one field of this collection; omit relation.field"
            )
            .into());
        }
        page.validate(&selected_fields)?;
        if args.options.contains_key("offset") || args.options.contains_key("order") {
            return Err(anyhow!("field_page cannot combine with row offset/order").into());
        }
        if signer {
            return Err(anyhow!(
                "field_page cannot combine with signer; read _signer with find without field_page"
            )
            .into());
        }
        let exact = json!({"_docID":{"_eq":page.doc_id}});
        params.filter = Some(match params.filter.take().filter(|v| !v.is_null()) {
            Some(filter) => json!({"_and":[filter, exact]}),
            None => exact,
        });
        params.fields = vec![page.field.clone(), "_docID".into()];
        params.fields.dedup();
        params.limit = Some(1);
    } else if matches!(command, "find" | "search")
        && !params.fields.iter().any(|field| field == "_docID")
    {
        params.fields.push("_docID".into());
    }
    let query = if command == "count" {
        let filter = params
            .filter
            .as_ref()
            .filter(|v| !v.is_null())
            .map(super::render::render_filter)
            .transpose()?;
        let arguments = filter.map(|f| format!("filter: {f}")).unwrap_or_default();
        format!("{{ COUNT({collection}: {{ {arguments} }}) }}")
    } else if command == "search" {
        super::search::build_search_query(&params, scope, &observed, &args.options)?
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
    let query = if relations.is_empty() {
        query
    } else {
        let selection = query
            .rfind(" } }")
            .context("query renderer omitted selection boundary")?;
        let mut query = query;
        query.insert_str(selection, &relation::render(&relations));
        query
    };
    let mode = if command == "explain" {
        let mode = args
            .options
            .get("mode")
            .map(|v| v.as_str().context("mode must be simple or execute"))
            .transpose()?
            .unwrap_or("simple");
        if !matches!(mode, "simple" | "execute") {
            return Err(anyhow!(
                "mode must be simple or execute; call query with argv:[help,explain]"
            )
            .into());
        }
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
        let error = anyhow::Error::from(error);
        let diagnostic =
            super::diagnose_failed_query(&params, Some(&observed.schema), &format!("{error:#}"));
        error.context(format!(
            "{diagnostic}; recovery: {}",
            json!({"tool":"query","args":{"argv":["help",command]}})
        ))
    })?;
    if let Some(mode) = mode {
        let plan = response["data"]["explain"].clone();
        if !plan.is_object() {
            return Err(anyhow!(
                "DefraDB returned no explain plan; call query with argv:[help,explain]"
            )
            .into());
        }
        if serde_json::to_vec(&plan)
            .map_err(anyhow::Error::from)?
            .len()
            > 65536
        {
            return Err(anyhow!(
                "native plan exceeds 65536 bytes; narrow the fields/filter and repeat query explain"
            )
            .into());
        }
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
    if !rows.is_array() {
        return Err(anyhow!("DefraDB returned no collection result").into());
    }
    if let Some(page) = field_page {
        let rows = rows.as_array().unwrap();
        if rows.len() != 1 {
            return Err(anyhow!(
                "field_page document is unavailable under the resolved query filters and authorization"
            )
            .into());
        }
        return Ok(
            json!({"collection":collection,"field_page":page.read(&rows[0], &selected_fields)?}),
        );
    }
    let truncated_relations = relation::page_related(&mut rows, &relations);
    let recovery = if command == "find" {
        super::field_page::recovery_metadata(&rows)
    } else {
        Vec::new()
    };
    if command == "find" && signer {
        for row in rows.as_array_mut().unwrap() {
            let doc_id = row["_docID"]
                .as_str()
                .context("a find row came back without _docID")?
                .to_owned();
            let signers = gents_store::provenance::document_signers(access, &doc_id)
                .await
                .map_err(anyhow::Error::from)
                .with_context(|| format!("name the signers of document {doc_id}"))?;
            row["_signer"] = serde_json::to_value(signers).map_err(anyhow::Error::from)?;
        }
    }
    if !requested.iter().any(|field| field == "_docID") {
        for row in rows.as_array_mut().unwrap() {
            if let Some(object) = row.as_object_mut() {
                object.remove("_docID");
            }
        }
    }
    let returned_count = rows.as_array().unwrap().len();
    let total_bytes = serde_json::to_vec(&rows)
        .map_err(anyhow::Error::from)?
        .len();
    let truncated_fields = super::truncation_sidecar::truncated_fields(&rows);
    let truncated = super::truncate_field_strings(&mut rows);
    let mut result = json!({"results":rows,"returned_count":returned_count,"collection":collection,"truncated":truncated,"total_bytes":total_bytes});
    if !truncated_fields.is_empty() {
        result["truncated_fields"] = json!(truncated_fields);
    }
    if !recovery.is_empty() {
        result["field_recovery"] = json!(recovery);
    }
    if !truncated_relations.is_empty() {
        result["truncated_relations"] = json!(truncated_relations);
    }
    if command == "search" {
        result["ranking"] = json!(super::search::ranking_name(&args.options));
    }
    Ok(result)
}

fn collect_plan_observations(plan: &Value, out: &mut Vec<String>) {
    match plan {
        Value::Object(object) => {
            if let Some(scan) = object.get("scanNode").and_then(Value::as_object) {
                let collection = scan
                    .get("collectionName")
                    .and_then(Value::as_str)
                    .unwrap_or("collection");
                let counters = [
                    ("docFetches", "document fetches"),
                    ("fieldFetches", "field fetches"),
                    ("indexFetches", "index fetches"),
                ]
                .into_iter()
                .filter_map(|(key, label)| {
                    scan.get(key)
                        .and_then(Value::as_u64)
                        .map(|count| format!("{count} {label}"))
                })
                .collect::<Vec<_>>();
                let fact = if !counters.is_empty() {
                    format!(
                        "{collection}: native execution recorded {}.",
                        counters.join(", ")
                    )
                } else if let Some(index) = scan.get("indexName").and_then(Value::as_str) {
                    format!("{collection}: native plan selects index {index}.")
                } else {
                    format!("{collection}: native plan has no indexName; inspect its scan bounds, including direct document lookups.")
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
        &gents_session::session::output_render::Ordered::reading_order(
            value,
            &[
                "findings",
                "plan",
                "results",
                "field_page",
                "total_count",
                "fields",
                "truncated_fields",
                "truncated_relations",
                "field_recovery",
                "next_call",
                "mode",
                "returned_count",
                "collection",
                "ranking",
                "truncated",
                "total_bytes",
            ],
        ),
    )?)
}
