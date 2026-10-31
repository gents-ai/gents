use anyhow::{anyhow, bail, ensure, Result};
use serde_json::{json, Map, Value};

use crate::documents::QueryToolDecl;
use crate::inference::llm::tool::{Tool, ToolDefinition};
use crate::store::config_client::ConfigAccess;

use super::query::{self, CollectionScope, DefraQueryParams, MAX_LIMIT};

/// Matches the bound on the `query` tool's `find` operation; deeper pages need
/// a narrower filter to avoid unbounded database work.
pub const MAX_OFFSET: u32 = 100_000;

const PLACEHOLDER_TOOL_NAME: &str = "defra_query_bound";

#[derive(Debug)]
pub struct DefraBoundQueryError(anyhow::Error);

impl std::fmt::Display for DefraBoundQueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for DefraBoundQueryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.root_cause())
    }
}

impl From<anyhow::Error> for DefraBoundQueryError {
    fn from(error: anyhow::Error) -> Self {
        Self(error)
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(transparent)]
pub struct BoundedQueryParams(pub Map<String, Value>);

#[derive(Clone)]
pub struct BoundedQueryTool {
    access: ConfigAccess,
    decl: QueryToolDecl,
    surface_id: Option<String>,
    actor: Option<::identity::Did>,
}

impl BoundedQueryTool {
    /// `access` is where the rows are read and whose principal reads
    /// them: the embedded node, or a served node's GraphQL endpoint acting
    /// as its DID.
    pub fn new(access: ConfigAccess, decl: QueryToolDecl) -> Self {
        Self {
            access,
            decl,
            surface_id: None,
            actor: None,
        }
    }

    /// The ACP actor an embedded read runs as. A GraphQL endpoint already
    /// acts as its own principal, so a tool over one refuses the call
    /// rather than read as a DID it cannot sign for.
    pub fn with_actor(mut self, actor: ::identity::Did) -> Self {
        self.actor = Some(actor);
        self
    }

    pub fn declared_by(mut self, surface_id: Option<String>) -> Self {
        self.surface_id = surface_id;
        self
    }

    pub fn is_well_formed(&self) -> bool {
        self.ensure_well_formed().is_ok()
    }

    pub(crate) async fn validate_schema(&self) -> Result<()> {
        self.ensure_well_formed()?;
        let fields = self
            .access
            .collection_fields(&self.decl.collection)
            .await?
            .ok_or_else(|| anyhow!("collection `{}` is not available", self.decl.collection))?;
        for field in self.decl.fields.iter().map(String::as_str).chain(
            self.decl
                .filter_fields
                .iter()
                .map(|field| field.name.as_str()),
        ) {
            if !fields.contains(field) {
                bail!("field `{field}` is absent from `{}`", self.decl.collection);
            }
        }
        Ok(())
    }

    fn ensure_well_formed(&self) -> Result<()> {
        if !self.decl.is_well_formed() {
            bail!(
                "query tool `{}` reached execution with an invalid declaration",
                self.decl.tool_name
            );
        }
        anyhow::ensure!(
            !self.decl.filter_fields.iter().any(|field| matches!(
                field.name.trim(),
                "fields" | "limit" | "offset" | "order" | "field_page"
            )),
            "filter field collides with a reserved query argument"
        );
        crate::documents::reject_protected_collection_name(&self.decl.collection)
    }

    async fn filter_parameters(&self) -> Result<Map<String, Value>> {
        let version = self
            .access
            .collection_version(&self.decl.collection)
            .await?
            .ok_or_else(|| anyhow!("collection `{}` is not available", self.decl.collection))?;
        let schema: ::schema::CollectionVersion = serde_json::from_value(version)?;
        self.model_filter_fields()
            .map(|field| {
                let native = schema
                    .fields
                    .iter()
                    .find(|native| native.name == field.name)
                    .ok_or_else(|| {
                        anyhow!(
                            "field `{}` is absent from `{}`",
                            field.name,
                            self.decl.collection
                        )
                    })?;
                let parameter_type = format!(
                    "{}{}",
                    native.kind.graphql_type_name().trim_end_matches('!'),
                    if field.required { "!" } else { "" }
                );
                let mut shape =
                    crate::tools::builtin::defra_write::field_parameters(&parameter_type)?;
                shape["description"] = json!(format!("Filter {} by exact match.", field.name));
                Ok((field.name.clone(), shape))
            })
            .collect()
    }

    fn model_filter_fields(&self) -> impl Iterator<Item = &crate::documents::WriteToolField> {
        self.decl
            .filter_fields
            .iter()
            .filter(|field| field.fill.is_none())
    }

    fn filled_filter_fields(&self) -> impl Iterator<Item = &crate::documents::WriteToolField> {
        self.decl
            .filter_fields
            .iter()
            .filter(|field| field.fill.is_some())
    }

    fn resolve_projection(&self, args: &Map<String, Value>) -> Result<Vec<String>> {
        match args.get("fields") {
            None | Some(Value::Null) => Ok(self.decl.fields.clone()),
            Some(Value::Array(items)) => {
                let mut fields = Vec::new();
                for item in items {
                    let Some(name) = item.as_str().map(str::trim).filter(|name| !name.is_empty())
                    else {
                        bail!(
                            "tool `{}` fields must be a list of field names",
                            self.decl.tool_name
                        );
                    };
                    if !self.decl.fields.iter().any(|allowed| allowed == name) {
                        bail!(
                            "field `{name}` is not in the projection allowlist for tool `{}`",
                            self.decl.tool_name
                        );
                    }
                    if query::is_restricted_field(&self.decl.collection, name) {
                        bail!(
                            "field `{name}` on {:?} is restricted and cannot be queried",
                            self.decl.collection
                        );
                    }
                    if !fields.iter().any(|existing| existing == name) {
                        fields.push(name.to_string());
                    }
                }
                if fields.is_empty() {
                    bail!(
                        "tool `{}` fields must list at least one allowed field",
                        self.decl.tool_name
                    );
                }
                Ok(fields)
            }
            Some(_) => bail!(
                "tool `{}` fields must be a list of field names",
                self.decl.tool_name
            ),
        }
    }

    fn resolve_limit(&self, args: &Map<String, Value>) -> Result<u32> {
        match args.get("limit") {
            None | Some(Value::Null) => Ok(MAX_LIMIT),
            Some(Value::Number(number)) => {
                let Some(limit) = number.as_u64().and_then(|value| u32::try_from(value).ok())
                else {
                    bail!(
                        "tool `{}` limit must be a positive integer",
                        self.decl.tool_name
                    );
                };
                if limit == 0 {
                    bail!(
                        "tool `{}` limit must be a positive integer",
                        self.decl.tool_name
                    );
                }
                Ok(limit.min(MAX_LIMIT))
            }
            Some(_) => bail!(
                "tool `{}` limit must be a positive integer",
                self.decl.tool_name
            ),
        }
    }

    fn resolve_offset(&self, args: &Map<String, Value>) -> Result<u32> {
        match args.get("offset") {
            None | Some(Value::Null) => Ok(0),
            Some(Value::Number(number)) => {
                let Some(offset) = number.as_u64().and_then(|value| u32::try_from(value).ok())
                else {
                    bail!(
                        "tool `{}` offset must be a non-negative integer",
                        self.decl.tool_name
                    );
                };
                ensure!(
                    offset <= MAX_OFFSET,
                    "tool `{}` offset exceeds {MAX_OFFSET}; narrow the filter",
                    self.decl.tool_name
                );
                Ok(offset)
            }
            Some(_) => bail!(
                "tool `{}` offset must be a non-negative integer",
                self.decl.tool_name
            ),
        }
    }

    // An order names only fields the tool may project: ordering by a hidden
    // field would reveal it one comparison at a time.
    fn resolve_order(&self, args: &Map<String, Value>) -> Result<Option<Value>> {
        let Some(order) = args.get("order").filter(|value| !value.is_null()) else {
            return Ok(None);
        };
        let entries: Vec<&Value> = match order {
            Value::Object(_) => vec![order],
            Value::Array(entries) => entries.iter().collect(),
            _ => bail!(
                "tool `{}` order must be an object or array of single-field objects",
                self.decl.tool_name
            ),
        };
        for entry in &entries {
            let Some((field, _)) = entry
                .as_object()
                .filter(|v| v.len() == 1)
                .and_then(|v| v.iter().next())
            else {
                bail!(
                    "tool `{}` order entries name one field each, ASC or DESC",
                    self.decl.tool_name
                );
            };
            ensure!(
                field == "_docID" || self.decl.fields.iter().any(|allowed| allowed == field),
                "tool `{}` cannot order by `{field}`; allowed: {}",
                self.decl.tool_name,
                self.decl.fields.join(", ")
            );
        }
        Ok(Some(order.clone()))
    }

    fn resolve_filter(&self, args: &Map<String, Value>) -> Result<Option<Value>> {
        for key in args.keys() {
            if matches!(
                key.as_str(),
                "fields" | "limit" | "offset" | "order" | "field_page"
            ) {
                continue;
            }
            if let Some(fill) = self
                .filled_filter_fields()
                .find(|field| field.name == *key)
                .and_then(|field| field.fill.as_ref())
            {
                bail!(
                    "{}",
                    crate::documents::runtime_filled_refusal(
                        "filter",
                        key,
                        &self.decl.tool_name,
                        self.surface_id.as_deref(),
                        fill,
                    )
                );
            }
            if !self.model_filter_fields().any(|field| field.name == *key) {
                bail!(
                    "{}",
                    crate::documents::undeclared_field_refusal(
                        "filter",
                        key,
                        &self.decl.tool_name,
                        self.surface_id.as_deref(),
                        &self
                            .model_filter_fields()
                            .map(|field| field.name.as_str())
                            .collect::<Vec<_>>(),
                    )
                );
            }
        }

        let mut filter = Map::new();
        for field in &self.decl.filter_fields {
            let value = if let Some(fill) = &field.fill {
                Some(Value::String(fill.resolve(&field.name)?))
            } else {
                args.get(&field.name).cloned()
            };
            match value {
                Some(Value::Null) | None => {
                    if field.required {
                        bail!(
                            "required filter `{}` missing for tool `{}`",
                            field.name,
                            self.decl.tool_name
                        );
                    }
                }
                Some(Value::String(text)) if text.trim().is_empty() => {
                    if field.required {
                        bail!(
                            "required filter `{}` missing for tool `{}`",
                            field.name,
                            self.decl.tool_name
                        );
                    }
                }
                Some(value) => {
                    if query::is_restricted_field(&self.decl.collection, &field.name) {
                        bail!(
                            "filter `{}` on {:?} is restricted and cannot be queried",
                            field.name,
                            self.decl.collection
                        );
                    }
                    filter.insert(field.name.clone(), json!({ "_eq": value }));
                }
            }
        }
        if filter.is_empty() {
            Ok(None)
        } else {
            Ok(Some(Value::Object(filter)))
        }
    }
}

impl Tool for BoundedQueryTool {
    const NAME: &'static str = PLACEHOLDER_TOOL_NAME;

    type Error = DefraBoundQueryError;
    type Args = BoundedQueryParams;
    type Output = String;

    fn name(&self) -> String {
        self.decl.tool_name.clone()
    }

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let mut properties = Map::new();
        properties.insert(
            "fields".to_string(),
            json!({
                "type": "array",
                "items": { "type": "string" },
                "description": format!(
                    "Optional subset of allowed fields. Omit to return all of: {}.",
                    self.decl.fields.join(", ")
                )
            }),
        );
        properties.insert(
            "limit".to_string(),
            json!({
                "type": "integer",
                "description": format!(
                    "Maximum rows to return (default {MAX_LIMIT}, capped at {MAX_LIMIT})."
                )
            }),
        );
        properties.insert(
            "offset".to_string(),
            json!({
                "type": "integer",
                "minimum": 0,
                "description": format!(
                    "Rows to skip before the page (default 0, at most {MAX_OFFSET}). A full page carries next_offset; pass it back for the next page."
                )
            }),
        );
        properties.insert(
            "order".to_string(),
            json!({
                "description": format!(
                    "Sort as an object or array of single-field objects with ASC or DESC, over: {}. _docID breaks ties, so pages do not overlap.",
                    self.decl.fields.join(", ")
                )
            }),
        );
        let mut required = Vec::new();
        match self.filter_parameters().await {
            Ok(filters) => properties.extend(filters),
            Err(error) => {
                tracing::error!(tool = %self.decl.tool_name, %error, "bounded query schema unavailable");
                return ToolDefinition {
                    name: self.decl.tool_name.clone(),
                    description: "Unavailable: collection schema could not be resolved.".into(),
                    parameters: json!({"type":"object", "properties":{}, "additionalProperties":false}),
                };
            }
        }
        for field in self.model_filter_fields() {
            if field.required {
                required.push(Value::String(field.name.clone()));
            }
        }
        let description = if self.decl.description.trim().is_empty() {
            format!(
                "Read documents from the {} collection. The collection is bound; do not name it.",
                self.decl.collection
            )
        } else {
            self.decl.description.clone()
        };
        properties.insert("field_page".into(), json!({"type":"object","additionalProperties":false,"required":["doc_id","field"],"properties":{"doc_id":{"type":"string"},"field":{"type":"string"},"offset_bytes":{"type":"integer","minimum":0},"expected_hash":{"type":"string"}},"description":"Recover a complete String field in bounded UTF-8 pages. Field must be selected. Use offset 0 first; continue with next_offset_bytes and value_hash as expected_hash. Existing filters and authorization still apply; changed values require restarting."}));
        ToolDefinition {
            name: self.decl.tool_name.clone(),
            description,
            parameters: json!({
                "type": "object",
                "properties": properties,
                "required": required,
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        self.ensure_well_formed()?;
        let fields = self.resolve_projection(&args.0)?;
        let filter = self.resolve_filter(&args.0)?;
        let limit = self.resolve_limit(&args.0)?;
        let offset = self.resolve_offset(&args.0)?;
        let order = self.resolve_order(&args.0)?;
        let params = DefraQueryParams {
            collection: self.decl.collection.clone(),
            filter,
            fields,
            limit: Some(limit),
        };
        let scope = CollectionScope::restricted(vec![self.decl.collection.clone()]);
        let mut command: super::QueryParams = params.into();
        if let Some(page) = args.0.get("field_page") {
            command.options.insert("field_page".into(), page.clone());
        } else {
            if offset > 0 {
                command.options.insert("offset".into(), json!(offset));
            }
            if let Some(order) = order {
                command.options.insert("order".into(), order);
            }
        }
        let result = match (&self.access, &self.actor) {
            (ConfigAccess::Local(local), Some(actor)) => {
                ConfigAccess::transact_local_readonly(
                    local.node(),
                    Some(actor.clone()),
                    "bounded_application_query",
                    |txn| Box::pin(super::execute_command(txn, &command, &scope)),
                )
                .await?
            }
            (ConfigAccess::Graphql(_), Some(_)) => {
                return Err(anyhow!(
                    "query tool `{}` is actor-scoped, which needs the embedded node; a GraphQL \
                     endpoint reads as its own principal",
                    self.decl.tool_name
                )
                .into())
            }
            // One request, no snapshot: a field page whose value changed
            // since the previous page is caught by its `expected_hash`.
            (_, None) => super::execute_command(&self.access, &command, &scope).await?,
        };
        let mut payload = result;
        let count = payload["returned_count"].as_u64().unwrap_or(0);
        if payload.get("results").is_some() {
            payload["count"] = json!(count);
        }
        // A full page may be the last one; the next offset says where the
        // following page starts and the caller stops at the first short one.
        if count as u32 == limit && args.0.get("field_page").is_none() {
            payload["next_offset"] = json!(offset + count as u32);
            payload["limit_note"] = json!(format!(
                "Result set hit the {limit}-row cap; continue with offset {} or narrow the filter.",
                offset + count as u32
            ));
        }
        crate::tools::output::render(
            &payload,
            &[
                "results",
                "field_page",
                "count",
                "next_offset",
                "field_recovery",
                "limit_note",
            ],
        )
        .map_err(|e| DefraBoundQueryError(anyhow!("failed to serialize query results: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::documents::{QueryToolDecl, WriteToolField, WriteToolFieldFill};
    use crate::inference::llm::tool::Tool;
    use defra_node::EmbeddedNode;

    #[tokio::test]
    async fn native_scalar_filters_match_lean_admission() {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        node.add_collection(
            "type ScalarRecord { text: String count: Int measure: Float64 enabled: Boolean }",
        )
        .await
        .unwrap();
        crate::store::config_client::ConfigAccess::write_local_response(
            &node, "test.scalar_filter.seed",
            r#"mutation { add_ScalarRecord(input: {text: "measurement", count: 86, measure: 0.5, enabled: false}) {_docID} }"#,
        ).await.unwrap();
        for case in crate::lean_vocab_test::lean_write_input_cases() {
            if case["nullable"] != false || case["required"] != true || case["filled"] != false {
                continue;
            }
            let (field, kind) = match case["expected"].as_str().unwrap() {
                "text" => ("text", "string"),
                "integer" => ("count", "integer"),
                "number" => ("measure", "number"),
                "boolean" => ("enabled", "boolean"),
                other => panic!("unknown kind {other}"),
            };
            let value = match case["actual"].as_str() {
                None => None,
                Some("text") => Some(json!("measurement")),
                Some("integer") => Some(json!(86)),
                Some("number") => Some(json!(0.5)),
                Some("boolean") => Some(json!(false)),
                Some("array") => Some(json!([1])),
                Some("object") => Some(json!({"x":1})),
                Some("null") => Some(Value::Null),
                other => panic!("unknown kind {other:?}"),
            };
            let tool = BoundedQueryTool::new(
                ConfigAccess::local(node.clone()),
                QueryToolDecl {
                    tool_name: "read_scalar".into(),
                    collection: "ScalarRecord".into(),
                    description: String::new(),
                    fields: vec![field.into()],
                    filter_fields: vec![WriteToolField {
                        name: field.into(),
                        required: true,
                        fill: None,
                    }],
                },
            );
            let definition = tool.definition(String::new()).await;
            assert_eq!(
                definition.parameters["properties"][field]["type"],
                json!(kind)
            );
            let args = value
                .map(|value| Map::from_iter([(field.into(), value)]))
                .unwrap_or_default();
            let result = tool.call(BoundedQueryParams(args)).await;
            assert_eq!(
                result.is_ok(),
                case["accepted"] == true,
                "{case}: {result:?}"
            );
            if case["expected"] == case["actual"] {
                let payload: Value = serde_json::from_str(&result.unwrap()).unwrap();
                assert_eq!(payload["count"], 1, "{case}");
            }
        }
    }

    async fn node_with_findings() -> Arc<EmbeddedNode> {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        node.add_collection(
            r#"
            type CandidateFinding {
                run_id: String
                finding_id: String
                title: String
            }
        "#,
        )
        .await
        .unwrap();
        node.execute(
            r#"mutation { add_CandidateFinding(input: {
                run_id: "run-42", finding_id: "f1", title: "graphql"
            }) { _docID } }"#,
        )
        .await;
        node.execute(
            r#"mutation { add_CandidateFinding(input: {
                run_id: "other", finding_id: "f2", title: "other-run"
            }) { _docID } }"#,
        )
        .await;
        node
    }

    fn decl() -> QueryToolDecl {
        QueryToolDecl {
            tool_name: "query_candidate_finding".into(),
            collection: "CandidateFinding".into(),
            description: "Load candidate findings for this run.".into(),
            fields: vec!["finding_id".into(), "title".into(), "run_id".into()],
            filter_fields: vec![WriteToolField {
                name: "run_id".into(),
                required: false,
                fill: Some(WriteToolFieldFill::Correlation),
            }],
        }
    }

    #[tokio::test]
    async fn field_recovery_preserves_complete_contract_and_runtime_scope() {
        let node = node_with_findings().await;
        let text = format!("{}{}", "é".repeat(2001), "😀".repeat(300));
        let mutation = format!("mutation {{ update_CandidateFinding(filter: {{finding_id: {{_eq: \"f1\"}}}}, input: {{title: \"{}\"}}) {{_docID}} }}", crate::store::graphql::escape_graphql_string(&text));
        let response = crate::store::config_client::ConfigAccess::write_local_response(
            &node,
            "test.field_recovery.seed",
            &mutation,
        )
        .await
        .unwrap();
        let doc = response.data.as_ref().unwrap()["update_CandidateFinding"][0]["_docID"]
            .as_str()
            .unwrap()
            .to_owned();
        let tool = BoundedQueryTool::new(ConfigAccess::local(node.clone()), decl());
        crate::tools::call::lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
            None, tokio_util::sync::CancellationToken::new(), None, None, None,
            Some("run-42".into()), Default::default(), false, async {
                let initial: Value = serde_json::from_str(&Tool::call(&tool, BoundedQueryParams(json!({"fields":["title"]}).as_object().unwrap().clone())).await.unwrap()).unwrap();
                assert_eq!(initial["truncated"], true);
                assert_eq!(initial["field_recovery"][0]["total_bytes"], text.len());
                assert_eq!(initial["field_recovery"][0]["doc_id"], doc);
                assert_eq!(initial["field_recovery"][0]["row"], 0);
                assert!(initial["results"][0].get("_docID").is_none());
                assert!(initial["total_bytes"].as_u64().unwrap() > text.len() as u64);
                let mut offset=0; let mut hash=None; let mut recovered=String::new();
                loop {
                    let mut page=json!({"doc_id":doc,"field":"title","offset_bytes":offset});
                    if let Some(hash)=&hash { page["expected_hash"]=json!(hash); }
                    let out: Value=serde_json::from_str(&Tool::call(&tool, BoundedQueryParams(json!({"fields":["title"],"field_page":page}).as_object().unwrap().clone())).await.unwrap()).unwrap();
                    let page=&out["field_page"];
                    recovered.push_str(page["text"].as_str().unwrap());
                    hash=Some(page["value_hash"].as_str().unwrap().to_owned());
                    offset=page["next_offset_bytes"].as_u64().unwrap();
                    if page["complete"] == true { break; }
                }
                assert_eq!(recovered,text);
                for page in [json!({"doc_id":doc,"field":"run_id"}),json!({"doc_id":doc,"field":"title","offset_bytes":1}),json!({"doc_id":doc,"field":"title","offset_bytes":2}),json!({"doc_id":doc,"field":"title","expected_hash":"changed"})] {
                    assert!(Tool::call(&tool,BoundedQueryParams(json!({"fields":["title"],"field_page":page}).as_object().unwrap().clone())).await.is_err());
                }
                let other = crate::store::config_client::ConfigAccess::local(node.clone()).execute("{ CandidateFinding(filter: {finding_id: {_eq: \"f2\"}}) {_docID} }").await.unwrap();
                let other_doc=&other["data"]["CandidateFinding"][0]["_docID"];
                assert!(Tool::call(&tool,BoundedQueryParams(json!({"fields":["title"],"field_page":{"doc_id":other_doc,"field":"title"}}).as_object().unwrap().clone())).await.is_err());
                crate::store::config_client::ConfigAccess::write_local(&node,"test.field_recovery.change",&format!("mutation {{update_CandidateFinding(docID: \"{}\", input: {{title: \"changed\"}}) {{_docID}}}}",crate::store::graphql::escape_graphql_string(&doc))).await.unwrap();
                assert!(Tool::call(&tool,BoundedQueryParams(json!({"fields":["title"],"field_page":{"doc_id":doc,"field":"title","offset_bytes":2,"expected_hash":hash.unwrap()}}).as_object().unwrap().clone())).await.is_err());
            }).await;
    }

    #[tokio::test]
    async fn actor_scoped_query_completes_while_mutation_gate_is_held() {
        let node = node_with_findings().await;
        let holder = crate::store::config_client::ConfigApplyTxn::begin_local(&node, None)
            .await
            .unwrap();
        let mut declaration = decl();
        declaration.filter_fields.clear();
        let tool = BoundedQueryTool::new(ConfigAccess::local(Arc::clone(&node)), declaration)
            .with_actor(
                ::identity::Did::new("did:key:z6MkfXG2FkNy3u7Eg3jm8e2YQpGz7Z1JqWgHDAP1hLk9r2bR")
                    .unwrap(),
            );
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            Tool::call(&tool, BoundedQueryParams(Map::new())),
        )
        .await
        .expect("actor read must not wait for mutation gate")
        .unwrap();
        assert!(result.contains("f1"));
        assert!(result.contains("f2"));
        assert!(result.contains("\"count\": 2"));
        holder.commit().await.unwrap();
    }

    #[tokio::test]
    async fn queries_only_the_correlated_run() {
        let node = node_with_findings().await;
        let tool = BoundedQueryTool::new(ConfigAccess::local(Arc::clone(&node)), decl());
        let out =
            crate::tools::call::lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
                None,
                tokio_util::sync::CancellationToken::new(),
                None,
                None,
                None,
                Some("run-42".to_string()),
                Default::default(),
                false,
                async {
                    Tool::call(&tool, BoundedQueryParams(Map::new()))
                        .await
                        .expect("query")
                },
            )
            .await;
        assert!(out.contains("f1"));
        assert!(!out.contains("other-run"));
        assert!(out.contains("\"count\": 1"));
    }

    #[tokio::test]
    async fn hides_filled_filter_and_rejects_model_override() {
        let node = node_with_findings().await;
        let tool = BoundedQueryTool::new(ConfigAccess::local(node), decl());
        let definition = Tool::definition(&tool, String::new()).await;
        let properties = definition.parameters["properties"].as_object().unwrap();
        assert!(properties.contains_key("fields"));
        assert!(!properties.contains_key("run_id"));
        assert!(!properties.contains_key("collection"));

        let mut args = Map::new();
        args.insert("run_id".into(), json!("model-value"));
        let err = Tool::call(&tool, BoundedQueryParams(args))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("runtime-filled"));
    }

    #[tokio::test]
    async fn rejects_fields_outside_the_allowlist() {
        let node = node_with_findings().await;
        let tool = BoundedQueryTool::new(ConfigAccess::local(node), decl());
        let mut args = Map::new();
        args.insert("fields".into(), json!(["_docID"]));
        crate::tools::call::lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
            None,
            tokio_util::sync::CancellationToken::new(),
            None,
            None,
            None,
            Some("run-42".to_string()),
            Default::default(),
            false,
            async {
                let err = Tool::call(&tool, BoundedQueryParams(args))
                    .await
                    .unwrap_err();
                assert!(err.to_string().contains("allowlist"));
            },
        )
        .await;
    }

    #[tokio::test]
    async fn rejects_a_persisted_protected_collection_at_execution() {
        let node = node_with_findings().await;
        let mut protected = decl();
        protected.collection = gents_protocol::schemas::EVAL_VERDICT_NAME.into();
        let tool = BoundedQueryTool::new(ConfigAccess::local(node), protected);
        assert!(!tool.is_well_formed());
        let err = Tool::call(&tool, BoundedQueryParams(Map::new()))
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("protected"), "{err:#}");
    }

    #[tokio::test]
    async fn projection_fields_do_not_grant_filter_arguments() {
        let node = node_with_findings().await;
        let tool = BoundedQueryTool::new(ConfigAccess::local(node), decl())
            .declared_by(Some("findings".into()));
        let args = serde_json::from_value(json!({"title":"graphql"})).unwrap();
        let error = Tool::call(&tool, BoundedQueryParams(args))
            .await
            .unwrap_err()
            .to_string();
        for detail in [
            "filter `title` is not permitted",
            "filter_fields declares filter arguments",
            r#"surface "findings", entry "query_candidate_finding""#,
            "next request",
        ] {
            assert!(error.contains(detail), "{error}");
        }
    }

    #[tokio::test]
    async fn rejects_number_for_native_string_filter() {
        let node = node_with_findings().await;
        let tool = BoundedQueryTool::new(
            ConfigAccess::local(node),
            QueryToolDecl {
                tool_name: "query_candidate_finding".into(),
                collection: "CandidateFinding".into(),
                description: String::new(),
                fields: vec!["finding_id".into(), "title".into()],
                filter_fields: vec![WriteToolField {
                    name: "title".into(),
                    required: false,
                    fill: None,
                }],
            },
        );
        let mut args = Map::new();
        args.insert("title".into(), json!(1));
        let err = Tool::call(&tool, BoundedQueryParams(args))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("String"), "{err}");
    }

    async fn node_with_many_findings(rows: usize) -> Arc<EmbeddedNode> {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        node.add_collection(
            "type CandidateFinding { run_id: String finding_id: String title: String }",
        )
        .await
        .unwrap();
        for index in 0..rows {
            node.execute(&format!(
                r#"mutation {{ add_CandidateFinding(input: {{ run_id: "run-42", finding_id: "f{index:03}", title: "t{index:03}" }}) {{ _docID }} }}"#
            ))
            .await;
        }
        node
    }

    fn titles(payload: &Value) -> Vec<String> {
        payload["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["finding_id"].as_str().unwrap().to_owned())
            .collect()
    }

    /// the finding tool without a runtime-filled filter: pages need no request context
    fn plain_decl() -> QueryToolDecl {
        QueryToolDecl {
            filter_fields: Vec::new(),
            ..decl()
        }
    }

    async fn call_unscoped(tool: &BoundedQueryTool, args: Value) -> Result<Value> {
        let text = Tool::call(tool, BoundedQueryParams(args.as_object().unwrap().clone()))
            .await
            .map_err(|error| anyhow!("{error}"))?;
        Ok(serde_json::from_str(&text)?)
    }

    #[tokio::test]
    async fn pages_through_the_cap_with_offset_and_a_stable_order() {
        let node = node_with_many_findings(7).await;
        let tool = BoundedQueryTool::new(ConfigAccess::local(node), plain_decl());
        let first = call_unscoped(
            &tool,
            json!({"fields":["finding_id"],"limit":3,"order":{"finding_id":"ASC"}}),
        )
        .await
        .unwrap();
        assert_eq!(titles(&first), ["f000", "f001", "f002"]);
        assert_eq!(first["count"], 3);
        assert_eq!(first["next_offset"], 3);
        assert!(first["limit_note"].as_str().unwrap().contains("offset 3"));
        let second = call_unscoped(
            &tool,
            json!({"fields":["finding_id"],"limit":3,"offset":3,"order":{"finding_id":"ASC"}}),
        )
        .await
        .unwrap();
        assert_eq!(titles(&second), ["f003", "f004", "f005"]);
        assert_eq!(second["next_offset"], 6);
        let last = call_unscoped(
            &tool,
            json!({"fields":["finding_id"],"limit":3,"offset":6,"order":{"finding_id":"ASC"}}),
        )
        .await
        .unwrap();
        assert_eq!(titles(&last), ["f006"]);
        assert_eq!(last["count"], 1);
        assert!(last.get("next_offset").is_none(), "{last}");
        assert!(last.get("limit_note").is_none(), "{last}");
        let descending = call_unscoped(
            &tool,
            json!({"fields":["finding_id"],"limit":2,"order":[{"finding_id":"DESC"}]}),
        )
        .await
        .unwrap();
        assert_eq!(titles(&descending), ["f006", "f005"]);
        let past_the_end = call_unscoped(
            &tool,
            json!({"fields":["finding_id"],"limit":3,"offset":40}),
        )
        .await
        .unwrap();
        assert_eq!(past_the_end["count"], 0);
        assert!(past_the_end.get("next_offset").is_none());
    }

    #[tokio::test]
    async fn refuses_an_order_outside_the_allowlist_and_a_bad_offset() {
        let node = node_with_many_findings(1).await;
        let tool = BoundedQueryTool::new(ConfigAccess::local(node), plain_decl());
        let hidden = call_unscoped(&tool, json!({"order":{"_version":"ASC"}}))
            .await
            .unwrap_err()
            .to_string();
        assert!(hidden.contains("cannot order by `_version`"), "{hidden}");
        let shape = call_unscoped(&tool, json!({"order":"finding_id"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            shape.contains("order must be an object or array"),
            "{shape}"
        );
        let negative = call_unscoped(&tool, json!({"offset":-1}))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            negative.contains("offset must be a non-negative integer"),
            "{negative}"
        );
        let deep = call_unscoped(&tool, json!({"offset": MAX_OFFSET + 1}))
            .await
            .unwrap_err()
            .to_string();
        assert!(deep.contains("offset exceeds 100000"), "{deep}");
        let by_doc_id = call_unscoped(
            &tool,
            json!({"fields":["finding_id"],"order":{"_docID":"ASC"}}),
        )
        .await
        .unwrap();
        assert_eq!(by_doc_id["count"], 1);
    }

    #[tokio::test]
    async fn rejects_zero_limit() {
        let node = node_with_findings().await;
        let tool = BoundedQueryTool::new(ConfigAccess::local(node), decl());
        let mut args = Map::new();
        args.insert("limit".into(), json!(0));
        let err =
            crate::tools::call::lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
                None,
                tokio_util::sync::CancellationToken::new(),
                None,
                None,
                None,
                Some("run-42".to_string()),
                Default::default(),
                false,
                async {
                    Tool::call(&tool, BoundedQueryParams(args))
                        .await
                        .unwrap_err()
                },
            )
            .await;
        assert!(err.to_string().contains("positive integer"));
    }
}
