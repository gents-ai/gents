use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{clamp_limit, SessionHistoryParams, SessionHistoryTool};
use crate::session::TxnCanonicalReader;
use crate::store::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::store::graphql::escape_graphql_string;

const FIELDS: &str = crate::session::AGENT_SESSION_FIELDS;
const HELP: &str = "Read canonical sessions visible to your authenticated DID through DefraDB ACP. Actions: list, count, search, get, transcript, output. Use {\"action\":\"help\",\"topic\":\"search\"} for an action's syntax. Copied IDs grant no access; ACP determines visibility across nodes and requesters.";

fn action_help(topic: Option<&str>) -> Result<&'static str> {
    Ok(match topic {
        None => HELP,
        Some("list") => {
            r#"{"action":"list","filter":{"status":"closed","tag":"release"},"limit":10}. Filters: agent_id, status (open/closed), tag, created_after (inclusive), created_before (exclusive), text (title or ID substring). Order: created_at descending, then ID descending. Pass next_cursor as cursor with the same filter. Limit: 1–100."#
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
        Some("output") => {
            r#"{"action":"output","call_id":"<call_id>"}. Returns a tool call's full stored output, including bytes trimmed from the model's view, 16000 bytes per page. call_id comes from a tool result or a [tool: …] stub; session_id defaults to the current session. Continue with the returned next_call: its offset_bytes and expected_hash keep pages from mixing output versions. If a call_id is ambiguous, pass the tool_call_doc_id the error lists."#
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
    pub agent_id: Option<String>,
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
        "node_did: {{_eq: {}}}, requester_did: {{_eq: {}}}",
        quoted(agent),
        requester.map(quoted).unwrap_or_else(|| "null".into())
    )
}

fn filter_query(filter: &SessionFilter, cursor: Option<&Cursor>) -> Result<String> {
    let mut parts = Vec::new();
    if let Some(behavior) = &filter.agent_id {
        parts.push(format!("{{ agent_id: {{_eq: {}}} }}", quoted(behavior)));
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
    json!({"session_id":row["session_id"],"title":row["title"]["text"],"created_at":row["created_at"],"closed_at":row["closed_at"],"agent_id":row["agent_id"],"tags":row["tags"],"node_did":row["node_did"],"requester_did":row["requester_did"],"is_current":current_session.is_some_and(|id|super::is_current_session(agent,id,row["node_did"].as_str().unwrap_or_default(),row["session_id"].as_str().unwrap_or_default())),"session_doc_id":row["_docID"]})
}

const OUTPUT_PAGE_BYTES: usize = 16_000;
/// Search stops reading a session's headers once it has enough hits.
const SEARCH_MESSAGE_PAGE: usize = if cfg!(test) { 1 } else { 100 };

pub(super) const OUTPUT_ORDER: &[&str] = &[
    "text",
    "next_call",
    "complete",
    "offset_bytes",
    "next_offset_bytes",
    "total_bytes",
    "output_hash",
    "tool_name",
    "call_id",
    "tool_call_doc_id",
    "session_id",
];

pub(super) struct AnswerFirst<'a>(pub &'a Value);

impl Serialize for AnswerFirst<'_> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
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
                    "agent_id",
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
    let current = crate::tools::call::lifecycle::runtime::current_tool_runtime_context();
    if let Some(context) = &current {
        anyhow::ensure!(
            context.node_did.as_deref() == Some(tool.node_did.as_str()),
            "sessions requires the running node; next call: sessions {{\"action\":\"help\"}}"
        );
    }
    let requester = current
        .as_ref()
        .and_then(|context| context.requester_did.clone());
    let current_session = current.and_then(|context| context.session_id);
    let agent = tool.node_did.clone();
    let identity = agent
        .parse()
        .context("sessions requires a valid running DID")?;
    let args = args.clone();
    let action = action.to_owned();
    ConfigAccess::transact_local_readonly(
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
    if action == "output" {
        return tool_output(txn, current_session, args).await;
    }
    if matches!(action, "get" | "transcript") {
        let id = args
            .session_id
            .as_deref()
            .filter(|id| !id.trim().is_empty())
            .context("session_id required; next call: sessions {\"action\":\"list\"}")?;
        let session = authorized_session(txn, id).await?;
        let agent = session["node_did"]
            .as_str()
            .context("session node DID missing")?;
        let requester = session["requester_did"].as_str();
        if action == "get" {
            let details = if args.details {
                let value = txn
                    .execute(&super::investigation::session_investigation_scoped_query(
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
                    let query = super::investigation::session_investigation_calls_scoped_query(
                        agent,
                        &docs,
                        Some(requester),
                    );
                    let value = txn.execute(&query).await?;
                    super::decode(value.get("data"), "session investigation calls")?
                };
                Some(super::investigation::build_session_investigation(
                    agent, id, envelope, calls,
                )?)
            } else {
                None
            };
            return Ok(json!({"session":session,"details":details}));
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
            json!({"count":count,"scope":{"node_did":agent,"requester_did":requester,"visibility":"acp"}}),
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
                            json!({"sessions":selected,"next_cursor":serde_json::to_string(&last_selected)? ,"scope":{"node_did":agent,"requester_did":requester}}),
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
        json!({"count":count,"scope":{"node_did":agent,"requester_did":requester,"visibility":"acp"}})
    } else if action == "search" {
        json!({"hits":hits,"next_cursor":null,"search_kind":"literal_scan"})
    } else {
        json!({"sessions":selected,"next_cursor":null,"scope":{"node_did":agent,"requester_did":requester}})
    })
}

async fn authorized_session(txn: &ConfigApplyTxn<'_>, id: &str) -> Result<Value> {
    let mut session = rows(
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
    Ok(session.remove(0))
}

async fn tool_output(
    txn: &ConfigApplyTxn<'_>,
    current_session: Option<&str>,
    args: &SessionHistoryParams,
) -> Result<Value> {
    let nonblank = |value: &Option<String>| {
        value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let session_id = nonblank(&args.session_id)
        .or_else(|| current_session.map(str::to_owned))
        .context(
            "output needs session_id outside a session; next call: sessions {\"action\":\"list\"}",
        )?;
    let call_id = nonblank(&args.call_id);
    let named_doc = nonblank(&args.tool_call_doc_id);
    let session = authorized_session(txn, &session_id).await?;
    let agent = session["node_did"]
        .as_str()
        .context("session node DID missing")?;
    let requester = session["requester_did"].as_str();
    let transcript_call = json!({"action":"transcript","session_id":session_id});
    let tool_call_doc_id = match (named_doc, &call_id) {
        (Some(doc), _) => doc,
        (None, Some(key)) => {
            let headers = rows(
                txn.execute(&format!(
                    "{{AgentMessage(filter: {{{},session_id: {{_eq: {}}}}},order: {{sequence: ASC}}) {{{}}}}}",
                    scope(agent, requester),
                    quoted(&session_id),
                    crate::session::canonical_rows::AGENT_MESSAGE_FIELDS
                ))
                .await?,
                "AgentMessage",
            )?;
            let mut docs = std::collections::BTreeSet::new();
            for row in &headers {
                let header = crate::session::canonical_rows::decode_transcript_message_row(row)?;
                for block in &header.message.blocks {
                    use gents_protocol::output::MessageBlock;
                    let (MessageBlock::ToolCall {
                        tool_call_doc_id,
                        id,
                        call_id,
                        ..
                    }
                    | MessageBlock::ToolResult {
                        tool_call_doc_id,
                        id,
                        call_id,
                        ..
                    }) = block
                    else {
                        continue;
                    };
                    if id == key || call_id.as_deref() == Some(key.as_str()) {
                        docs.insert(tool_call_doc_id.clone());
                    }
                }
            }
            match docs.len() {
                0 => bail!(
                    "no tool call with call_id {key} in session {session_id}; next call: sessions {transcript_call}"
                ),
                1 => docs.pop_first().unwrap(),
                _ => bail!(
                    "call_id {key} names {} tool calls in session {session_id}: {}; next call: sessions {}",
                    docs.len(),
                    docs.iter().cloned().collect::<Vec<_>>().join(", "),
                    json!({"action":"output","session_id":session_id,"tool_call_doc_id":docs.first()})
                ),
            }
        }
        (None, None) => bail!(
            "output requires call_id; next call: sessions {{\"action\":\"help\",\"topic\":\"output\"}}"
        ),
    };
    let read = crate::tools::call::lifecycle::query::load_tool_call_read_in_txn(
        txn,
        &tool_call_doc_id,
        agent,
        &session_id,
        requester,
    )
    .await
    .map_err(|error| anyhow::anyhow!("{error:#}; next call: sessions {transcript_call}"))?;
    let mut next = json!({"action":"output","session_id":session_id});
    match &call_id {
        Some(call_id) => next["call_id"] = json!(call_id),
        None => next["tool_call_doc_id"] = json!(tool_call_doc_id),
    }
    let Some(output) = read.raw_result.as_deref() else {
        if read.result.is_none() || !read.lifecycle_state.is_terminal() {
            bail!(
                "tool call {tool_call_doc_id} has no delivered output yet (state {:?}); next call: sessions {next}",
                read.lifecycle_state
            );
        }
        bail!(
            "tool call {tool_call_doc_id} has no single text output to page; next call: sessions {transcript_call}"
        );
    };
    let offset = args.offset_bytes.unwrap_or(0);
    let page = crate::tools::builtin::defra_query::utf8_page(
        output,
        offset,
        args.expected_hash.as_deref(),
        OUTPUT_PAGE_BYTES,
    )
    .map_err(|error| {
        let reason = match error {
            crate::tools::builtin::defra_query::Utf8PageError::MissingHash => {
                "offset_bytes past 0 requires expected_hash"
            }
            crate::tools::builtin::defra_query::Utf8PageError::Changed => {
                "output changed since expected_hash; restart at offset 0"
            }
            crate::tools::builtin::defra_query::Utf8PageError::Offset => {
                "offset_bytes is not a UTF-8 boundary within the output"
            }
        };
        anyhow::anyhow!("{reason}; next call: sessions {next}")
    })?;
    let complete = page.end == output.len();
    let next_call = (!complete).then(|| {
        let mut next = next.clone();
        next["offset_bytes"] = json!(page.end);
        next["expected_hash"] = json!(page.hash);
        next
    });
    Ok(json!({
        "text": page.text,
        "next_call": next_call,
        "complete": complete,
        "offset_bytes": offset,
        "next_offset_bytes": page.end,
        "total_bytes": output.len(),
        "output_hash": page.hash,
        "tool_name": read.tool_name,
        "call_id": call_id,
        "tool_call_doc_id": tool_call_doc_id,
        "session_id": session_id,
    }))
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
    let agent = session["node_did"]
        .as_str()
        .context("session node DID missing")?;
    let requester = session["requester_did"].as_str();
    let mut hits = Vec::new();
    if resume_sequence.is_none() && matches_text(session, Some(needle)) {
        hits.push((-1, json!({"session_id":id,"session_doc_id":session["_docID"],"title":session["title"]["text"],"source":"metadata"})));
    }
    let mut reader = TxnCanonicalReader::new(txn, agent, requester);
    let needle_lower = needle.to_lowercase();
    let mut after = resume_sequence;
    'pages: loop {
        let bound = after
            .map(|sequence| format!(",sequence: {{_gt: {sequence}}}"))
            .unwrap_or_default();
        let headers = rows(txn.execute(&format!("{{AgentMessage(filter: {{{},session_id: {{_eq: {}}}{bound}}},order: {{sequence: ASC}},limit: {SEARCH_MESSAGE_PAGE}) {{{}}}}}",scope(agent,requester),quoted(id),crate::session::canonical_rows::AGENT_MESSAGE_FIELDS)).await?,"AgentMessage")?;
        let observed = headers
            .iter()
            .map(crate::session::canonical_rows::decode_transcript_message_row)
            .collect::<Result<Vec<_>>>()?;
        reader.observe_headers(&observed);
        let exhausted = headers.len() < SEARCH_MESSAGE_PAGE;
        for row in headers {
            if hits.len() >= remaining {
                break 'pages;
            }
            let header_id = row["_docID"].as_str().context("header ID missing")?;
            let sequence = row["sequence"]
                .as_i64()
                .context("message has no sequence")?;
            after = Some(sequence);
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
        if exhausted {
            break;
        }
    }
    Ok(hits)
}

#[cfg(test)]
mod tests;
