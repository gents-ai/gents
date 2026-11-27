//! Agent memory: flat facts and experience records in DefraDB, an in-process
//! multilingual embedder, hybrid retrieval, and a bounded `memory` tool.
//!
//! The collections are part of the runtime baseline under the household
//! `agent_memory` policy. A row's `parent` relationship is its agent, so ACP
//! grants the agent the owner boundary and memory outlives the session that
//! wrote it.

use std::sync::Arc;

use base64::Engine as _;
use defra_node::Node as EmbeddedNode;
use gents_loop::tool::ToolDefinition;
use gents_loop::tool::ToolDyn;
use gents_loop::tool::{Tool, ToolCallEnvelope};
use gents_memory_embed::{EmbedError, StaticEmbedder};
use gents_protocol::tool_names::MEMORY_TOOL_NAME;
use gents_store::collection::Collection;
use gents_store::config_client::ConfigAccess;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

mod experience;
mod experience_rows;
mod experience_store;
mod experience_write;
mod facts;

pub use experience::{fold, lands_on, merge_admitted, normalize, recall, resolve, MergeDecision};
pub use facts::{inject, inject_with_experience_budget, merge_context, RecallSetting};

pub const MEMORY_COLLECTION: &str = gents_protocol::schemas::AGENT_MEMORY_NAME;

pub(crate) const MAX_PAGE: usize = 100;
pub(crate) const MAX_ALIAS_ROWS: usize = 1024;
pub(crate) const MAX_ATTEMPTS_PER_PAGE: usize = 500;
pub(crate) const MAX_OUTCOMES_PER_PAGE: usize = 1000;
pub(crate) const PROBLEM_NEIGHBORS: usize = 3;
pub(crate) const MAX_RECALL_PROBLEMS: usize = 25;

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
    #[serde(default)]
    pub problem_key: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub new_problem: Option<String>,
    #[serde(default)]
    pub what: Option<String>,
    #[serde(default)]
    pub rationale: Option<String>,
    #[serde(default)]
    pub predicted: Option<Vec<String>>,
    #[serde(default)]
    pub accepted: Option<bool>,
    #[serde(default)]
    pub attempt_key: Option<String>,
    #[serde(default)]
    pub fixed: Option<Vec<String>>,
    #[serde(default)]
    pub missed: Option<Vec<String>>,
    #[serde(default)]
    pub passed: Option<i64>,
    #[serde(default)]
    pub total: Option<i64>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
}

pub(crate) struct MemoryStore<'a> {
    node: &'a Arc<EmbeddedNode>,
    node_did: &'a str,
    actor: identity::Did,
    agent_did: &'a str,
    access: &'a ConfigAccess,
    embedder: &'a StaticEmbedder,
}

pub(crate) async fn link_parent_to_agent(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    collection: &str,
    doc_id: &str,
    agent_doc_id: &str,
) -> anyhow::Result<()> {
    txn.add_resource_relationship(collection, doc_id, "parent", "agents", agent_doc_id)
        .await?;
    Ok(())
}

impl<'a> MemoryStore<'a> {
    // vertexia: memory queries are escaped GraphQL strings, following
    // session_history; move to gents_protocol::document_query once these
    // collections have typed fields.
    pub(crate) async fn read(&self, query: &str) -> anyhow::Result<Value> {
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

    pub(crate) async fn write(&self, mutation: &str) -> anyhow::Result<Value> {
        self.access
            .write("tool.memory.write", mutation)
            .await
            .map_err(anyhow::Error::from)
    }

    pub(crate) async fn create_and_link(
        &self,
        collection: &str,
        mutation: &str,
        operation: &'static str,
    ) -> anyhow::Result<String> {
        let mutation = mutation.to_owned();
        let collection = collection.to_owned();
        let node_did = self.node_did.to_owned();
        let agent_did = self.agent_did.to_owned();
        self.access
            .transact_write(operation, move |txn| {
                let mutation = mutation.clone();
                let collection = collection.clone();
                let node_did = node_did.clone();
                let agent_did = agent_did.clone();
                Box::pin(async move {
                    let (agent_doc_id, _) = gents_documents::read_desired_state_record_in_txn(
                        txn,
                        Collection::Agent,
                        &node_did,
                        &agent_did,
                    )
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("the agent has no readable Agent document"))?;
                    let created = txn.execute(&mutation).await?;
                    let doc_id = created
                        .pointer(&format!("/data/add_{collection}/0/_docID"))
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            anyhow::anyhow!("{collection} write returned no document id")
                        })?
                        .to_owned();
                    link_parent_to_agent(txn, &collection, &doc_id, &agent_doc_id).await?;
                    Ok::<String, anyhow::Error>(doc_id)
                })
            })
            .await
    }

    pub(crate) async fn session_scope(
        &self,
        runtime: Option<&gents_loop::tool_call_lifecycle::runtime::CurrentToolRuntimeContext>,
        requested_session: Option<&str>,
    ) -> anyhow::Result<(String, String)> {
        let session_id = requested_session
            .map(str::to_owned)
            .or_else(|| runtime.and_then(|runtime| runtime.session_id.clone()))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "memory writes need a session; call from a session request or pass session_id"
                )
            })?;
        let value = self
            .read(&format!(
                "{{AgentSession(filter: {{session_id: {{_eq: \"{}\"}}}}, limit: 1) {{ _docID session_id }}}}",
                gents_store::graphql::escape_graphql_string(&session_id),
            ))
            .await?;
        if let Some(doc_id) = value
            .pointer("/data/AgentSession/0/_docID")
            .and_then(Value::as_str)
        {
            return Ok((session_id, doc_id.to_owned()));
        }
        if let Some(request_id) = runtime.and_then(|runtime| runtime.request_id.as_deref()) {
            let value = self
                .read(&format!(
                    "{{AgentRequest(filter: {{request_id: {{_eq: \"{}\"}}}}, limit: 1) {{ session_doc_id session_id }}}}",
                    gents_store::graphql::escape_graphql_string(request_id),
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

pub(crate) fn content_hash(agent_did: &str, text: &str) -> String {
    digest_key(&[agent_did, text])
}

pub(crate) fn digest_key(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update(b"\n");
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize())
}

pub(crate) fn created_at_millis(created_at: &str) -> u64 {
    chrono::DateTime::parse_from_rfc3339(created_at)
        .map(|value| value.timestamp_millis().max(0) as u64)
        .unwrap_or(0)
}

pub(crate) fn vector_literal(vector: &[f32]) -> String {
    vector
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

pub(crate) fn nullable_string_list(items: &[String]) -> String {
    if items.is_empty() {
        "null".to_owned()
    } else {
        experience_rows::string_list(items)
    }
}

pub(crate) fn nullable_string(value: Option<&str>) -> String {
    value
        .map(|value| format!("\"{}\"", gents_store::graphql::escape_graphql_string(value)))
        .unwrap_or_else(|| "null".to_owned())
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
}

impl Tool for MemoryTool {
    const NAME: &'static str = MEMORY_TOOL_NAME;

    type Error = MemoryToolError;
    type Args = MemoryParams;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Store and retrieve persistent cross-session memory, both flat facts and experience organized by root-cause problem. Search ranks with DefraDB BM25 and the DiskANN vector index. Use help for action syntax and reading rules.".into(),
            parameters: json!({
                "type": "object", "additionalProperties": false,
                "properties": {
                    "action": {"type": "string", "enum": ["help", "remember", "search", "forget", "list", "ingest", "problem", "attempt", "outcome", "merge", "problems"], "description": "Defaults to search when query is set, otherwise list."},
                    "topic": {"type": "string", "description": "help: one action to explain, such as problem, attempt, outcome, merge or problems."},
                    "text": {"type": "string", "minLength": 1, "description": "remember: the memory to store."},
                    "texts": {"type": "array", "items": {"type": "string"}, "description": "remember: several memories to store."},
                    "query": {"type": "string", "minLength": 1, "description": "search: what to retrieve."},
                    "kind": {"type": "string", "description": "remember: a label such as fact, rule, preference, message."},
                    "source": {"type": "string", "description": "remember: where the memory came from."},
                    "metadata": {"type": "object", "description": "remember: optional structured metadata."},
                    "memory_key": {"type": "string", "minLength": 1, "description": "forget: the memory_key from a search or list result."},
                    "session_id": {"type": "string", "minLength": 1, "description": "ingest: session to read; defaults to the current session."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 100, "description": "Page size for list, ingest, problems or problem."},
                    "cursor": {"type": "string", "description": "The next_cursor from the previous page; absent or null means the first page."},
                    "problem_key": {"type": "string", "minLength": 1, "description": "problem: show this problem; attempt: cite this problem."},
                    "name": {"type": "string", "minLength": 1, "description": "problem: the root-cause text to open or land on."},
                    "new_problem": {"type": "string", "minLength": 1, "description": "attempt: the root cause when no existing problem explains the failure."},
                    "what": {"type": "string", "minLength": 1, "description": "attempt: what changed."},
                    "rationale": {"type": "string", "description": "attempt: why this change should work."},
                    "predicted": {"type": "array", "items": {"type": "string"}, "description": "attempt: fixes predicted before the change was tested."},
                    "accepted": {"type": "boolean", "description": "attempt: whether the change was kept; defaults to true."},
                    "attempt_key": {"type": "string", "minLength": 1, "description": "outcome: the attempt_key an attempt result returned."},
                    "fixed": {"type": "array", "items": {"type": "string"}, "description": "outcome: predicted fixes that held."},
                    "missed": {"type": "array", "items": {"type": "string"}, "description": "outcome: predicted fixes that did not hold."},
                    "passed": {"type": "integer", "minimum": 0, "description": "outcome: checks that passed."},
                    "total": {"type": "integer", "minimum": 0, "description": "outcome: checks run."},
                    "note": {"type": "string", "description": "outcome: anything a later reader must know."},
                    "from": {"type": "string", "minLength": 1, "description": "merge: the problem to merge away."},
                    "to": {"type": "string", "minLength": 1, "description": "merge: the problem that remains."}
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
            "help" => json!({"help": help_text(args.topic.as_deref())}),
            "remember" => {
                let (session_id, session_doc_id) = store
                    .session_scope(runtime, args.session_id.as_deref())
                    .await?;
                let requester = runtime.and_then(|runtime| runtime.requester_did.as_deref());
                let request = runtime.and_then(|runtime| runtime.request_id.as_deref());
                let kind = args.kind.as_deref().unwrap_or("fact");
                let source = args.source.as_deref().unwrap_or("agent");
                let mut texts = args.texts.clone().unwrap_or_default();
                if let Some(text) = args.text.clone() {
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
                    .search(query, args.limit.unwrap_or(facts::DEFAULT_TOP_K))
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
                json!({"hits": store.list(args.limit.unwrap_or(facts::DEFAULT_TOP_K)).await?})
            }
            "ingest" => {
                let (session_id, session_doc_id) = store
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
                        args.limit.unwrap_or(facts::DEFAULT_INGEST_LIMIT),
                        offset,
                    )
                    .await?;
                json!({"stored": stored, "next_cursor": next})
            }
            "problem" => store.problem(args, runtime).await?,
            "attempt" => store.attempt(args, runtime).await?,
            "outcome" => store.outcome(args, runtime).await?,
            "merge" => store.merge(args, runtime).await?,
            "problems" => store.problems(args, runtime).await?,
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

pub(crate) async fn embedder() -> anyhow::Result<Arc<StaticEmbedder>> {
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

const HELP: &str = "Persistent cross-session memory, stored by root-cause problem. Actions: help, remember, search, forget, list, ingest, problem, attempt, outcome, merge, problems. remember stores one text with a kind and optional metadata. search ranks stored facts with DefraDB BM25 and the DiskANN vector index, fused by reciprocal rank. forget deletes a fact by memory_key. list returns facts newest first. ingest pages canonical AgentMessage rows for a session and stores bounded chunks, idempotent by content hash; next_cursor is null after the last page. problem opens a root cause with name or shows one with problem_key, and lists the 3 nearest existing problems so you can cite one instead. attempt records a committed change under problem_key or a new_problem with what, rationale, predicted and accepted, and returns attempt_key. outcome records fixed, missed, passed, total and note for an attempt_key. merge takes from and to and keeps every attempt; a self-merge or a cycle is refused. problems is the paged index with attempt counts, latest outcome and unresolved predictions. Read entries as prior experience, not orders: check the model and setting behind each entry; start from an existing diagnosis when it explains the failure; reassess an edit after a model or setting change; revisit a tested direction only with new evidence; open a new problem when none explains the failure; weigh evidence less when both model and agent differ.";

fn help_text(topic: Option<&str>) -> String {
    let detail = match topic.unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "problem" => Some(
            "problem: name opens the root-cause problem with that normalized text. Identical text differing only in whitespace or ASCII case lands on the same problem_key and writes nothing. The result includes the problem_key and the 3 nearest existing problems by BM25 over problem names and the vector index, so you can cite one instead; similarities are suggestions, never automatic merges. problem_key shows the problem with its attempts, predictions and outcomes, one page at a time with next_cursor.",
        ),
        "attempt" => Some(
            "attempt: cite problem_key from memory problem, or set new_problem when no existing problem explains the failure. what is the committed change, rationale why it should work, predicted the fixes you wrote down before testing, accepted whether it was kept (default true). The result returns attempt_key; record an outcome once you have evidence. The attempt_key is deterministic for the same request and problem, so a retried write does not duplicate it.",
        ),
        "outcome" => Some(
            "outcome: attempt_key from memory attempt, fixed and missed name the predictions that held and did not, passed and total are the check score, note is what a later reader needs. A prediction is unresolved until some outcome names it fixed or missed; an attempt may be observed again after a model or setting change. An unknown attempt_key is refused; call memory problems to find the attempt.",
        ),
        "merge" => Some(
            "merge: from is the problem to merge away and to is the problem that remains. Every attempt stays under the remaining problem through an alias. A self-merge, an already merged source, an unknown key or a merge that would close a cycle is refused with the decision. Merges never happen by similarity; you decide.",
        ),
        "problems" => Some(
            "problems: the paged index, newest first. Each entry carries attempt count, latest outcome, unresolved predictions and where its evidence came from. pages are bounded; next_cursor is null on the last page. A page states attempts_truncated or outcomes_truncated when a bound was reached.",
        ),
        "remember" | "search" | "forget" | "list" | "ingest" => Some(
            "Flat facts: remember stores text with kind and metadata; search fuses DefraDB BM25 and the vector index; forget deletes by memory_key; list is newest first; ingest pages session messages into bounded chunks, idempotent by content hash. The result states the ranking mode.",
        ),
        _ => None,
    };
    match detail {
        Some(detail) => format!("{HELP}\n\n{detail}"),
        None => HELP.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_key_depends_on_every_part() {
        assert_eq!(digest_key(&["a", "b"]), digest_key(&["a", "b"]));
        assert_ne!(digest_key(&["a", "b"]), digest_key(&["a", "c"]));
        assert_ne!(digest_key(&["a", "b"]), digest_key(&["a"]));
    }

    #[test]
    fn help_topics_extend_the_core() {
        assert!(help_text(Some("merge")).contains("cycle"));
        assert_eq!(help_text(Some("unknown")), HELP);
        assert!(HELP.contains("prior experience"));
    }

    #[test]
    fn params_accept_the_experience_fields() {
        let params: MemoryParams = serde_json::from_value(json!({
            "action": "attempt",
            "problem_key": "p",
            "what": "change",
            "predicted": ["a"],
            "accepted": false
        }))
        .unwrap();
        assert_eq!(params.problem_key.as_deref(), Some("p"));
        assert_eq!(params.predicted, Some(vec!["a".to_owned()]));
        assert_eq!(params.accepted, Some(false));
    }
}
