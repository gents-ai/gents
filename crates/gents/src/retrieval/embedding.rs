use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;

use crate::config_client::ConfigAccess;

/// Query embedding runs on a tool call, not on the inference path, but a model
/// waiting on an unreachable endpoint should get an actionable error promptly.
const QUERY_EMBEDDING_TIMEOUT: Duration = Duration::from_secs(20);

/// Where DefraDB sends a vector field's text: the field's `@embedding`
/// provider and model, at its URL, else the node's embedding URL, else the
/// provider default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingTarget {
    pub provider: String,
    pub url: String,
    pub model: String,
}

impl std::fmt::Display for EmbeddingTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} model {} at {}", self.provider, self.model, self.url)
    }
}

pub async fn embedding_target(
    node: &Arc<EmbeddedNode>,
    collection: &str,
    field: &str,
) -> Result<EmbeddingTarget> {
    let version = ConfigAccess::local(node.clone())
        .collection_version(collection)
        .await?
        .with_context(|| format!("collection {collection} does not exist"))?;
    let version: schema::CollectionVersion =
        serde_json::from_value(version).context("decoding collection version")?;
    let embedding = version
        .vector_embeddings
        .iter()
        .find(|embedding| embedding.field_name == field)
        .with_context(|| {
            format!("{collection}.{field} has no @embedding; semantic search needs one to embed query text")
        })?;
    let resolved =
        db::search::embedding::resolve_embedding_config(embedding, node.embedding_config())
            .map_err(|missing| {
                anyhow::anyhow!(
                    "{collection}.{field} has no embedding {}; set it in @embedding or on the node",
                    match missing {
                        db::search::embedding::MissingEmbeddingConfig::Url => "URL",
                        db::search::embedding::MissingEmbeddingConfig::Model => "model",
                    }
                )
            })?;
    Ok(EmbeddingTarget {
        provider: embedding.provider.clone(),
        url: resolved.url.to_owned(),
        model: resolved.model.to_owned(),
    })
}

/// Embeds `text` into the vector space of `collection.field`'s documents.
pub async fn embed_query(
    node: &Arc<EmbeddedNode>,
    collection: &str,
    field: &str,
    text: &str,
) -> Result<Vec<f64>> {
    let target = embedding_target(node, collection, field).await?;
    let config = db::EmbeddingClientConfig::new()
        .with_url(&target.url)
        .with_model(&target.model)
        .with_api_key(node.embedding_config().api_key.clone());
    let vector = tokio::time::timeout(
        QUERY_EMBEDDING_TIMEOUT,
        db::search::embedding::embed_text_with_provider(&config, &target.provider, text, None),
    )
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "embedding endpoint {target} did not answer within {QUERY_EMBEDDING_TIMEOUT:?}"
        )
    })?
    .map_err(|error| anyhow::anyhow!("embedding endpoint {target} failed: {error}"))?;
    anyhow::ensure!(
        !vector.is_empty(),
        "embedding endpoint {target} returned an empty vector"
    );
    Ok(vector)
}

/// A GraphQL vector literal for `SIMILARITY(field: {vector: …})`.
pub(crate) fn vector_literal(vector: &[f64]) -> String {
    let mut literal = String::with_capacity(vector.len() * 12 + 2);
    literal.push('[');
    for (index, value) in vector.iter().enumerate() {
        if index > 0 {
            literal.push(',');
        }
        literal.push_str(&format!("{value}"));
    }
    literal.push(']');
    literal
}
