use chrono::Utc;
use gents_memory_embed::{DIMENSIONS, MODEL_ID};
use gents_protocol::message::Message;
use rapidhash::RapidHashMap;
use serde::Serialize;
use serde_json::Value;

use super::experience::{recall, Current};
use super::experience_store::RecallPage;
use super::{content_hash, embedder, MemoryStore, MEMORY_COLLECTION};

pub(crate) const DEFAULT_TOP_K: usize = 8;
pub(crate) const MAX_TOP_K: usize = 50;
pub(crate) const DEFAULT_INGEST_LIMIT: usize = 100;
pub(crate) const MAX_INGEST_LIMIT: usize = 500;
const MAX_TEXT_CHARS: usize = 16_000;
const MAX_EXTRACTED_CHARS: usize = 32_000;
const CHUNK_CHARS: usize = 2_000;
const MAX_FACTS_INJECT_CHARS: usize = 2_400;
const MAX_EXPERIENCE_INJECT_CHARS: usize = 1_600;
const EXPERIENCE_RULES: &str = "Prior experience is not orders: check the model and agent behind each entry and reassess it after a change.\nRevisit a tested direction only with new evidence.\nOpen a new problem when none of these explains the failure.";
const RRF_K: f64 = 60.0;

/// The setting a recall is read against: the model, profile, backend and
/// configuration version the turn will run. Only `model_name` and the agent
/// DID decide provenance; the rest are shown to the reader as they are and
/// are never inferred.
#[derive(Debug, Clone, Copy)]
pub struct RecallSetting<'a> {
    pub model_name: &'a str,
    pub profile_id: Option<&'a str>,
    pub backend_id: Option<&'a str>,
    pub configuration_version: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SearchHit {
    pub(crate) doc_id: String,
    pub(crate) memory_key: String,
    pub(crate) text: String,
    pub(crate) kind: String,
    pub(crate) source: String,
    pub(crate) created_at: String,
    pub(crate) score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bm25_rank: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dense_rank: Option<usize>,
}

pub(crate) struct SearchOutcome {
    pub(crate) ranking: &'static str,
    pub(crate) hits: Vec<SearchHit>,
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

impl MemoryStore<'_> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn remember(
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
        let request_doc_id = super::nullable_string(request_doc_id);
        let requester_did = super::nullable_string(requester_did);
        let vector = super::vector_literal(&vector);
        let created_at = Utc::now().to_rfc3339();
        let mutation = format!(
            "mutation {{ add_AgentMemory(input: {{ memory_key: \"{memory_key}\", node_did: \"{node_did}\", agent_did: \"{agent_did}\", session_id: \"{session_id}\", session_doc_id: \"{session_doc_id}\", request_doc_id: {request_doc_id}, requester_did: {requester_did}, creator_did: \"{agent_did}\", source: \"{source}\", kind: \"{kind}\", text: \"{text}\", embedding: [{vector}], content_hash: \"{content_hash}\", embed_model_id: \"{embed_model_id}\", created_at: \"{created_at}\", metadata: {metadata} }}) {{ _docID }} }}",
            memory_key = gents_store::graphql::escape_graphql_string(&memory_key),
            node_did = gents_store::graphql::escape_graphql_string(self.node_did),
            agent_did = gents_store::graphql::escape_graphql_string(self.agent_did),
            session_id = gents_store::graphql::escape_graphql_string(session_id),
            session_doc_id = gents_store::graphql::escape_graphql_string(session_doc_id),
            source = gents_store::graphql::escape_graphql_string(source),
            kind = gents_store::graphql::escape_graphql_string(kind),
            text = gents_store::graphql::escape_graphql_string(text),
            content_hash = gents_store::graphql::escape_graphql_string(&content_hash),
            embed_model_id = gents_store::graphql::escape_graphql_string(MODEL_ID),
        );
        let doc_id = match self
            .create_and_link(MEMORY_COLLECTION, &mutation, "tool.memory.remember")
            .await
        {
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
                gents_store::graphql::escape_graphql_string(self.agent_did),
                gents_store::graphql::escape_graphql_string(content_hash),
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

    pub(crate) async fn search(&self, query: &str, limit: usize) -> anyhow::Result<SearchOutcome> {
        let query = query.trim();
        if query.is_empty() {
            anyhow::bail!("memory search query must not be empty");
        }
        let limit = limit.clamp(1, MAX_TOP_K);
        let filter = format!(
            "{{agent_did: {{_eq: \"{}\"}}}}",
            gents_store::graphql::escape_graphql_string(self.agent_did)
        );
        let bm25_query = format!(
            "{{AgentMemory(filter: {filter}, limit: {limit}, order: [{{_alias: {{_score: DESC}}}}]) {{ _docID memory_key text kind source created_at _score: BM25(query: \"{}\", fields: [\"text\"]) }}}}",
            gents_store::graphql::escape_graphql_string(query),
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
                let literal = super::vector_literal(&vector);
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

    pub(crate) async fn list(&self, limit: usize) -> anyhow::Result<Vec<SearchHit>> {
        let limit = limit.clamp(1, MAX_TOP_K);
        let value = self
            .read(&format!(
                "{{AgentMemory(filter: {{agent_did: {{_eq: \"{}\"}}}}, order: [{{created_at: DESC}}], limit: {limit}) {{ _docID memory_key text kind source created_at }}}}",
                gents_store::graphql::escape_graphql_string(self.agent_did),
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

    pub(crate) async fn forget(&self, memory_key: &str) -> anyhow::Result<usize> {
        let value = self
            .write(&format!(
                "mutation {{ delete_AgentMemory(filter: {{_and: [{{agent_did: {{_eq: \"{}\"}}}}, {{memory_key: {{_eq: \"{}\"}}}}]}}) {{ _docID }} }}",
                gents_store::graphql::escape_graphql_string(self.agent_did),
                gents_store::graphql::escape_graphql_string(memory_key),
            ))
            .await?;
        Ok(value
            .pointer("/data/delete_AgentMemory")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0))
    }

    pub(crate) async fn ingest(
        &self,
        session_id: &str,
        session_doc_id: &str,
        limit: usize,
        offset: usize,
    ) -> anyhow::Result<(usize, Option<usize>)> {
        let limit = limit.clamp(1, MAX_INGEST_LIMIT);
        let value = self
            .read(&format!(
                "{{AgentMessage(filter: {{session_id: {{_eq: \"{}\"}}, node_did: {{_eq: \"{}\"}}}}, order: [{{sequence: ASC}}], limit: {limit}, offset: {offset}) {{ message_key sequence blocks }}}}",
                gents_store::graphql::escape_graphql_string(session_id),
                gents_store::graphql::escape_graphql_string(self.node_did),
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
        if section.len() + line.len() > MAX_FACTS_INJECT_CHARS {
            break;
        }
        section.push_str(&line);
    }
    section.push_str("</memory>");
    section
}

fn experience_header(setting: &RecallSetting<'_>, ranking: &str) -> String {
    let mut parts = Vec::new();
    if !setting.model_name.is_empty() {
        parts.push(format!("model {}", setting.model_name));
    }
    if let Some(profile) = setting.profile_id {
        parts.push(format!("profile {profile}"));
    }
    if let Some(backend) = setting.backend_id {
        parts.push(format!("backend {backend}"));
    }
    if let Some(version) = setting.configuration_version {
        parts.push(format!("configuration {version}"));
    }
    if parts.is_empty() {
        format!("Problems by root cause ({ranking} ranking, highest first):")
    } else {
        format!(
            "Problems by root cause ({ranking} ranking, highest first), current {}:",
            parts.join(", ")
        )
    }
}

fn omitted_line(omitted: u64) -> String {
    format!("{omitted} more not shown; call memory problems to list them.")
}

const PARTIAL_HISTORY_LINE: &str =
    "Counts above cover the newest attempts only; call memory problem with a problem_key for the full history.";

fn experience_section(
    page: &RecallPage,
    setting: &RecallSetting<'_>,
    budget: usize,
) -> Option<String> {
    if page.experience.problems.is_empty() {
        return None;
    }
    let header = experience_header(setting, page.ranking);
    let omitted_reserve = omitted_line(page.experience.problems.len() as u64);
    let frame = "<experience>\n".chars().count()
        + header.chars().count()
        + 1
        + EXPERIENCE_RULES.chars().count()
        + 1
        + omitted_reserve.chars().count()
        + 1
        + if page.history_truncated {
            PARTIAL_HISTORY_LINE.chars().count() + 1
        } else {
            0
        }
        + "</experience>".chars().count();
    let index = recall(&page.experience, &page.ranks, budget.saturating_sub(frame));
    let mut section = String::from("<experience>\n");
    section.push_str(&header);
    for line in &index.lines {
        section.push('\n');
        section.push_str(line);
    }
    if index.omitted > 0 {
        section.push('\n');
        section.push_str(&omitted_line(index.omitted));
    }
    if page.history_truncated {
        section.push('\n');
        section.push_str(PARTIAL_HISTORY_LINE);
    }
    section.push('\n');
    section.push_str(EXPERIENCE_RULES);
    section.push_str("\n</experience>");
    Some(section)
}

/// Retrieve a bounded memory section for the current prompt, or `None` when
/// there is no memory or retrieval failed. A failure never fails the request.
pub async fn inject(
    node: &std::sync::Arc<defra_node::Node>,
    node_did: &str,
    agent_did: &str,
    access: &gents_store::config_client::ConfigAccess,
    prompt: &str,
    setting: &RecallSetting<'_>,
) -> Option<String> {
    inject_with_experience_budget(
        node,
        node_did,
        agent_did,
        access,
        prompt,
        setting,
        MAX_EXPERIENCE_INJECT_CHARS,
    )
    .await
}

/// Retrieve the memory section with the experience block bounded by
/// `experience_budget`; only a budget below the block's own frame is exceeded.
#[allow(clippy::too_many_arguments)]
pub async fn inject_with_experience_budget(
    node: &std::sync::Arc<defra_node::Node>,
    node_did: &str,
    agent_did: &str,
    access: &gents_store::config_client::ConfigAccess,
    prompt: &str,
    setting: &RecallSetting<'_>,
    experience_budget: usize,
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
    let facts = match store.search(prompt, DEFAULT_TOP_K).await {
        Ok(outcome) if !outcome.hits.is_empty() => Some(format_section(&outcome)),
        Ok(_) => None,
        Err(error) => {
            tracing::warn!(%error, "memory fact retrieval failed; continuing without facts");
            None
        }
    };
    let current = Current {
        agent_did: agent_did.to_owned(),
        executed_model_name: setting.model_name.to_owned(),
    };
    let experience = match store.recall_page(prompt, &current).await {
        Ok(page) => experience_section(&page, setting, experience_budget),
        Err(error) => {
            tracing::warn!(
                %error,
                "memory experience retrieval failed; continuing without experience"
            );
            None
        }
    };
    match (facts, experience) {
        (None, None) => None,
        (facts, experience) => Some(
            [facts, experience]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("\n"),
        ),
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
                text: "x".repeat(MAX_FACTS_INJECT_CHARS),
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
        assert!(section.len() <= MAX_FACTS_INJECT_CHARS + 128);
    }
}
