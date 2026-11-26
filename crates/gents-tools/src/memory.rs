//! Agent memory: a DefraDB collection with a DiskANN vector index and a BM25
//! full-text index, an in-process multilingual embedder, hybrid retrieval, and
//! a bounded `memory` tool.
//!
//! The collection is installed by the runtime with the household `children`
//! policy. A memory document's `parent` relationship is its session, so ACP
//! grants the agent the same boundary it has on the session's other children.

use std::sync::Arc;

use base64::Engine as _;
use chrono::Utc;
use defra_node::Node as EmbeddedNode;
use gents_loop::tool::ToolDefinition;
use gents_loop::tool::ToolDyn;
use gents_loop::tool::{Tool, ToolCallEnvelope};
use gents_memory_embed::{EmbedError, StaticEmbedder, DIMENSIONS, MODEL_ID};
use gents_protocol::message::Message;
use gents_protocol::tool_names::MEMORY_TOOL_NAME;
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use gents_store::local::document_acp::node_policy_ids;
use rapidhash::RapidHashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const MEMORY_COLLECTION: &str = "AgentMemory";

const DEFAULT_TOP_K: usize = 8;
const MAX_TOP_K: usize = 50;
const DEFAULT_INGEST_LIMIT: usize = 100;
const MAX_INGEST_LIMIT: usize = 500;
const MAX_TEXT_CHARS: usize = 16_000;
const MAX_EXTRACTED_CHARS: usize = 32_000;
const CHUNK_CHARS: usize = 2_000;
const MAX_INJECT_CHARS: usize = 4_000;
const RRF_K: f64 = 60.0;

/// DefraDB SDL for the memory collection. The household policy placeholder is
/// rendered with the home's policy ids before installation.
pub const MEMORY_SDL: &str = r#"type AgentMemory
  @policy(id: "$household", resource: "children")
  @index(fields: ["agent_did", "content_hash"], unique: true)
  @index(fields: ["agent_did", "created_at"]) {
    memory_key: String @index(unique: true)
    node_did: String @index @immutable
    agent_did: String @index @immutable
    session_id: String @index @immutable
    session_doc_id: String @index @immutable
    request_doc_id: String
    requester_did: String
    creator_did: String @immutable
    source: String @index
    kind: String @index
    text: String @fulltext
    embedding: [Float32!] @index(vector: {dimensions: 256, diskann: {R: 64, lBuild: 100, lSearch: 100, alphaPercent: 120, sampleBytes: 134217728, residentCodes: true}})
    content_hash: String @index
    model_id: String
    created_at: DateTime @index(direction: DESC)
    last_used_at: DateTime
    metadata: JSON
}"#;

const HELP: &str = "Persistent cross-session memory. Actions: help, remember, search, forget, list, ingest. remember stores one text with a kind and optional metadata. search ranks stored memory with DefraDB BM25 and the DiskANN vector index, fused by reciprocal rank. forget deletes by memory_key. list returns newest first. ingest pages canonical AgentMessage rows for a session and stores bounded chunks, idempotent by content hash; stored counts new memory only and next_cursor is null after the last page. Results state the ranking mode; semantic ranking is not claimed when the embedder is unavailable.";

#[derive(Debug)]
pub struct MemoryToolError(anyhow::Error);

impl std::fmt::Display for MemoryToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for MemoryToolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.root_cause())
    }
}

impl From<anyhow::Error> for MemoryToolError {
    fn from(error: anyhow::Error) -> Self {
        Self(error)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryParams {
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub topic: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub texts: Option<Vec<String>>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub metadata: Option<Value>,
    #[serde(default)]
    pub memory_key: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub cursor: Option<String>,
}

struct MemoryStore<'a> {
    node: &'a Arc<EmbeddedNode>,
    node_did: &'a str,
    actor: identity::Did,
    agent_did: &'a str,
    access: &'a ConfigAccess,
    embedder: &'a StaticEmbedder,
}

#[derive(Debug, Clone, Serialize)]
struct SearchHit {
    doc_id: String,
    memory_key: String,
    text: String,
    kind: String,
    source: String,
    created_at: String,
    score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    bm25_rank: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dense_rank: Option<usize>,
}

struct SearchOutcome {
    ranking: &'static str,
    hits: Vec<SearchHit>,
}

#[derive(Debug, Clone)]
struct RawHit {
    doc_id: String,
    memory_key: String,
    text: String,
    kind: String,
    source: String,
    created_at: String,
}

impl<'a> MemoryStore<'a> {
    fn install_access(&self) -> anyhow::Result<ConfigAccess> {
        let did = identity::Did::new(self.node_did)?;
        Ok(ConfigAccess::local_as(self.node.clone(), did))
    }

    async fn ensure_collection(&self) -> anyhow::Result<()> {
        let install = self.install_access()?;
        if install
            .collection_version(MEMORY_COLLECTION)
            .await?
            .is_none()
        {
            let sdl = node_policy_ids(self.node.as_ref()).render(MEMORY_SDL);
            install.add_collection(&sdl).await?;
        }
        Ok(())
    }

    // vertexia: memory queries are escaped GraphQL strings, following
    // session_history; move to gents_protocol::document_query once this
    // collection has typed fields.
    async fn read(&self, query: &str) -> anyhow::Result<Value> {
        let query = query.to_owned();
        ConfigAccess::transact_local_readonly(
            self.node,
            Some(self.actor.clone()),
            "tool.memory.read",
            move |txn| {
                let query = query.clone();
                Box::pin(async move { txn.execute(&query).await })
            },
        )
        .await
        .map_err(anyhow::Error::from)
    }

    async fn write(&self, mutation: &str) -> anyhow::Result<Value> {
        self.access
            .write("tool.memory.write", mutation)
            .await
            .map_err(anyhow::Error::from)
    }

    #[allow(clippy::too_many_arguments)]
    async fn remember(
        &self,
        text: &str,
        kind: &str,
        source: &str,
        session_id: &str,
        session_doc_id: &str,
        request_doc_id: Option<&str>,
        requester_did: Option<&str>,
        metadata: Option<&Value>,
    ) -> anyhow::Result<(SearchHit, bool)> {
        let text = text.trim();
        if text.is_empty() {
            anyhow::bail!("memory text must not be empty");
        }
        if text.chars().count() > MAX_TEXT_CHARS {
            anyhow::bail!("memory text exceeds {MAX_TEXT_CHARS} characters");
        }
        self.ensure_collection().await?;
        let content_hash = content_hash(self.agent_did, text);
        if let Some(hit) = self.existing_memory(&content_hash).await? {
            return Ok((hit, false));
        }
        let vector = self
            .embedder
            .embed_one(text.to_owned())
            .await
            .map_err(|error| anyhow::anyhow!("embedding failed: {error}"))?;
        if vector.len() != DIMENSIONS {
            anyhow::bail!(
                "embedding width {} does not match the collection's {DIMENSIONS}",
                vector.len()
            );
        }
        let memory_key = format!("{}:{}", self.agent_did, content_hash);
        let metadata = metadata
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_owned());
        let request_doc_id = request_doc_id
            .map(|value| format!("\"{}\"", escape_graphql_string(value)))
            .unwrap_or_else(|| "null".to_owned());
        let requester_did = requester_did
            .map(|value| format!("\"{}\"", escape_graphql_string(value)))
            .unwrap_or_else(|| "null".to_owned());
        let vector = vector
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let created_at = Utc::now().to_rfc3339();
        let mutation = format!(
            "mutation {{ add_AgentMemory(input: {{ memory_key: \"{memory_key}\", node_did: \"{node_did}\", agent_did: \"{agent_did}\", session_id: \"{session_id}\", session_doc_id: \"{session_doc_id}\", request_doc_id: {request_doc_id}, requester_did: {requester_did}, creator_did: \"{agent_did}\", source: \"{source}\", kind: \"{kind}\", text: \"{text}\", embedding: [{vector}], content_hash: \"{content_hash}\", model_id: \"{model_id}\", created_at: \"{created_at}\", metadata: {metadata} }}) {{ _docID }} }}",
            memory_key = escape_graphql_string(&memory_key),
            node_did = escape_graphql_string(self.node_did),
            agent_did = escape_graphql_string(self.agent_did),
            session_id = escape_graphql_string(session_id),
            session_doc_id = escape_graphql_string(session_doc_id),
            source = escape_graphql_string(source),
            kind = escape_graphql_string(kind),
            text = escape_graphql_string(text),
            content_hash = escape_graphql_string(&content_hash),
            model_id = escape_graphql_string(MODEL_ID),
        );
        let doc_id = match self.create_and_link(&mutation, session_doc_id).await {
            Ok(doc_id) => doc_id,
            Err(error) => {
                if let Some(hit) = self.existing_memory(&content_hash).await? {
                    return Ok((hit, false));
                }
                return Err(error);
            }
        };
        Ok((
            SearchHit {
                doc_id,
                memory_key,
                text: text.to_owned(),
                kind: kind.to_owned(),
                source: source.to_owned(),
                created_at,
                score: 1.0,
                bm25_rank: None,
                dense_rank: None,
            },
            true,
        ))
    }

    async fn existing_memory(&self, content_hash: &str) -> anyhow::Result<Option<SearchHit>> {
        let existing = self
            .read(&format!(
                "{{AgentMemory(filter: {{_and: [{{agent_did: {{_eq: \"{}\"}}}}, {{content_hash: {{_eq: \"{}\"}}}}]}}, limit: 1) {{ _docID memory_key text kind source created_at }}}}",
                escape_graphql_string(self.agent_did),
                escape_graphql_string(content_hash),
            ))
            .await?;
        Ok(existing
            .pointer("/data/AgentMemory/0")
            .and_then(parse_row)
            .map(|hit| SearchHit {
                doc_id: hit.doc_id,
                memory_key: hit.memory_key,
                text: hit.text,
                kind: hit.kind,
                source: hit.source,
                created_at: hit.created_at,
                score: 1.0,
                bm25_rank: None,
                dense_rank: None,
            }))
    }

    async fn create_and_link(
        &self,
        mutation: &str,
        session_doc_id: &str,
    ) -> anyhow::Result<String> {
        let mutation = mutation.to_owned();
        let collection = MEMORY_COLLECTION.to_owned();
        let parent = session_doc_id.to_owned();
        self.access
            .transact_write("tool.memory.remember", move |txn| {
                let mutation = mutation.clone();
                let collection = collection.clone();
                let parent = parent.clone();
                Box::pin(async move {
                    let created = txn.execute(&mutation).await?;
                    let doc_id = created
                        .pointer("/data/add_AgentMemory/0/_docID")
                        .and_then(Value::as_str)
                        .ok_or_else(|| anyhow::anyhow!("memory write returned no document id"))?
                        .to_owned();
                    txn.add_resource_relationship(
                        &collection,
                        &doc_id,
                        "parent",
                        "sessions",
                        &parent,
                    )
                    .await?;
                    Ok::<String, anyhow::Error>(doc_id)
                })
            })
            .await
    }

    async fn search(&self, query: &str, limit: usize) -> anyhow::Result<SearchOutcome> {
        let query = query.trim();
        if query.is_empty() {
            anyhow::bail!("memory search query must not be empty");
        }
        self.ensure_collection().await?;
        let limit = limit.clamp(1, MAX_TOP_K);
        let filter = format!(
            "{{agent_did: {{_eq: \"{}\"}}}}",
            escape_graphql_string(self.agent_did)
        );
        let bm25_query = format!(
            "{{AgentMemory(filter: {filter}, limit: {limit}, order: [{{_alias: {{_score: DESC}}}}]) {{ _docID memory_key text kind source created_at _score: BM25(query: \"{}\", fields: [\"text\"]) }}}}",
            escape_graphql_string(query),
        );
        let vector = match self.embedder.embed_one(query.to_owned()).await {
            Ok(vector) => Some(vector),
            Err(error) => {
                tracing::warn!(%error, "memory query embedding failed; using BM25 only");
                None
            }
        };
        let (bm25, dense) = match vector {
            Some(vector) => {
                let literal = vector
                    .iter()
                    .map(|value| value.to_string())
                    .collect::<Vec<_>>()
                    .join(",");
                let dense_query = format!(
                    "{{AgentMemory(filter: {filter}, limit: {limit}, order: [{{_alias: {{_score: DESC}}}}]) {{ _docID memory_key text kind source created_at _score: SIMILARITY(embedding: {{vector: [{literal}]}}) }}}}",
                );
                let (bm25, dense) = tokio::join!(self.read(&bm25_query), self.read(&dense_query));
                (bm25, dense)
            }
            None => (
                self.read(&bm25_query).await,
                Err(anyhow::anyhow!("no embedder")),
            ),
        };
        let bm25_rows = match bm25 {
            Ok(value) => parse_rows(&value),
            Err(error) => {
                tracing::warn!(%error, "memory BM25 search failed");
                Vec::new()
            }
        };
        let dense_rows = match dense {
            Ok(value) => parse_rows(&value),
            Err(error) => {
                tracing::warn!(%error, "memory dense search failed");
                Vec::new()
            }
        };
        if bm25_rows.is_empty() && dense_rows.is_empty() {
            return Ok(SearchOutcome {
                ranking: "none",
                hits: Vec::new(),
            });
        }
        let ranking = match (bm25_rows.is_empty(), dense_rows.is_empty()) {
            (false, false) => "hybrid",
            (false, true) => "bm25",
            (true, false) => "dense",
            (true, true) => "none",
        };
        Ok(SearchOutcome {
            ranking,
            hits: fuse(bm25_rows, dense_rows, limit),
        })
    }

    async fn list(&self, limit: usize) -> anyhow::Result<Vec<SearchHit>> {
        self.ensure_collection().await?;
        let limit = limit.clamp(1, MAX_TOP_K);
        let value = self
            .read(&format!(
                "{{AgentMemory(filter: {{agent_did: {{_eq: \"{}\"}}}}, order: [{{created_at: DESC}}], limit: {limit}) {{ _docID memory_key text kind source created_at }}}}",
                escape_graphql_string(self.agent_did),
            ))
            .await?;
        Ok(parse_rows(&value)
            .into_iter()
            .map(|row| SearchHit {
                doc_id: row.doc_id,
                memory_key: row.memory_key,
                text: row.text,
                kind: row.kind,
                source: row.source,
                created_at: row.created_at,
                score: 0.0,
                bm25_rank: None,
                dense_rank: None,
            })
            .collect())
    }

    async fn forget(&self, memory_key: &str) -> anyhow::Result<usize> {
        self.ensure_collection().await?;
        let value = self
            .write(&format!(
                "mutation {{ delete_AgentMemory(filter: {{_and: [{{agent_did: {{_eq: \"{}\"}}}}, {{memory_key: {{_eq: \"{}\"}}}}]}}) {{ _docID }} }}",
                escape_graphql_string(self.agent_did),
                escape_graphql_string(memory_key),
            ))
            .await?;
        Ok(value
            .pointer("/data/delete_AgentMemory")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0))
    }

    async fn ingest(
        &self,
        session_id: &str,
        session_doc_id: &str,
        limit: usize,
        offset: usize,
    ) -> anyhow::Result<(usize, Option<usize>)> {
        self.ensure_collection().await?;
        let limit = limit.clamp(1, MAX_INGEST_LIMIT);
        let value = self
            .read(&format!(
                "{{AgentMessage(filter: {{session_id: {{_eq: \"{}\"}}, node_did: {{_eq: \"{}\"}}}}, order: [{{sequence: ASC}}], limit: {limit}, offset: {offset}) {{ message_key sequence blocks }}}}",
                escape_graphql_string(session_id),
                escape_graphql_string(self.node_did),
            ))
            .await?;
        let rows = value
            .pointer("/data/AgentMessage")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut stored = 0;
        for row in &rows {
            let mut text = String::new();
            if let Some(blocks) = row.get("blocks") {
                collect_text(blocks, &mut text);
            }
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            for chunk in chunks(text, CHUNK_CHARS) {
                let (_, created) = self
                    .remember(
                        &chunk,
                        "message",
                        "session",
                        session_id,
                        session_doc_id,
                        None,
                        None,
                        None,
                    )
                    .await?;
                if created {
                    stored += 1;
                }
            }
        }
        let next = (rows.len() == limit).then_some(offset + rows.len());
        Ok((stored, next))
    }
}

fn content_hash(agent_did: &str, text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(agent_did.as_bytes());
    hasher.update(b"\n");
    hasher.update(text.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize())
}

fn parse_rows(value: &Value) -> Vec<RawHit> {
    value
        .pointer("/data/AgentMemory")
        .and_then(Value::as_array)
        .map(|rows| rows.iter().filter_map(parse_row).collect())
        .unwrap_or_default()
}

fn parse_row(row: &Value) -> Option<RawHit> {
    Some(RawHit {
        doc_id: row.get("_docID")?.as_str()?.to_owned(),
        memory_key: row
            .get("memory_key")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        text: row
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        kind: row
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        source: row
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        created_at: row
            .get("created_at")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    })
}

fn fuse(bm25: Vec<RawHit>, dense: Vec<RawHit>, limit: usize) -> Vec<SearchHit> {
    let mut fused: RapidHashMap<String, SearchHit> = RapidHashMap::default();
    for (rank, row) in bm25.into_iter().enumerate() {
        let contribution = 1.0 / (RRF_K + (rank + 1) as f64);
        let hit = fused
            .entry(row.doc_id.clone())
            .or_insert_with(|| SearchHit {
                doc_id: row.doc_id.clone(),
                memory_key: row.memory_key.clone(),
                text: row.text.clone(),
                kind: row.kind.clone(),
                source: row.source.clone(),
                created_at: row.created_at.clone(),
                score: 0.0,
                bm25_rank: None,
                dense_rank: None,
            });
        hit.score += contribution;
        hit.bm25_rank = Some(rank + 1);
    }
    for (rank, row) in dense.into_iter().enumerate() {
        let contribution = 1.0 / (RRF_K + (rank + 1) as f64);
        let hit = fused
            .entry(row.doc_id.clone())
            .or_insert_with(|| SearchHit {
                doc_id: row.doc_id.clone(),
                memory_key: row.memory_key.clone(),
                text: row.text.clone(),
                kind: row.kind.clone(),
                source: row.source.clone(),
                created_at: row.created_at.clone(),
                score: 0.0,
                bm25_rank: None,
                dense_rank: None,
            });
        hit.score += contribution;
        hit.dense_rank = Some(rank + 1);
    }
    let mut hits: Vec<_> = fused.into_values().collect();
    hits.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.doc_id.cmp(&right.doc_id))
    });
    hits.truncate(limit);
    hits
}

// vertexia: session ingest extracts text from AgentMessage.blocks directly;
// switch to the canonical transcript renderer when it exposes a bounded
// plain-text projection.
fn collect_text(value: &Value, out: &mut String) {
    if out.len() >= MAX_EXTRACTED_CHARS {
        return;
    }
    match value {
        Value::String(text) => {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(text);
        }
        Value::Array(items) => {
            for item in items {
                collect_text(item, out);
            }
        }
        Value::Object(map) => {
            if let Some(Value::String(text)) = map.get("text") {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            } else {
                for value in map.values() {
                    collect_text(value, out);
                }
            }
        }
        _ => {}
    }
}

fn chunks(text: &str, max: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if !current.is_empty() && current.len() + word.len() + 1 > max {
            chunks.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn format_section(outcome: &SearchOutcome) -> String {
    let mut section = format!(
        "<memory>\nStored memory relevant to this turn ({} ranking):\n",
        outcome.ranking
    );
    for hit in &outcome.hits {
        let line = format!("- [{}] {}\n", hit.kind, hit.text.replace('\n', " "));
        if section.len() + line.len() > MAX_INJECT_CHARS {
            break;
        }
        section.push_str(&line);
    }
    section.push_str("</memory>");
    section
}

/// Retrieve a bounded memory section for the current prompt, or `None` when
/// there is no memory or retrieval failed. A failure never fails the request.
pub async fn inject(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    agent_did: &str,
    access: &ConfigAccess,
    prompt: &str,
) -> Option<String> {
    if prompt.trim().is_empty() {
        return None;
    }
    let embedder = match embedder().await {
        Ok(embedder) => embedder,
        Err(error) => {
            tracing::warn!(%error, "memory embedder unavailable; continuing without memory");
            return None;
        }
    };
    let actor = match identity::Did::new(agent_did) {
        Ok(actor) => actor,
        Err(error) => {
            tracing::warn!(%error, "memory agent identity invalid; continuing without memory");
            return None;
        }
    };
    let store = MemoryStore {
        node,
        node_did,
        actor,
        agent_did,
        access,
        embedder: &embedder,
    };
    match store.search(prompt, DEFAULT_TOP_K).await {
        Ok(outcome) if !outcome.hits.is_empty() => Some(format_section(&outcome)),
        Ok(_) => None,
        Err(error) => {
            tracing::warn!(%error, "memory retrieval failed; continuing without memory");
            None
        }
    }
}

pub fn merge_context(existing: Option<Message>, section: String) -> Message {
    match existing {
        Some(Message::User { mut content }) => {
            content.push(gents_protocol::message::UserContent::text(section));
            Message::User { content }
        }
        Some(Message::System { content }) => Message::System {
            content: format!("{content}\n{section}"),
        },
        Some(_) => Message::user(section),
        None => Message::user(section),
    }
}

async fn embedder() -> anyhow::Result<Arc<StaticEmbedder>> {
    static EMBEDDER: tokio::sync::OnceCell<Arc<StaticEmbedder>> =
        tokio::sync::OnceCell::const_new();
    EMBEDDER
        .get_or_try_init(|| async {
            StaticEmbedder::load()
                .await
                .map(Arc::new)
                .map_err(|error: EmbedError| anyhow::anyhow!(error))
        })
        .await
        .map(Arc::clone)
}

pub struct MemoryTool {
    node: Arc<EmbeddedNode>,
    node_did: String,
    agent_did: String,
    access: ConfigAccess,
    embedder: Arc<StaticEmbedder>,
}

impl MemoryTool {
    fn store(&self) -> anyhow::Result<MemoryStore<'_>> {
        Ok(MemoryStore {
            node: &self.node,
            node_did: &self.node_did,
            actor: identity::Did::new(&self.agent_did)?,
            agent_did: &self.agent_did,
            access: &self.access,
            embedder: &self.embedder,
        })
    }

    async fn session_scope(
        &self,
        runtime: Option<&gents_loop::tool_call_lifecycle::runtime::CurrentToolRuntimeContext>,
        requested_session: Option<&str>,
    ) -> anyhow::Result<(String, String)> {
        let store = self.store()?;
        let session_id = requested_session
            .map(str::to_owned)
            .or_else(|| runtime.and_then(|runtime| runtime.session_id.clone()))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "memory writes need a session; call from a session request or pass session_id"
                )
            })?;
        let value = store
            .read(&format!(
                "{{AgentSession(filter: {{session_id: {{_eq: \"{}\"}}}}, limit: 1) {{ _docID session_id }}}}",
                escape_graphql_string(&session_id),
            ))
            .await?;
        if let Some(doc_id) = value
            .pointer("/data/AgentSession/0/_docID")
            .and_then(Value::as_str)
        {
            return Ok((session_id, doc_id.to_owned()));
        }
        if let Some(request_id) = runtime.and_then(|runtime| runtime.request_id.as_deref()) {
            let value = store
                .read(&format!(
                    "{{AgentRequest(filter: {{request_id: {{_eq: \"{}\"}}}}, limit: 1) {{ session_doc_id session_id }}}}",
                    escape_graphql_string(request_id),
                ))
                .await?;
            if let Some(row) = value.pointer("/data/AgentRequest/0") {
                if let Some(doc_id) = row.get("session_doc_id").and_then(Value::as_str) {
                    let session = row
                        .get("session_id")
                        .and_then(Value::as_str)
                        .unwrap_or(&session_id)
                        .to_owned();
                    return Ok((session, doc_id.to_owned()));
                }
            }
        }
        anyhow::bail!("session {session_id} has no readable document")
    }
}

impl Tool for MemoryTool {
    const NAME: &'static str = MEMORY_TOOL_NAME;

    type Error = MemoryToolError;
    type Args = MemoryParams;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Store and retrieve persistent cross-session memory. Search ranks with DefraDB BM25 and the DiskANN vector index. Use help for action syntax.".into(),
            parameters: json!({
                "type": "object", "additionalProperties": false,
                "properties": {
                    "action": {"type": "string", "enum": ["help", "remember", "search", "forget", "list", "ingest"], "description": "Defaults to search when query is set, otherwise list."},
                    "topic": {"type": "string", "description": "Help topic when action is help."},
                    "text": {"type": "string", "minLength": 1, "description": "remember: the memory to store."},
                    "texts": {"type": "array", "items": {"type": "string"}, "description": "remember: several memories to store."},
                    "query": {"type": "string", "minLength": 1, "description": "search: what to retrieve."},
                    "kind": {"type": "string", "description": "A label such as fact, rule, preference, message."},
                    "source": {"type": "string", "description": "Where the memory came from."},
                    "metadata": {"type": "object", "description": "Optional structured metadata."},
                    "memory_key": {"type": "string", "minLength": 1, "description": "forget: the memory_key from a search or list result."},
                    "session_id": {"type": "string", "minLength": 1, "description": "ingest: session to read; defaults to the current session."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 500},
                    "cursor": {"type": "string", "description": "ingest: the next_cursor from the previous page; absent or null means the first page."}
                }
            }),
        }
    }

    async fn call(
        &self,
        args: Self::Args,
        context: &ToolCallEnvelope,
    ) -> Result<Self::Output, Self::Error> {
        let runtime = context.runtime();
        let runtime = runtime.as_ref();
        let action = args.action.as_deref().unwrap_or_else(|| {
            if args.query.is_some() {
                "search"
            } else {
                "list"
            }
        });
        let store = self.store()?;
        let value = match action {
            "help" => json!({"help": HELP}),
            "remember" => {
                let (session_id, session_doc_id) = self
                    .session_scope(runtime, args.session_id.as_deref())
                    .await?;
                let requester = runtime.and_then(|runtime| runtime.requester_did.as_deref());
                let request = runtime.and_then(|runtime| runtime.request_id.as_deref());
                let kind = args.kind.as_deref().unwrap_or("fact");
                let source = args.source.as_deref().unwrap_or("agent");
                let mut texts = args.texts.unwrap_or_default();
                if let Some(text) = args.text {
                    texts.push(text);
                }
                if texts.is_empty() {
                    return Err(anyhow::anyhow!("remember requires text or texts").into());
                }
                let mut stored = Vec::new();
                for text in texts {
                    let (hit, _) = store
                        .remember(
                            &text,
                            kind,
                            source,
                            &session_id,
                            &session_doc_id,
                            request,
                            requester,
                            args.metadata.as_ref(),
                        )
                        .await?;
                    stored.push(hit);
                }
                json!({"stored": stored})
            }
            "search" => {
                let query = args
                    .query
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("search requires query"))?;
                let outcome = store
                    .search(query, args.limit.unwrap_or(DEFAULT_TOP_K))
                    .await?;
                json!({"ranking": outcome.ranking, "hits": outcome.hits})
            }
            "forget" => {
                let key = args
                    .memory_key
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("forget requires memory_key"))?;
                json!({"deleted": store.forget(key).await?})
            }
            "list" => {
                json!({"hits": store.list(args.limit.unwrap_or(DEFAULT_TOP_K)).await?})
            }
            "ingest" => {
                let (session_id, session_doc_id) = self
                    .session_scope(runtime, args.session_id.as_deref())
                    .await?;
                let offset = args
                    .cursor
                    .as_deref()
                    .and_then(|cursor| cursor.parse::<usize>().ok())
                    .unwrap_or(0);
                let (stored, next) = store
                    .ingest(
                        &session_id,
                        &session_doc_id,
                        args.limit.unwrap_or(DEFAULT_INGEST_LIMIT),
                        offset,
                    )
                    .await?;
                json!({"stored": stored, "next_cursor": next})
            }
            other => {
                return Err(anyhow::anyhow!("unsupported memory action {other}; use help").into())
            }
        };
        Ok(serde_json::to_string(&value).map_err(anyhow::Error::from)?)
    }
}

pub async fn build_memory_tool(
    node: Arc<EmbeddedNode>,
    node_did: String,
    agent_did: String,
    agent_identity: &dyn gents_store::identity::NodeIdentity,
) -> anyhow::Result<Box<dyn ToolDyn>> {
    let actor = identity::Did::new(&agent_did)?;
    let access = ConfigAccess::local_managed_agent(node.clone(), actor, agent_identity)?;
    let embedder = embedder().await?;
    Ok(Box::new(MemoryTool {
        node,
        node_did,
        agent_did,
        access,
        embedder,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(doc_id: &str) -> RawHit {
        RawHit {
            doc_id: doc_id.to_owned(),
            memory_key: format!("key:{doc_id}"),
            text: format!("text {doc_id}"),
            kind: "fact".to_owned(),
            source: "test".to_owned(),
            created_at: "2026-01-01T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn rrf_ranks_a_hit_in_both_lists_first() {
        let hits = fuse(vec![raw("a"), raw("b")], vec![raw("b"), raw("c")], 3);
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].doc_id, "b");
        assert_eq!(hits[0].bm25_rank, Some(2));
        assert_eq!(hits[0].dense_rank, Some(1));
        assert!(hits[0].score > hits[1].score);
    }

    #[test]
    fn chunks_split_on_word_boundaries_under_the_limit() {
        let chunks = chunks("alpha beta gamma delta epsilon", 11);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|chunk| chunk.len() <= 11));
        assert_eq!(chunks.join(" "), "alpha beta gamma delta epsilon");
    }

    #[test]
    fn collect_text_reads_nested_text_fields() {
        let value = serde_json::json!([
            {"type": "text", "text": "a rule"},
            {"type": "tool_result", "content": {"text": "b output"}},
            {"type": "text", "text": "c note"}
        ]);
        let mut out = String::new();
        collect_text(&value, &mut out);
        assert!(out.contains("a rule"));
        assert!(out.contains("b output"));
        assert!(out.contains("c note"));
    }

    #[test]
    fn content_hash_depends_on_agent_and_text() {
        assert_eq!(
            content_hash("agent-a", "same"),
            content_hash("agent-a", "same")
        );
        assert_ne!(
            content_hash("agent-a", "same"),
            content_hash("agent-b", "same")
        );
        assert_ne!(
            content_hash("agent-a", "same"),
            content_hash("agent-a", "other")
        );
    }

    #[test]
    fn merge_context_appends_to_one_user_message() {
        let merged = merge_context(
            Some(Message::user("<context>base</context>")),
            "<memory>m</memory>".to_owned(),
        );
        let Message::User { content } = merged else {
            panic!("expected a user message");
        };
        assert_eq!(content.len(), 2);
    }

    #[test]
    fn format_section_states_the_ranking_and_bounds_the_text() {
        let outcome = SearchOutcome {
            ranking: "hybrid",
            hits: vec![SearchHit {
                doc_id: "doc".to_owned(),
                memory_key: "key".to_owned(),
                text: "x".repeat(MAX_INJECT_CHARS),
                kind: "rule".to_owned(),
                source: "test".to_owned(),
                created_at: "2026-01-01T00:00:00Z".to_owned(),
                score: 1.0,
                bm25_rank: Some(1),
                dense_rank: Some(1),
            }],
        };
        let section = format_section(&outcome);
        assert!(section.contains("hybrid ranking"));
        assert!(section.len() <= MAX_INJECT_CHARS + 128);
    }
}
