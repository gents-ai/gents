use std::sync::Arc;

use anyhow::{bail, ensure, Context, Result};
use defra_node::EmbeddedNode;
use serde_json::{json, Map, Value};

use super::{build_query, CollectionScope, DefraQueryParams, MAX_LIMIT};
use crate::config_client::ConfigAccess;
use crate::retrieval::embedding::{embed_query, vector_literal};

pub(super) const SIMILARITY_FIELD: &str = "_similarity";
const DEFAULT_LIMIT: u32 = 10;

/// A failure whose recovery is a different runnable call rather than help.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub(super) struct SimilarRecovery {
    pub(super) message: String,
    pub(super) next_call: Value,
}

/// A vector field as the node describes it, for discovery and validation.
#[derive(Debug)]
pub(super) struct VectorField {
    pub(super) name: String,
    dimensions: u32,
    algorithm: &'static str,
    metric: &'static str,
    embedding: Option<schema::VectorEmbeddingDescription>,
}

impl VectorField {
    pub(super) fn describe(&self) -> Value {
        let mut value = json!({
            "dimensions": self.dimensions,
            "algorithm": self.algorithm,
            "metric": self.metric,
        });
        if let Some(embedding) = &self.embedding {
            let mut declared = json!({"provider": embedding.provider, "fields": embedding.fields});
            if !embedding.model.is_empty() {
                declared["model"] = json!(embedding.model);
            }
            value["embedding"] = declared;
            value["query"] = json!("similar");
        }
        value
    }
}

/// Facts that only the embedded node can supply, gathered before the read
/// transaction opens: query embedding is a remote call, and the read
/// transaction may be retried.
#[derive(Debug, Default)]
pub(super) struct NodeFacts {
    pub(super) vector_fields: Vec<VectorField>,
    pub(super) query: Option<QueryVector>,
}

#[derive(Debug)]
pub(super) struct QueryVector {
    pub(super) field: String,
    pub(super) vector: Vec<f64>,
}

#[derive(Debug, PartialEq)]
pub(super) struct SimilarArgs<'a> {
    pub(super) field: &'a str,
    pub(super) text: &'a str,
    pub(super) limit: u32,
}

impl<'a> SimilarArgs<'a> {
    pub(super) fn parse(options: &'a Map<String, Value>) -> Result<Self> {
        let field = options
            .get("vector_field")
            .and_then(Value::as_str)
            .filter(|field| !field.is_empty())
            .context("options.vector_field must name a vector field; call query with argv:[fields] to find fields marked vector")?;
        super::render::validate_identifier(field)
            .context("invalid vector_field; call query with argv:[fields]")?;
        let text = options
            .get("text")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .context("options.text must be nonempty text to match by meaning; call query with argv:[help,similar]")?;
        let limit = match options.get("limit") {
            None => DEFAULT_LIMIT,
            Some(value) => value
                .as_u64()
                .and_then(|limit| u32::try_from(limit).ok())
                .filter(|limit| (1..=MAX_LIMIT).contains(limit))
                .with_context(|| {
                    format!(
                        "similar limit must be 1–{MAX_LIMIT}; call query with argv:[help,similar]"
                    )
                })?,
        };
        Ok(Self { field, text, limit })
    }
}

async fn vector_fields(node: &Arc<EmbeddedNode>, collection: &str) -> Result<Vec<VectorField>> {
    let version = ConfigAccess::local(node.clone())
        .collection_version(collection)
        .await?
        .ok_or_else(|| {
            super::command::CollectionDiscoveryError(super::unknown_collection_message(collection))
        })?;
    let version: schema::CollectionVersion =
        serde_json::from_value(version).context("decoding collection version")?;
    let mut fields = Vec::new();
    for index in &version.indexes {
        let (Some(vector), [field]) = (index.vector(), index.fields.as_slice()) else {
            continue;
        };
        if fields
            .iter()
            .any(|known: &VectorField| known.name == field.name)
        {
            continue;
        }
        fields.push(VectorField {
            name: field.name.clone(),
            dimensions: vector.dimensions,
            algorithm: vector.algorithm.as_str(),
            metric: vector.metric.as_str(),
            embedding: version
                .vector_embeddings
                .iter()
                .find(|embedding| embedding.field_name == field.name)
                .cloned(),
        });
    }
    Ok(fields)
}

/// Reads vector metadata for `fields`, and embeds the query text for
/// `similar`, on the node that answers the query.
pub(super) async fn prepare(
    node: &Arc<EmbeddedNode>,
    args: &super::QueryParams,
    scope: &CollectionScope,
) -> Result<NodeFacts> {
    let command = match args.argv.as_slice() {
        [command] => command.as_str(),
        _ => return Ok(NodeFacts::default()),
    };
    if !matches!(command, "fields" | "similar") {
        return Ok(NodeFacts::default());
    }
    let collection = super::command::resolve_collection(args, scope)?;
    super::command::ensure_known_options(command, &args.options)?;
    if command == "fields" {
        return Ok(NodeFacts {
            vector_fields: vector_fields(node, collection).await.unwrap_or_default(),
            query: None,
        });
    }
    let vector_fields = vector_fields(node, collection).await?;
    let similar = SimilarArgs::parse(&args.options)?;
    let field = vector_fields
        .iter()
        .find(|field| field.name == similar.field)
        .with_context(|| {
            let known = vector_fields
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>();
            format!(
                "{collection}.{} has no vector index; vector fields: {known:?}. Call query with argv:[fields] and collection {collection:?}, or declare one through schema help indexes vector diskann",
                similar.field
            )
        })?;
    let Some(embedding) = &field.embedding else {
        bail!(
            "{collection}.{} has a vector index but no @embedding, so query text cannot be embedded into its space; declare one through schema help embeddings indexing",
            field.name
        );
    };
    let vector = embed_query(node, collection, &field.name, similar.text)
        .await
        .map_err(|error| {
            let mut options = Map::new();
            if let Some(fields) = args.options.get("fields") {
                options.insert("fields".into(), fields.clone());
            }
            options.insert("search_fields".into(), json!(embedding.fields));
            options.insert("text".into(), json!(similar.text));
            options.insert("limit".into(), json!(similar.limit));
            if let Some(filter) = args.options.get("filter") {
                options.insert("filter".into(), filter.clone());
            }
            SimilarRecovery {
                message: format!(
                    "{error:#}; similar needs that endpoint reachable. Rank by keywords instead with search (BM25)"
                ),
                next_call: json!({"tool":"query","args":{"argv":["search"],"collection":collection,"options":options}}),
            }
        })?;
    Ok(NodeFacts {
        vector_fields: Vec::new(),
        query: Some(QueryVector {
            field: field.name.clone(),
            vector,
        }),
    })
}

/// One `SIMILARITY` selection ordered by a single descending alias with a
/// limit and no offset: the only shape DefraDB answers from the vector index.
pub(super) fn build_similar_query(
    params: &DefraQueryParams,
    scope: &CollectionScope,
    query: &QueryVector,
) -> Result<String> {
    ensure!(
        !params.fields.iter().any(|field| field == SIMILARITY_FIELD),
        "{SIMILARITY_FIELD} is reserved for the native similarity score; omit it from options.fields"
    );
    let mut out = build_query(params, scope)?;
    let arguments = out
        .rfind(") {")
        .context("query renderer omitted argument boundary")?;
    out.insert_str(
        arguments,
        &format!(", order: {{_alias: {{{SIMILARITY_FIELD}: DESC}}}}"),
    );
    let selection = out
        .rfind(" } }")
        .context("query renderer omitted selection boundary")?;
    out.insert_str(
        selection,
        &format!(
            " {SIMILARITY_FIELD}: SIMILARITY({}: {{vector: {}}})",
            query.field,
            vector_literal(&query.vector)
        ),
    );
    Ok(out)
}
