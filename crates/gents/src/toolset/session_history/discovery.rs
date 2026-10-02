use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{clamp_limit, SessionHistoryParams, SessionHistoryTool};
use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::graphql::escape_graphql_string;
use crate::session::TxnCanonicalReader;

const FIELDS: &str = crate::session::AGENT_SESSION_FIELDS;
const HELP: &str = "Read canonical sessions visible to your authenticated DID through DefraDB ACP. Actions: list, count, search, get, transcript. Use {\"action\":\"help\",\"topic\":\"search\"} for an action's syntax. Copied IDs grant no access; ACP determines visibility across agents and requesters.";

fn action_help(topic: Option<&str>) -> Result<&'static str> {
    Ok(match topic {
        None => HELP,
        Some("list") => {
            r#"{"action":"list","filter":{"status":"closed","tag":"release"},"limit":10}. Filters: behavior_id, status (open/closed), tag, created_after (inclusive), created_before (exclusive), text (title or ID substring). Order: created_at descending, then ID descending. Pass next_cursor as cursor with the same filter. Limit: 1–100."#
        }
        Some("count") => {
            r#"{"action":"count","filter":{"status":"closed"}}. Exact count of authorized sessions, independent of request volume or list limits. Filters are the same as list; no cursor. Use help topic=list for filters."#
        }
        Some("search") => {
            r#"{"action":"search","query":"rollback decision","limit":10}. Query is required: literal case-insensitive substring of titles, IDs and reconstructed transcript text, not semantic or indexed retrieval. filter.text restricts title/ID, not transcript. Hits include physical message IDs and payload references. Use a shorter substring if no hits. Pass next_cursor as cursor with the same query and filter. Limit: 1–100. Use help topic=list for filters."#
        }
        Some("get") => {
            r#"{"action":"get","session_id":"<ID>","details":true}. Session ID is required; discover it with list or search. details=true adds timeline, tool/token/context accounting and compaction observations."#
        }
        Some("transcript") => {
            r#"{"action":"transcript","session_id":"<ID>","limit":6}. Session ID is required; discover it with list or search. Returns canonical text with physical message IDs and payload references, up to six messages and 8000 characters per chunk. Pass next_cursor as cursor with the same session_id to continue."#
        }
        Some(_) => bail!("unknown help topic; next call: sessions {{\"action\":\"help\"}}"),
    })
}

fn missing_search_query(args: &SessionHistoryParams) -> anyhow::Error {
    let mut next = json!({"action":"search","query":"<text to find>"});
    if let Some(filter) = &args.filter {
        let mut filter = serde_json::to_value(filter).expect("session filter serializes");
        filter
            .as_object_mut()
            .unwrap()
            .retain(|_, value| !value.is_null());
        if let Some(text) = filter.as_object_mut().unwrap().remove("text") {
            if text.as_str().is_some_and(|text| !text.trim().is_empty()) {
                next["query"] = text;
            }
        }
        if !filter.as_object().unwrap().is_empty() {
            next["filter"] = filter;
        }
    }
    if let Some(limit) = args.limit {
        next["limit"] = json!(limit);
    }
    anyhow::anyhow!("search requires query; filter.text matches only titles and IDs; next call: sessions {next}")
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionFilter {
    pub behavior_id: Option<String>,
    pub status: Option<String>,
    pub tag: Option<String>,
    pub created_after: Option<String>,
    pub created_before: Option<String>,
    pub text: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    created_at: String,
    session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hit_sequence: Option<i64>,
}

fn quoted(value: &str) -> String {
    format!("\"{}\"", escape_graphql_string(value))
}

fn scope(agent: &str, requester: Option<&str>) -> String {
    format!(
        "agent_did: {{_eq: {}}}, requester_did: {{_eq: {}}}",
        quoted(agent),
        requester.map(quoted).unwrap_or_else(|| "null".into())
    )
}

fn filter_query(filter: &SessionFilter, cursor: Option<&Cursor>) -> Result<String> {
    let mut parts = Vec::new();
    if let Some(behavior) = &filter.behavior_id {
        parts.push(format!("{{ behavior_id: {{_eq: {}}} }}", quoted(behavior)));
    }
    if let Some(tag) = &filter.tag {
        parts.push(format!("{{ tags: {{_any: {{_eq: {}}}}} }}", quoted(tag)));
    }
    if let Some(status) = &filter.status {
        parts.push(match status.as_str() {
            "open" => "{closed_at: {_eq: null}}".into(),
            "closed" => "{closed_at: {_ne: null}}".into(),
            _ => bail!("invalid status '{status}'; next call: sessions {{\"action\":\"help\"}}"),
        });
    }
    if let Some(after) = &filter.created_after {
        parts.push(format!("{{created_at: {{_ge: {}}}}}", quoted(after)));
    }
    if let Some(before) = &filter.created_before {
        parts.push(format!("{{created_at: {{_lt: {}}}}}", quoted(before)));
    }
    if let Some(cursor) = cursor {
        let id_operator = if cursor.hit_sequence.is_some() {
            "_le"
        } else {
            "_lt"
        };
        parts.push(format!("{{_or: [{{created_at: {{_lt: {time}}}}}, {{created_at: {{_eq: {time}}}, session_id: {{{id_operator}: {id}}}}}]}}", time=quoted(&cursor.created_at), id=quoted(&cursor.session_id)));
    }
    Ok(if parts.is_empty() {
        "{}".into()
    } else {
        format!("{{_and: [{}]}}", parts.join(","))
    })
}

fn matches_text(row: &Value, needle: Option<&str>) -> bool {
    needle.is_none_or(|needle| {
        let needle = needle.to_lowercase();
        row["session_id"]
            .as_str()
            .unwrap_or_default()
            .to_lowercase()
            .contains(&needle)
            || row["title"]["text"]
                .as_str()
                .unwrap_or_default()
                .to_lowercase()
                .contains(&needle)
    })
}

fn rows(response: Value, collection: &str) -> Result<Vec<Value>> {
    anyhow::ensure!(
        response
            .get("errors")
            .is_none_or(|e| e.is_null() || e.as_array().is_some_and(Vec::is_empty)),
        "sessions query failed: {}",
        response["errors"]
    );
    let rows = response["data"][collection]
        .as_array()
        .cloned()
        .with_context(|| format!("sessions response omitted {collection}"))?;
    if collection == "AgentSession" {
        for row in &rows {
            crate::session::decode_session_row(row)?;
        }
    }
    Ok(rows)
}

fn compact(row: &Value, current_session: Option<&str>, agent: &str) -> Value {
    json!({"session_id":row["session_id"],"title":row["title"]["text"],"created_at":row["created_at"],"closed_at":row["closed_at"],"behavior_id":row["behavior_id"],"tags":row["tags"],"agent_did":row["agent_did"],"requester_did":row["requester_did"],"is_current":current_session.is_some_and(|id|super::is_current_session(agent,id,row["agent_did"].as_str().unwrap_or_default(),row["session_id"].as_str().unwrap_or_default())),"session_doc_id":row["_docID"]})
}

pub(super) struct AnswerFirst<'a>(pub &'a Value);

impl Serialize for AnswerFirst<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::{SerializeMap, SerializeSeq};
        match self.0 {
            Value::Object(fields) => {
                let order = [
                    "sessions",
                    "hits",
                    "messages",
                    "count",
                    "session",
                    "help",
                    "session_id",
                    "title",
                    "text",
                    "excerpt",
                    "closed_at",
                    "created_at",
                    "behavior_id",
                    "tags",
                    "message_doc_id",
                    "session_doc_id",
                    "payload_references",
                    "next_cursor",
                    "details",
                    "scope",
                ];
                let mut map = serializer.serialize_map(Some(fields.len()))?;
                for key in order {
                    if let Some(value) = fields.get(key) {
                        map.serialize_entry(key, &AnswerFirst(value))?;
                    }
                }
                for (key, value) in fields {
                    if !order.contains(&key.as_str()) {
                        map.serialize_entry(key, &AnswerFirst(value))?;
                    }
                }
                map.end()
            }
            Value::Array(values) => {
                let mut seq = serializer.serialize_seq(Some(values.len()))?;
                for value in values {
                    seq.serialize_element(&AnswerFirst(value))?;
                }
                seq.end()
            }
            value => value.serialize(serializer),
        }
    }
}

pub(super) async fn run(
    tool: &SessionHistoryTool,
    args: &SessionHistoryParams,
    action: &str,
) -> Result<Value> {
    if action == "help" {
        return Ok(json!({"help":action_help(args.topic.as_deref())?}));
    }
    let current = crate::tool_call_lifecycle::runtime::current_tool_runtime_context();
    if let Some(context) = &current {
        anyhow::ensure!(
            context.agent_did.as_deref() == Some(tool.agent_did.as_str()),
            "sessions requires the running principal; next call: sessions {{\"action\":\"help\"}}"
        );
    }
    let requester = current
        .as_ref()
        .and_then(|context| context.requester_did.clone());
    let current_session = current.and_then(|context| context.session_id);
    let agent = tool.agent_did.clone();
    let identity = agent
        .parse()
        .context("sessions requires a valid running DID")?;
    let args = args.clone();
    let action = action.to_owned();
    ConfigAccess::transact_local(
        &tool.node,
        Some(identity),
        "tool.sessions.discovery",
        move |txn| {
            let requester = requester.clone();
            let agent = agent.clone();
            let args = args.clone();
            let action = action.clone();
            let current_session = current_session.clone();
            Box::pin(async move {
                execute(
                    txn,
                    &agent,
                    requester.as_deref(),
                    current_session.as_deref(),
                    &args,
                    &action,
                )
                .await
            })
        },
    )
    .await
}

async fn execute(
    txn: &ConfigApplyTxn<'_>,
    agent: &str,
    requester: Option<&str>,
    current_session: Option<&str>,
    args: &SessionHistoryParams,
    action: &str,
) -> Result<Value> {
    let limit = clamp_limit(args.limit).min(100);
    let filter = args.filter.clone().unwrap_or_default();
    anyhow::ensure!(
        args.limit.is_none_or(|limit| (1..=100).contains(&limit)),
        "limit must be 1 through 100; next call: sessions {{\"action\":\"help\"}}"
    );
    if matches!(action, "get" | "transcript") {
        let id = args
            .session_id
            .as_deref()
            .filter(|id| !id.trim().is_empty())
            .context("session_id required; next call: sessions {\"action\":\"list\"}")?;
        let session = rows(
            txn.execute(&format!(
                "{{AgentSession(filter: {{session_id: {{_eq: {}}}}}, limit: 2) {{{FIELDS}}}}}",
                quoted(id)
            ))
            .await?,
            "AgentSession",
        )?;
        anyhow::ensure!(
            session.len() == 1,
            "session is unavailable or ambiguous under ACP; next call: sessions {{\"action\":\"list\"}}"
        );
        let agent = session[0]["agent_did"]
            .as_str()
            .context("session agent DID missing")?;
        let requester = session[0]["requester_did"].as_str();
        if action == "get" {
            let details = if args.details {
                let value = txn
                    .execute(&super::session_investigation_scoped_query(
                        agent,
                        id,
                        Some(requester),
                    ))
                    .await?;
                let envelope: super::InvestigationEnvelope =
                    super::decode(value.get("data"), "session investigation")?;
                let docs = envelope
                    .requests
                    .iter()
                    .map(|row| row.doc_id.clone().context("request has no physical ID"))
                    .collect::<Result<Vec<_>>>()?;
                let calls = if docs.is_empty() {
                    super::InvestigationCallsEnvelope {
                        inference_calls: vec![],
                        child_requests: vec![],
                    }
                } else {
                    let query = super::session_investigation_calls_scoped_query(
                        agent,
                        &docs,
                        Some(requester),
                    );
                    let value = txn.execute(&query).await?;
                    super::decode(value.get("data"), "session investigation calls")?
                };
                Some(super::build_session_investigation(
                    agent, id, envelope, calls,
                )?)
            } else {
                None
            };
            return Ok(json!({"session":session[0],"details":details}));
        }
        return transcript(txn, agent, requester, id, args, limit).await;
    }
    anyhow::ensure!(
        action != "count" || args.cursor.is_none(),
        "count does not accept cursor; next call: sessions {{\"action\":\"count\"}}"
    );
    let cursor = args
        .cursor
        .as_deref()
        .map(serde_json::from_str::<Cursor>)
        .transpose()
        .context("invalid cursor; next call: sessions {\"action\":\"list\"}")?;
    let condition = filter_query(&filter, cursor.as_ref())?;
    if action == "count" && filter.text.is_none() {
        let result = txn
            .execute(&format!(
                "{{total: COUNT(AgentSession: {{filter: {condition}}})}}"
            ))
            .await?;
        let count = result["data"]["total"]
            .as_u64()
            .context("sessions exact aggregate count missing")?;
        return Ok(
            json!({"count":count,"scope":{"agent_did":agent,"requester_did":requester,"visibility":"acp"}}),
        );
    }
    let needle = if action == "search" {
        Some(
            args.query
                .as_deref()
                .filter(|query| !query.trim().is_empty())
                .ok_or_else(|| missing_search_query(args))?,
        )
    } else {
        None
    };
    let mut selected = Vec::new();
    let mut hits = Vec::new();
    let mut count = 0usize;
    let mut scan_cursor = cursor;
    let mut last_selected = None;
    let mut last_hit = None;
    loop {
        let condition = filter_query(&filter, scan_cursor.as_ref())?;
        let page = rows(txn.execute(&format!("{{AgentSession(filter: {condition}, order: [{{created_at: DESC}},{{session_id: DESC}}], limit: 100) {{{FIELDS}}}}}")).await?, "AgentSession")?;
        if page.is_empty() {
            break;
        }
        let exhausted = page.len() < 100;
        for row in page {
            let position = Cursor {
                hit_sequence: None,
                created_at: row["created_at"]
                    .as_str()
                    .context("session has no creation time")?
                    .into(),
                session_id: row["session_id"]
                    .as_str()
                    .context("session has no ID")?
                    .into(),
            };
            let resume_sequence = scan_cursor
                .as_ref()
                .filter(|cursor| cursor.session_id == position.session_id)
                .and_then(|cursor| cursor.hit_sequence);
            scan_cursor = Some(position.clone());
            if !matches_text(&row, filter.text.as_deref()) {
                continue;
            }
            if let Some(needle) = needle {
                let session_hits = search_session(
                    txn,
                    agent,
                    requester,
                    &row,
                    needle,
                    resume_sequence,
                    limit.saturating_add(1).saturating_sub(hits.len()),
                )
                .await?;
                for (sequence, hit) in session_hits {
                    if hits.len() == limit {
                        return Ok(
                            json!({"hits":hits,"next_cursor":serde_json::to_string(&last_hit)?,"search_kind":"literal_scan"}),
                        );
                    }
                    hits.push(hit);
                    last_hit = Some(Cursor {
                        hit_sequence: Some(sequence),
                        ..position.clone()
                    });
                }
            } else {
                count += 1;
                if action != "count" {
                    if selected.len() == limit {
                        return Ok(
                            json!({"sessions":selected,"next_cursor":serde_json::to_string(&last_selected)? ,"scope":{"agent_did":agent,"requester_did":requester}}),
                        );
                    }
                    selected.push(compact(&row, current_session, agent));
                    last_selected = Some(position);
                }
            }
        }
        if exhausted {
            break;
        }
    }
    Ok(if action == "count" {
        json!({"count":count,"scope":{"agent_did":agent,"requester_did":requester,"visibility":"acp"}})
    } else if action == "search" {
        json!({"hits":hits,"next_cursor":null,"search_kind":"literal_scan"})
    } else {
        json!({"sessions":selected,"next_cursor":null,"scope":{"agent_did":agent,"requester_did":requester}})
    })
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TranscriptCursor {
    sequence: u32,
    #[serde(default)]
    offset: usize,
}

async fn transcript(
    txn: &ConfigApplyTxn<'_>,
    agent: &str,
    requester: Option<&str>,
    id: &str,
    args: &SessionHistoryParams,
    limit: usize,
) -> Result<Value> {
    let cursor = args.cursor.as_deref().map(serde_json::from_str::<TranscriptCursor>).transpose()
        .context("invalid transcript cursor; next call: sessions {\"action\":\"transcript\",\"session_id\":\"<ID>\"}")?;
    let bounds = cursor
        .as_ref()
        .map(|cursor| {
            format!(
                ",sequence: {{{}: {}}}",
                if cursor.offset == 0 { "_gt" } else { "_ge" },
                cursor.sequence
            )
        })
        .unwrap_or_default();
    let page_limit = limit.min(6);
    let headers = rows(txn.execute(&format!("{{AgentMessage(filter: {{{},session_id: {{_eq: {}}}{bounds}}},order: {{sequence: ASC}},limit: {}) {{{}}}}}",scope(agent,requester),quoted(id),page_limit+1,crate::session::canonical_rows::AGENT_MESSAGE_FIELDS)).await?,"AgentMessage")?;
    let mut reader = TxnCanonicalReader::new(txn, agent, requester);
    let observed = headers
        .iter()
        .map(crate::session::canonical_rows::decode_transcript_message_row)
        .collect::<Result<Vec<_>>>()?;
    reader.observe_headers(&observed);
    let mut messages = Vec::new();
    let mut next = None;
    for row in headers.iter().take(page_limit) {
        let header_id = row["_docID"]
            .as_str()
            .context("header has no physical ID")?;
        let (header, message) = reader.load_message(header_id).await?;
        let presentation = gents_protocol::transcript::present_message(&message);
        let offset = cursor
            .as_ref()
            .filter(|cursor| cursor.sequence == header.sequence)
            .map_or(0, |cursor| cursor.offset);
        let text: String = presentation
            .body_markdown
            .chars()
            .skip(offset)
            .take(8000)
            .collect();
        let consumed = offset.saturating_add(text.chars().count());
        let complete = presentation.body_markdown.chars().count() <= consumed;
        messages.push(json!({"message_doc_id":header_id,"sequence":header.sequence,"role":presentation.role.label(),"text":text,"text_offset":offset,"text_complete":complete,"payload_references":header.payload_references()}));
        if !complete {
            next = Some(serde_json::to_string(&TranscriptCursor {
                sequence: header.sequence,
                offset: consumed,
            })?);
            break;
        }
        if headers.len() > messages.len() {
            next = Some(serde_json::to_string(&TranscriptCursor {
                sequence: header.sequence,
                offset: 0,
            })?);
        } else {
            next = None;
        }
    }
    Ok(json!({"messages":messages,"session_id":id,"next_cursor":next}))
}

async fn search_session(
    txn: &ConfigApplyTxn<'_>,
    _agent: &str,
    _requester: Option<&str>,
    session: &Value,
    needle: &str,
    resume_sequence: Option<i64>,
    remaining: usize,
) -> Result<Vec<(i64, Value)>> {
    let id = session["session_id"]
        .as_str()
        .context("session ID missing")?;
    let agent = session["agent_did"]
        .as_str()
        .context("session agent DID missing")?;
    let requester = session["requester_did"].as_str();
    let mut hits = Vec::new();
    if resume_sequence.is_none() && matches_text(session, Some(needle)) {
        hits.push((-1, json!({"session_id":id,"session_doc_id":session["_docID"],"title":session["title"]["text"],"source":"metadata"})));
    }
    let headers = rows(txn.execute(&format!("{{AgentMessage(filter: {{{},session_id: {{_eq: {}}}}},order: {{sequence: ASC}}) {{{}}}}}",scope(agent,requester),quoted(id),crate::session::canonical_rows::AGENT_MESSAGE_FIELDS)).await?,"AgentMessage")?;
    let mut reader = TxnCanonicalReader::new(txn, agent, requester);
    let observed = headers
        .iter()
        .map(crate::session::canonical_rows::decode_transcript_message_row)
        .collect::<Result<Vec<_>>>()?;
    reader.observe_headers(&observed);
    let needle_lower = needle.to_lowercase();
    for row in headers {
        if hits.len() >= remaining {
            break;
        }
        let header_id = row["_docID"].as_str().context("header ID missing")?;
        let sequence = row["sequence"]
            .as_i64()
            .context("message has no sequence")?;
        if resume_sequence.is_some_and(|after| sequence <= after) {
            continue;
        }
        let (header, message) = reader.load_message(header_id).await?;
        let text = gents_protocol::transcript::present_message(&message).body_markdown;
        if let Some(position) = text.to_lowercase().find(&needle_lower) {
            let excerpt: String = text
                .chars()
                .skip(
                    text.to_lowercase()[..position]
                        .chars()
                        .count()
                        .saturating_sub(80),
                )
                .take(240)
                .collect();
            hits.push((sequence, json!({"session_id":id,"message_doc_id":header_id,"sequence":header.sequence,"excerpt":excerpt,"payload_references":header.payload_references()})));
        }
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{AgentIdentity, KeyIdentity};
    use crate::lifecycle::test_support::{pin_fixed_signing_identity, PIN_FIXED_DID};
    use crate::llm::tool::Tool;
    use std::sync::Arc;

    async fn seed(
        node: &Arc<defra_node::EmbeddedNode>,
        agent: &str,
        requester: Option<&str>,
        id: &str,
        title: &str,
        closed: bool,
    ) {
        ConfigAccess::write_local(node, "test.sessions.discovery", &format!(r#"mutation {{create_AgentSession(input: {{agent_did: {},requester_did: {},session_id: {},behavior_id: "engineer",created_at: "2025-01-01T00:00:00Z",closed_at: {},title: {{text: {},source: "user"}},tags: ["release"]}}) {{_docID}}}}"#,quoted(agent),requester.map(quoted).unwrap_or_else(||"null".into()),quoted(id),if closed {quoted("2025-01-02T00:00:00Z")}else{"null".into()},quoted(title))).await.unwrap();
    }

    async fn call(tool: &SessionHistoryTool, params: Value) -> Value {
        serde_json::from_str(
            &Tool::call(tool, serde_json::from_value(params).unwrap())
                .await
                .unwrap(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn sessions_reads_cross_agent_documents_only_when_acp_allows_the_caller() {
        let identities = tempfile::tempdir().unwrap();
        let owner = pin_fixed_signing_identity(identities.path());
        let reader =
            KeyIdentity::load_or_create(identities.path().join("reader.key"), None).unwrap();
        let denied =
            KeyIdentity::load_or_create(identities.path().join("denied.key"), None).unwrap();
        let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        let policy = node
            .add_dac_policy(
                owner.did(),
                r#"
name: Sessions reader test
resources:
  - name: sessions
    relations:
      - name: reader
    permissions:
      - name: read
        expr: reader
      - name: update
      - name: delete
"#,
            )
            .await
            .unwrap();
        ConfigAccess::Local(node.clone())
            .add_schema(&crate::schema::AGENT_SESSION_SCHEMA.replace(
                "type AgentSession ",
                &format!("type AgentSession @policy(id: \"{policy}\", resource: \"sessions\") "),
            ))
            .await
            .unwrap();
        let owner_did = owner.did().to_owned();
        let doc = ConfigAccess::transact_local(&node, Some(owner_did.parse().unwrap()), "test.sessions.acp.create", |txn| {
            let did = owner_did.clone();
            Box::pin(async move {
                let value = txn.execute(&format!("mutation {{add_AgentSession(input: {{agent_did: {}, requester_did: \"different-requester\", session_id: \"shared\", behavior_id: \"engineer\", created_at: \"2026-01-01T00:00:00Z\"}}) {{_docID}}}}", quoted(&did))).await?;
                Ok(value["data"]["add_AgentSession"][0]["_docID"].as_str().with_context(|| format!("ACP session fixture creation failed: {value}"))?.to_owned())
            })
        }).await.unwrap();
        node.add_dac_actor_relationship(owner.did(), "AgentSession", &doc, "reader", reader.did())
            .await
            .unwrap();
        let allowed_tool = SessionHistoryTool::new(node.clone(), reader.did());
        assert_eq!(
            call(&allowed_tool, json!({"action":"count"})).await["count"],
            1
        );
        let found = call(&allowed_tool, json!({"action":"get","session_id":"shared"})).await;
        assert_eq!(found["session"]["agent_did"], owner.did());
        assert_eq!(found["session"]["requester_did"], "different-requester");
        let denied_tool = SessionHistoryTool::new(node.clone(), denied.did());
        assert_eq!(
            call(&denied_tool, json!({"action":"count"})).await["count"],
            0
        );
        assert!(
            call(&denied_tool, json!({"action":"list"})).await["sessions"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let error = Tool::call(
            &denied_tool,
            serde_json::from_value(json!({"action":"get","session_id":"shared"})).unwrap(),
        )
        .await
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("unavailable or ambiguous under ACP"));
        node.shutdown().await;
    }

    #[tokio::test]
    async fn discovery_counts_and_pages_canonical_sessions_without_requests() {
        let identities = tempfile::tempdir().unwrap();
        let owner = pin_fixed_signing_identity(identities.path());
        let foreign =
            KeyIdentity::load_or_create(identities.path().join("foreign.key"), None).unwrap();
        let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        seed(&node, owner.did(), None, "atlas-a", "Atlas original", true).await;
        seed(&node, owner.did(), None, "atlas-b", "Atlas followup", true).await;
        seed(&node, owner.did(), None, "support", "Support", false).await;
        seed(
            &node,
            foreign.did(),
            None,
            "foreign-session",
            "Foreign private",
            true,
        )
        .await;
        seed(
            &node,
            owner.did(),
            Some(foreign.did()),
            "restricted",
            "Requester private",
            true,
        )
        .await;
        for start in (0..5001).step_by(500) {
            let rows = (start..(start+500).min(5001)).map(|index|format!("{{agent_did: {},request_id: {},session_id: \"support\",purpose: \"normal\",created_at: \"2026-01-01T00:00:00Z\"}}",quoted(owner.did()),quoted(&format!("noise-{index}")))).collect::<Vec<_>>().join(",");
            ConfigAccess::write_local(
                &node,
                "test.sessions.scan_bound",
                &format!("mutation {{create_AgentRequest(input: [{rows}]) {{_docID}}}}"),
            )
            .await
            .unwrap();
        }
        let tool = SessionHistoryTool::new(node.clone(), owner.did());
        let filter = json!({"status":"closed","tag":"release","text":"atlas"});
        assert_eq!(
            call(&tool, json!({"action":"count","filter":filter})).await["count"],
            2
        );
        assert_eq!(call(&tool, json!({"action":"count"})).await["count"], 5);
        let first = call(&tool, json!({"action":"list","filter":filter,"limit":1})).await;
        assert_eq!(first["sessions"][0]["session_id"], "atlas-b");
        let second = call(
            &tool,
            json!({"action":"list","filter":filter,"limit":1,"cursor":first["next_cursor"]}),
        )
        .await;
        assert_eq!(second["sessions"][0]["session_id"], "atlas-a");
        assert!(second["next_cursor"].is_null());
        assert_eq!(
            call(&tool, json!({"action":"count","filter":{"text":"atlas"}})).await["count"],
            2
        );
        let recovery = Tool::call(
            &tool,
            serde_json::from_value(json!({"action":"get","session_id":"atlas-typo"})).unwrap(),
        )
        .await
        .unwrap_err();
        assert!(recovery.to_string().contains("next call: sessions"));
        let cross_requester = call(&tool, json!({"action":"get","session_id":"restricted"})).await;
        assert_eq!(cross_requester["session"]["requester_did"], foreign.did());
        let cross_agent = call(
            &tool,
            json!({"action":"get","session_id":"foreign-session"}),
        )
        .await;
        assert_eq!(cross_agent["session"]["agent_did"], foreign.did());
        node.shutdown().await;
    }

    #[test]
    fn search_recovery_preserves_literal_text_and_requires_a_query_when_none_was_supplied() {
        for text in ["quoted \"decision\"\nline", "日本語", ""] {
            let args =
                serde_json::from_value(json!({"action":"search","filter":{"text":text}})).unwrap();
            let error = missing_search_query(&args).to_string();
            let next: Value =
                serde_json::from_str(error.split_once("next call: sessions ").unwrap().1).unwrap();
            assert_eq!(
                next["query"],
                if text.is_empty() {
                    "<text to find>"
                } else {
                    text
                }
            );
            assert!(next.get("filter").is_none());
        }
    }

    #[tokio::test]
    async fn transcript_search_reconstructs_canonical_text_and_pages_hits() {
        let identities = tempfile::tempdir().unwrap();
        let owner = pin_fixed_signing_identity(identities.path());
        let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        seed(&node, owner.did(), None, "work", "Investigation", false).await;
        for (sequence, text) in [
            (1, "The retry decision was retain state."),
            (2, "The retry budget was three calls."),
        ] {
            crate::session::import_history_observation(
                &node,
                "physical-request",
                "work",
                owner.did(),
                None,
                text,
                &format!("evidence-{sequence}"),
                sequence,
                None,
            )
            .await;
        }
        crate::session::import_history_observation(
            &node,
            "foreign-request",
            "foreign",
            PIN_FIXED_DID,
            Some("other-requester"),
            "The retry secret is protected.",
            "private-evidence",
            1,
            None,
        )
        .await;
        let tool = SessionHistoryTool::new(node.clone(), owner.did());
        let error = Tool::call(&tool, serde_json::from_value(json!({"action":"search","filter":{"text":"retry decision","tag":"release"},"limit":1})).unwrap()).await.unwrap_err();
        let error = error.to_string();
        let recovery: Value =
            serde_json::from_str(error.split_once("next call: sessions ").unwrap().1).unwrap();
        assert_eq!(recovery["query"], "retry decision");
        assert_eq!(recovery["filter"], json!({"tag":"release"}));
        assert_eq!(recovery["limit"], 1);
        let recovered = call(&tool, recovery).await;
        assert_eq!(recovered["hits"][0]["session_id"], "work");
        assert_eq!(recovered["hits"][0]["sequence"], 1);
        let definition = Tool::definition(&tool, String::new()).await;
        let validator = jsonschema::validator_for(&definition.parameters).unwrap();
        for invalid in [
            json!({"action":"search"}),
            json!({"action":"get"}),
            json!({"action":"transcript"}),
        ] {
            assert!(!validator.is_valid(&invalid), "{invalid}");
        }
        for valid in [
            json!({}),
            json!({"action":"search","query":"retry"}),
            json!({"action":"get","session_id":"work"}),
            json!({"action":"transcript","session_id":"work"}),
        ] {
            assert!(validator.is_valid(&valid), "{valid}");
        }
        let help = call(&tool, json!({"action":"help","topic":"search"})).await;
        assert!(help["help"].as_str().unwrap().contains("literal"));
        assert!(!help["help"].as_str().unwrap().contains("compaction"));
        let first = call(&tool, json!({"action":"search","query":"retry","limit":1})).await;
        assert_eq!(first["hits"][0]["sequence"], 1);
        assert!(first["hits"][0]["message_doc_id"].is_string());
        assert!(first["hits"][0]["payload_references"][0]["close_doc_id"].is_string());
        let second = call(
            &tool,
            json!({"action":"search","query":"retry","limit":1,"cursor":first["next_cursor"]}),
        )
        .await;
        assert_eq!(second["hits"][0]["sequence"], 2);
        assert!(second["next_cursor"].is_null());
        let first = call(
            &tool,
            json!({"action":"transcript","session_id":"work","limit":1}),
        )
        .await;
        assert_eq!(
            first["messages"][0]["text"],
            "The retry decision was retain state."
        );
        let second = call(&tool,json!({"action":"transcript","session_id":"work","limit":1,"cursor":first["next_cursor"]})).await;
        assert_eq!(second["messages"][0]["sequence"], 2);
        assert!(second["next_cursor"].is_null());
        node.shutdown().await;
    }
}
