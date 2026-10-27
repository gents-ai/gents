use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::message::{AssistantContent, Message, UserContent};
use gents_protocol::row::AgentRequestRow;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::graphql::escape_graphql_string;
use crate::session::TxnCanonicalReader;
use crate::session_origin::SessionScope;
use crate::tool_surface::AgentToolConfig;

const TEXT_CHARS: usize = 8_000;
const TOOL_PREVIEW_CHARS: usize = 240;
const STATUS_LIMIT: usize = 20;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentSessionReadArgs {
    pub session_id: String,
    #[serde(default)]
    pub view: ReadView,
    pub limit: Option<usize>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReadView {
    #[default]
    Last,
    Recent,
    Status,
}

impl AgentSessionReadArgs {
    pub(crate) fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            !self.session_id.trim().is_empty(),
            "session_id is required; discover it with agent_session_list"
        );
        anyhow::ensure!(
            self.limit.is_none_or(|limit| (1..=6).contains(&limit)),
            "limit must be between 1 and 6"
        );
        anyhow::ensure!(
            self.view == ReadView::Recent || self.cursor.is_none(),
            "cursor is supported only for recent messages"
        );
        if let Some(cursor) = &self.cursor {
            let cursor: Cursor = serde_json::from_str(cursor)
                .context("invalid recent cursor; restart agent_session_read with view=recent")?;
            anyhow::ensure!(
                cursor.session_id == self.session_id.trim(),
                "cursor belongs to another session; restart agent_session_read with view=recent"
            );
        }
        Ok(())
    }

    pub(crate) fn validate_target(&self, target: &SessionScope) -> Result<()> {
        self.validate()?;
        if let Some(cursor) = &self.cursor {
            let cursor: Cursor = serde_json::from_str(cursor)?;
            anyhow::ensure!(cursor.node_did == target.node_did && cursor.requester_did == target.requester_did, "cursor belongs to another session scope; restart agent_session_read with view=recent");
        }
        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    node_did: String,
    requester_did: Option<String>,
    session_id: String,
    before_sequence: u32,
}

fn quoted(value: &str) -> String {
    format!("\"{}\"", escape_graphql_string(value))
}

fn rows(response: &Value, collection: &str) -> Result<Vec<Value>> {
    response["data"][collection]
        .as_array()
        .cloned()
        .with_context(|| format!("session inspection omitted {collection}"))
}

fn bounded(text: &str, limit: usize) -> (String, bool) {
    let mut chars = text.chars();
    let result = chars.by_ref().take(limit).collect();
    (result, chars.next().is_some())
}

/// Only ordinary transcript text enters the body; tool arguments, reasoning
/// and provider captures are never returned by this interface.
fn visible_text(message: &Message) -> String {
    let text_only = match message {
        Message::User { content } => Message::User {
            content: content
                .iter()
                .filter(|part| matches!(part, UserContent::Text(_)))
                .cloned()
                .collect(),
        },
        Message::Assistant { id, content } => Message::Assistant {
            id: id.clone(),
            content: content
                .iter()
                .filter(|part| matches!(part, AssistantContent::Text(_)))
                .cloned()
                .collect(),
        },
        Message::System { .. } => return String::new(),
    };
    gents_protocol::transcript::render_message_body_markdown(&text_only)
}

async fn request_row(
    txn: &ConfigApplyTxn<'_>,
    scope: &str,
    doc_id: &str,
) -> Result<Option<AgentRequestRow>> {
    let response = txn.execute(&format!("{{AgentRequest(filter: {{{scope}, _docID: {{_eq: {}}}}}, limit: 1) {{_docID request_id lifecycle_state failure_reason created_at claimed_at terminalized_at request_hop}}}}", quoted(doc_id))).await?;
    rows(&response, "AgentRequest")?
        .into_iter()
        .next()
        .map(serde_json::from_value)
        .transpose()
        .context("decoding inspected request")
}

fn request_summary(row: &AgentRequestRow) -> Value {
    let (reason, truncated) = bounded(
        row.failure_reason.as_deref().unwrap_or_default(),
        TOOL_PREVIEW_CHARS,
    );
    json!({"request_id": row.request_id, "lifecycle_state": row.lifecycle_state,
        "terminal_reason": if reason.is_empty() { None } else { Some(reason) },
        "reason_truncated": truncated, "created_at": row.created_at,
        "claimed_at": row.claimed_at, "terminalized_at": row.terminalized_at,
        "causal_hop": row.request_hop})
}

async fn tools_in_flight(txn: &ConfigApplyTxn<'_>, scope: &str) -> Result<Value> {
    let response = txn.execute(&format!("{{AgentToolCall(filter: {{{scope}, lifecycle_state: {{_in: [\"pending\", \"running\"]}}}}, order: {{started_at: ASC}}, limit: {}) {{tool_call_key tool_name lifecycle_state started_at}}}}", STATUS_LIMIT + 1)).await?;
    let rows = rows(&response, "AgentToolCall")?;
    let now = chrono::Utc::now();
    let tools = rows.iter().take(STATUS_LIMIT).map(|row| {
        let elapsed = row["started_at"].as_str().and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok()).map(|at| (now - at.with_timezone(&chrono::Utc)).num_seconds().max(0));
        json!({"tool_call_id": row["tool_call_key"], "name": row["tool_name"], "status": row["lifecycle_state"], "started_at": row["started_at"], "elapsed_seconds": elapsed})
    }).collect::<Vec<_>>();
    Ok(json!({"items": tools, "truncated": rows.len() > STATUS_LIMIT}))
}

/// Persisted AgentRequest inputs omit the field entirely when they equal
/// `RequestInput::default()` (canonical writer), so storage represents them as
/// absent or null; `AgentRequestRow` decodes that through `Option`. A present
/// value must still satisfy the strict input contract and fails loudly.
///
/// Errors are constructed fresh from the request id only: the serde cause can
/// embed raw persisted values, so it is never chained into the error.
fn decode_queued_input(row: &Value) -> Result<gents_protocol::request_input::RequestInput> {
    let input: Option<gents_protocol::request_input::RequestInput> =
        serde_json::from_value(row["input"].clone()).map_err(|_| {
            anyhow::anyhow!(
                "decoding queued session input for request {}",
                row["request_id"]
            )
        })?;
    Ok(input.unwrap_or_default())
}

async fn queued_steering(txn: &ConfigApplyTxn<'_>, scope: &str) -> Result<Value> {
    let response = txn.execute(&format!("{{AgentRequest(filter: {{{scope}, purpose: {{_eq: \"normal\"}}, lifecycle_state: {{_eq: \"pending\"}}}}, order: [{{created_at: ASC}}, {{request_id: ASC}}], limit: {}) {{request_id input created_at}}}}", STATUS_LIMIT + 1)).await?;
    let pending = rows(&response, "AgentRequest")?;
    let mut queued = Vec::new();
    for row in pending.iter().take(STATUS_LIMIT) {
        let input = decode_queued_input(row)?;
        if let Some(queue) = input
            .queue
            .filter(|queue| queue.source == gents_protocol::request_input::QueueSource::Steering)
        {
            queued.push(json!({"request_id": row["request_id"], "created_at": row["created_at"], "queued_after_request_id": queue.queued_after_request_id}));
        }
    }
    Ok(json!({"items": queued, "truncated": pending.len() > STATUS_LIMIT}))
}

async fn messages(
    txn: &ConfigApplyTxn<'_>,
    target: &SessionScope,
    args: &AgentSessionReadArgs,
) -> Result<Value> {
    let scope = crate::session::session_scope_filter(
        &target.node_did,
        &target.session_id,
        target.requester_did.as_deref(),
    );
    let cursor = args
        .cursor
        .as_deref()
        .map(serde_json::from_str::<Cursor>)
        .transpose()
        .context("invalid cursor; next call: agent_session_read with session_id and view=recent")?;
    if let Some(cursor) = &cursor {
        anyhow::ensure!(
            cursor.node_did == target.node_did
                && cursor.requester_did == target.requester_did
                && cursor.session_id == target.session_id,
            "cursor belongs to another session; restart agent_session_read with view=recent"
        );
    }
    let bounds = cursor
        .map(|cursor| format!(", sequence: {{_lt: {}}}", cursor.before_sequence))
        .unwrap_or_default();
    let role = if args.view == ReadView::Last {
        ", role: {_eq: \"assistant\"}"
    } else {
        ""
    };
    let limit = if args.view == ReadView::Last {
        1
    } else {
        args.limit.unwrap_or(6)
    };
    let response = txn.execute(&format!("{{AgentMessage(filter: {{{scope}{bounds}{role}}}, order: {{sequence: DESC}}, limit: {}) {{{}}}}}", limit + 1, crate::session::canonical_rows::AGENT_MESSAGE_FIELDS)).await?;
    let headers = rows(&response, "AgentMessage")?
        .iter()
        .map(crate::session::canonical_rows::decode_transcript_message_row)
        .collect::<Result<Vec<_>>>()?;
    let tool_calls = load_tool_call_summaries(txn, &scope, &headers, limit).await?;
    let mut reader =
        TxnCanonicalReader::new(txn, &target.node_did, target.requester_did.as_deref());
    reader.observe_headers(&headers);
    let mut text_budget = TEXT_CHARS;
    let mut result = Vec::new();
    for row in headers.iter().take(limit) {
        let (header, message) = reader.load_message(&row.doc_id).await?;
        let presentation = gents_protocol::transcript::present_message(&message);
        let (text, truncated) = bounded(&visible_text(&message), text_budget);
        text_budget -= text.chars().count();
        let tools = tool_summaries(&tool_calls, &header, &message)?;
        result.push(json!({"message_doc_id": row.doc_id, "sequence": header.sequence, "role": presentation.role.label(), "created_at": header.created_at, "text": text, "text_truncated": truncated, "tools": tools}));
    }
    let next = if args.view == ReadView::Recent && headers.len() > limit {
        headers
            .get(limit - 1)
            .map(|row| {
                serde_json::to_string(&Cursor {
                    node_did: target.node_did.clone(),
                    requester_did: target.requester_did.clone(),
                    session_id: target.session_id.clone(),
                    before_sequence: row.message.sequence,
                })
            })
            .transpose()?
    } else {
        None
    };
    result.reverse();
    Ok(json!({"items": result, "next_cursor": next}))
}

fn selected_tool_call_doc_ids(
    headers: &[crate::session::canonical_rows::TranscriptMessageRow],
    message_limit: usize,
) -> Vec<String> {
    let mut seen = RapidHashSet::new();
    let mut doc_ids = Vec::new();
    for header in headers.iter().take(message_limit) {
        let mut count = 0;
        for block in &header.message.blocks {
            let doc_id = match block {
                gents_protocol::output::MessageBlock::ToolCall {
                    tool_call_doc_id, ..
                }
                | gents_protocol::output::MessageBlock::ToolResult {
                    tool_call_doc_id, ..
                } => tool_call_doc_id,
                _ => continue,
            };
            if count == STATUS_LIMIT {
                break;
            }
            count += 1;
            if seen.insert(doc_id.clone()) {
                doc_ids.push(doc_id.clone());
            }
        }
    }
    doc_ids
}

fn tool_call_batch_query(scope: &str, doc_ids: &[String]) -> Option<String> {
    if doc_ids.is_empty() {
        return None;
    }
    let ids = doc_ids
        .iter()
        .map(|id| quoted(id))
        .collect::<Vec<_>>()
        .join(",");
    Some(format!(
        "{{AgentToolCall(filter: {{{scope}, _docID: {{_in: [{ids}]}}}}) {{_docID tool_call_key tool_call_id tool_name lifecycle_state}}}}"
    ))
}

async fn load_tool_call_summaries(
    txn: &ConfigApplyTxn<'_>,
    scope: &str,
    headers: &[crate::session::canonical_rows::TranscriptMessageRow],
    message_limit: usize,
) -> Result<RapidHashMap<String, Value>> {
    let doc_ids = selected_tool_call_doc_ids(headers, message_limit);
    let Some(query) = tool_call_batch_query(scope, &doc_ids) else {
        return Ok(RapidHashMap::new());
    };
    let response = txn.execute(&query).await?;
    rows(&response, "AgentToolCall")?
        .into_iter()
        .map(|row| {
            let doc_id = row["_docID"]
                .as_str()
                .context("tool-call summary row omitted _docID")?
                .to_owned();
            Ok((doc_id, row))
        })
        .collect()
}

fn tool_summaries(
    tool_calls: &RapidHashMap<String, Value>,
    header: &gents_protocol::output::TranscriptMessage,
    message: &Message,
) -> Result<Value> {
    let mut summaries = Vec::new();
    for (index, block) in header.blocks.iter().enumerate() {
        let doc_id = match block {
            gents_protocol::output::MessageBlock::ToolCall {
                tool_call_doc_id, ..
            }
            | gents_protocol::output::MessageBlock::ToolResult {
                tool_call_doc_id, ..
            } => tool_call_doc_id,
            _ => continue,
        };
        if summaries.len() == STATUS_LIMIT {
            break;
        }
        let row = tool_calls
            .get(doc_id)
            .context("canonical tool summary references an unavailable call")?;
        let preview = match message {
            Message::User { content } => content
                .get(index)
                .filter(|part| matches!(part, UserContent::ToolResult(_)))
                .map(|part| {
                    gents_protocol::transcript::render_message_body_markdown(&Message::User {
                        content: vec![part.clone()],
                    })
                }),
            _ => None,
        };
        let (preview, truncated) =
            bounded(preview.as_deref().unwrap_or_default(), TOOL_PREVIEW_CHARS);
        summaries.push(json!({"tool_call_id": row["tool_call_key"], "name": row["tool_name"], "status": row["lifecycle_state"], "result_preview": preview, "preview_truncated": truncated}));
    }
    let count = header
        .blocks
        .iter()
        .filter(|block| {
            matches!(
                block,
                gents_protocol::output::MessageBlock::ToolCall { .. }
                    | gents_protocol::output::MessageBlock::ToolResult { .. }
            )
        })
        .count();
    Ok(json!({"items": summaries, "truncated": count > STATUS_LIMIT}))
}

pub(crate) async fn agent_session_read(
    node: &Arc<EmbeddedNode>,
    caller: &crate::AgentRequest,
    tools: &AgentToolConfig,
    args: &AgentSessionReadArgs,
) -> Result<Value> {
    args.validate()?;
    anyhow::ensure!(args.session_id.trim() != caller.session_id, "agent_session_read cannot address the calling session; use sessions to inspect this conversation");
    let target = super::resolve_send_target(node, &caller.node_did, tools, args.session_id.trim())
        .await?
        .context(
            "session is not messageable; discover available sessions with agent_session_list",
        )?;
    let target = super::target_scope(&caller.node_did, &target);
    args.validate_target(&target)?;
    // Captured before the first contributing lookup (active_session_request).
    let observation_started_at =
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let active = crate::interrupt::active_session_request(
        node,
        &target.session_id,
        &target.node_did,
        target.requester_did.as_deref(),
    )
    .await?;
    let identity = caller
        .node_did
        .parse()
        .context("session inspection requires a valid caller DID")?;
    ConfigAccess::transact_local_readonly(node, Some(identity), "agent_session.read", |txn| {
        let target = target.clone();
        let active = active.clone();
        let args = args.clone();
        let observation_started_at = observation_started_at.clone();
        Box::pin(async move {
            let scope = crate::session::session_scope_filter(&target.node_did, &target.session_id, target.requester_did.as_deref());
            let response = txn.execute(&format!("{{AgentSession(filter: {{{scope}}}, limit: 2) {{{}}}}}", crate::session::AGENT_SESSION_FIELDS)).await?;
            let sessions = rows(&response, "AgentSession")?;
            anyhow::ensure!(sessions.len() == 1, "inspected session is unavailable or ambiguous");
            let session = crate::session::decode_session_row(&sessions[0])?.session;
            anyhow::ensure!(super::session_target_allowed(&session, &caller.node_did, tools), "inspected session is no longer messageable");
            let unobserved = if active.is_none() {
                match session.observation.as_ref().and_then(|view| view.latest_request.as_ref()) {
                    Some(observed) if request_row(txn, &scope, &observed.request_doc_id).await?.is_none() => Some(observed),
                    _ => None,
                }
            } else { None };
            let latest = crate::session::load_latest_request_in_txn(txn, &target.node_did, &target.session_id, Some(target.requester_did.as_deref())).await?;
            let doc_id = active.as_ref().and_then(|row| row.doc_id.as_deref()).or_else(|| latest.as_ref().map(|head| head.observed.request_doc_id.as_str()));
            let request = match unobserved {
                Some(observed) => Some(json!({"request_id": observed.request_id, "lifecycle_state": observed.lifecycle_state, "available": false})),
                None => match doc_id { Some(id) => request_row(txn, &scope, id).await?.map(|row| request_summary(&row)), None => None },
            };
            let mut result = json!({"ok": true, "session_id": target.session_id, "stopped_at": session.stopped_at, "view": args.view, "request": request});
            if args.view == ReadView::Status {
                result["tools_in_flight"] = tools_in_flight(txn, &scope).await?;
                result["queued_steering"] = queued_steering(txn, &scope).await?;
                result["goal"] = match crate::goal::load_canonical_goal_in_txn(txn, &target.node_did, &target.session_id).await? {
                    Some(goal) => { let (objective, truncated) = bounded(&goal.objective, TOOL_PREVIEW_CHARS); json!({"goal_id": goal.goal_id, "status": goal.status, "objective": objective, "objective_truncated": truncated, "tokens_used": goal.tokens_used, "token_budget": goal.token_budget, "active_time_seconds": goal.active_time_seconds}) },
                    None => Value::Null,
                };
                result["observation_started_at"] = json!(observation_started_at);
            } else {
                result["messages"] = messages(txn, &target, &args).await?;
            }
            Ok(result)
        })
    }).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_inspection_excludes_reasoning_and_tool_arguments() {
        let message = Message::Assistant {
            id: None,
            content: vec![
                AssistantContent::Text(gents_protocol::message::Text {
                    text: "界".repeat(TEXT_CHARS + 1),
                }),
                AssistantContent::Reasoning(gents_protocol::message::Reasoning::new(
                    "private reasoning",
                )),
            ],
        };
        let (text, truncated) = bounded(&visible_text(&message), TEXT_CHARS);
        assert!(truncated);
        assert_eq!(text, "界".repeat(TEXT_CHARS));
        assert!(!text.contains("private reasoning"));
    }

    #[test]
    fn tool_call_summary_batch_is_one_exact_scoped_read() {
        let ids = vec!["tool-1".to_owned(), "tool\"2".to_owned()];
        let query = tool_call_batch_query(
            r#"node_did: {_eq: "node"}, session_id: {_eq: "session"}, requester_did: {_eq: null}"#,
            &ids,
        )
        .unwrap();
        assert_eq!(query.matches("AgentToolCall(").count(), 1);
        assert!(query.contains(r#"_docID: {_in: ["tool-1","tool\"2"]}"#));
        assert!(query.contains(r#"requester_did: {_eq: null}"#));
        assert!(tool_call_batch_query("scope", &[]).is_none());
    }

    #[test]
    fn queued_input_decode_accepts_canonical_default_and_fails_malformed() {
        let request_id = "pending-request";
        // Canonical writer omits `input` when it equals the default.
        let absent = json!({"request_id": request_id, "input": Value::Null});
        let decoded = decode_queued_input(&absent).unwrap();
        assert!(decoded.queue.is_none());
        let missing = json!({"request_id": request_id});
        assert!(decode_queued_input(&missing).unwrap().queue.is_none());
        // Canonical steering input still round-trips.
        let steering = json!({"request_id": request_id, "input": {"queue": {
            "source": "steering", "policy": "append", "delivery": "steer",
            "queued_after_request_id": "active-request"}}});
        let decoded = decode_queued_input(&steering).unwrap();
        assert_eq!(
            decoded.queue.expect("steering queue").source,
            gents_protocol::request_input::QueueSource::Steering
        );
        // A present value that violates the strict input contract stays an
        // error naming the request; it must not decode silently, and the
        // error must be payload-free on every rendering: the serde cause can
        // embed raw persisted values, so no rendering may leak them.
        let secret = "sentinel-secret-payload";
        for malformed_input in [
            json!({"unmodeled": secret}),
            json!(format!("string where a struct was required: {secret}")),
        ] {
            let row = json!({"request_id": request_id, "input": malformed_input});
            let error = decode_queued_input(&row).unwrap_err();
            for rendered in [
                error.to_string(),
                format!("{error:#}"),
                format!("{error:?}"),
            ] {
                assert!(rendered.contains(request_id), "{rendered}");
                assert!(!rendered.contains(secret), "{rendered}");
                assert!(!rendered.contains("unmodeled"), "{rendered}");
                assert!(!rendered.contains("invalid type"), "{rendered}");
            }
        }
    }

    #[tokio::test]
    async fn trigger_worker_inspection_reads_canonical_output_status_and_goal() {
        use crate::identity::NodeIdentity;
        use crate::tool_call_lifecycle::admission_fixture::{
            claimed_signed_request_with_trigger_correlation, publish_accepted_on_claimed_request,
        };
        use crate::tool_call_lifecycle::AwaitMode;
        let (message, caller, _) =
            super::super::agent_tools_tests::started("agent-session-inspection").await;
        let node = message.admission.node.clone();
        let identity =
            crate::KeyIdentity::load_or_create(message.admission.path.join("test-agent.key"), None)
                .unwrap();
        let mut worker = claimed_signed_request_with_trigger_correlation(
            &node,
            "read-worker-request",
            "read-worker",
            &identity,
            None,
            Some("shard-assignment"),
            Some("R3a"),
        )
        .await;
        let mut tool = publish_accepted_on_claimed_request(
            node.clone(),
            &mut worker,
            identity.did(),
            0,
            "bash",
            "worker-check",
            json!({"command": "private tool arguments"}),
            AwaitMode::Foreground,
            true,
        )
        .await
        .unwrap();
        crate::goal::create_goal_for_session(
            &node,
            identity.did(),
            "read-worker",
            "verify shard R3a",
            Some(1000),
        )
        .await
        .unwrap();
        let tools = AgentToolConfig {
            enabled: true,
            targets: Vec::new(),
        };
        let mut queued = gents_protocol::request_admission::AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            "read-worker-steering",
            identity.did(),
            identity.did(),
            "general",
            "read-worker",
            "continue after verification",
            "interactive",
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
                identity.did(),
            ),
        );
        queued.input.queue = Some(gents_protocol::request_input::RequestQueue {
            delivery: gents_protocol::request_input::QueueDelivery::Steer,
            position: None,
            source: gents_protocol::request_input::QueueSource::Steering,
            policy: gents_protocol::request_input::QueuePolicy::Append,
            key: None,
            queued_after_request_id: Some(worker.request().request_id.clone()),
            interrupted_request_id: None,
            background_completion_wake_version: None,
        });
        crate::sign_agent_request_create(&identity, &mut queued)
            .await
            .unwrap();
        ConfigAccess::local(node.clone())
            .write(
                "test.inspection.steering",
                &queued.graphql_mutation().unwrap(),
            )
            .await
            .unwrap();
        let mut args = AgentSessionReadArgs {
            session_id: "read-worker".to_owned(),
            view: ReadView::Status,
            ..Default::default()
        };
        let status = agent_session_read(&node, &caller, &tools, &args)
            .await
            .unwrap();
        assert_eq!(status["request"]["request_id"], worker.request().request_id);
        assert_eq!(status["request"]["lifecycle_state"], "processing");
        assert_eq!(status["request"]["causal_hop"], 0);
        // Status read adds only the read-start timestamp; the Goal was
        // created before the read and is exposed as-is.
        assert_eq!(status["goal"]["objective"], "verify shard R3a");
        assert!(status["observation_started_at"].is_string());
        chrono::DateTime::parse_from_rfc3339(status["observation_started_at"].as_str().unwrap())
            .unwrap();
        assert_eq!(status["tools_in_flight"]["items"][0]["name"], "bash");
        assert_eq!(status["tools_in_flight"]["items"][0]["status"], "running");
        assert!(
            status["tools_in_flight"]["items"][0]["elapsed_seconds"]
                .as_i64()
                .unwrap()
                >= 0
        );
        assert_eq!(
            status["queued_steering"]["items"][0]["request_id"],
            "read-worker-steering"
        );
        assert_eq!(
            status["queued_steering"]["items"][0]["queued_after_request_id"],
            worker.request().request_id
        );
        assert!(!status.to_string().contains("private tool arguments"));
        tool.complete(&"界".repeat(1000)).await.unwrap();
        let writer = crate::streaming::DefraStreamWriter::new(
            node.clone(),
            identity.did(),
            std::time::Duration::ZERO,
        );
        writer
            .start_provider_attempt(
                &worker.request().doc_id,
                1,
                0,
                "inference.2".parse().unwrap(),
            )
            .await;
        let report = Message::Assistant {
            id: None,
            content: vec![
                AssistantContent::Reasoning(gents_protocol::message::Reasoning {
                    id: Some("private provider id".to_owned()),
                    content: vec![gents_protocol::message::ReasoningContent::Encrypted(
                        "private encrypted reasoning".to_owned(),
                    )],
                }),
                AssistantContent::Text(gents_protocol::message::Text {
                    text: "shard R3a verified".to_owned(),
                }),
            ],
        };
        let published = writer
            .publish_native_turn(&worker, 1, 0, &report)
            .await
            .unwrap();
        worker
            .terminalize_owned(
                crate::lifecycle::RequestTerminalOutcome::Completed,
                gents_protocol::output::TerminalOutput::Message {
                    message_doc_id: published.message_doc_id,
                },
                None,
            )
            .await
            .unwrap();
        args.view = ReadView::Last;
        let last = agent_session_read(&node, &caller, &tools, &args)
            .await
            .unwrap();
        assert_eq!(last["messages"]["items"][0]["text"], "shard R3a verified");
        assert_eq!(last["request"]["request_id"], "read-worker-steering");
        assert_eq!(last["request"]["lifecycle_state"], "pending");
        assert!(last["request"]["created_at"].is_string());
        // The last view carries messages only; it does not add status fields.
        assert!(last.get("observation_started_at").is_none());
        assert!(!last.to_string().contains("private encrypted reasoning"));
        assert!(!last.to_string().contains("private provider id"));
        args.view = ReadView::Recent;
        args.limit = Some(1);
        let mut messages = Vec::new();
        loop {
            let page = agent_session_read(&node, &caller, &tools, &args)
                .await
                .unwrap();
            assert!(!page.to_string().contains("private tool arguments"));
            assert!(!page.to_string().contains("private encrypted reasoning"));
            messages.extend(page["messages"]["items"].as_array().unwrap().clone());
            args.cursor = page["messages"]["next_cursor"]
                .as_str()
                .map(ToOwned::to_owned);
            if args.cursor.is_none() {
                break;
            }
        }
        assert!(messages.iter().any(|row| row["tools"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "bash" && tool["status"] == "completed")));
        assert!(messages
            .iter()
            .any(|row| row["tools"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(
                    |tool| tool["result_preview"] == "界".repeat(TOOL_PREVIEW_CHARS)
                        && tool["preview_truncated"] == true
                )));
        let result_message = messages
            .iter()
            .find(|row| row["role"] == "TOOL")
            .expect("canonical tool result remains inspectable");
        assert_eq!(
            result_message["text"], "",
            "full tool output must not appear in the message body"
        );
        let scope = crate::session::session_scope_filter(
            identity.did(),
            "read-worker",
            Some(identity.did()),
        );
        let observation = gents_protocol::session::SessionObservation {
            reply_preview: None,
            last_activity_at: chrono::Utc::now().to_rfc3339(),
            preview: None,
            latest_request: Some(gents_protocol::session::SessionRequestObservation {
                request_doc_id: "unobserved-request-document".to_owned(),
                request_id: "unobserved-request".to_owned(),
                lifecycle_state:
                    gents_protocol::request_lifecycle::RequestLifecycleState::Processing,
            }),
        };
        ConfigAccess::transact_local_write(&node, None, "test.inspection.index_only", |txn| {
            let scope = scope.clone();
            let observation = observation.clone();
            Box::pin(async move { txn.execute_with_variables(&format!("mutation($input: AgentSessionMutationInputArg!) {{update_AgentSession(filter: {{{scope}}}, input: $input) {{_docID}}}}"), &json!({"input": {"observation": observation}})).await })
        }).await.unwrap();
        args.view = ReadView::Status;
        let unavailable = agent_session_read(&node, &caller, &tools, &args)
            .await
            .unwrap();
        assert_eq!(unavailable["request"]["request_id"], "unobserved-request");
        assert_eq!(unavailable["request"]["available"], false);
        assert!(unavailable["observation_started_at"].is_string());
        args.session_id = caller.session_id.clone();
        assert!(agent_session_read(&node, &caller, &tools, &args)
            .await
            .is_err());
        let foreign_identity = crate::KeyIdentity::load_or_create(
            message.admission.path.join("other-principal.key"),
            None,
        )
        .unwrap();
        let foreign = crate::AgentRequest {
            node_did: foreign_identity.did().to_owned(),
            ..caller.clone()
        };
        args.session_id = "read-worker".to_owned();
        let error = agent_session_read(&node, &foreign, &tools, &args)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("not messageable"), "{error:#}");
        drop(worker);
        node.shutdown().await;
        std::fs::remove_dir_all(&message.admission.path).unwrap();
    }
}
