use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::session::AgentSession;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::target::session_target_allowed;
use crate::config_client::ConfigAccess;
use crate::graphql::escape_graphql_string;
use crate::session::origin::{SessionLineage, SessionScope};
use crate::tool_surface::AgentToolConfig;

const PAGE_SIZE: usize = 100;
const SCAN_PAGES: usize = 10;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentSessionListArgs {
    pub filter: Option<SessionFilter>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionFilter {
    pub agent: Option<String>,
    pub status: Option<String>,
    pub relationship: Option<String>,
    pub caused_by_trigger_id: Option<String>,
    pub caused_by_correlation: Option<String>,
    pub created_after: Option<String>,
    pub updated_after: Option<String>,
}

impl AgentSessionListArgs {
    pub(crate) fn validate(&self) -> Result<()> {
        if let Some(limit) = self.limit {
            anyhow::ensure!(
                (1..=100).contains(&limit),
                "limit must be between 1 and 100"
            );
        }
        if let Some(filter) = &self.filter {
            if let Some(status) = &filter.status {
                anyhow::ensure!(
                    matches!(status.as_str(), "busy" | "idle"),
                    "status must be busy or idle"
                );
            }
            if let Some(relationship) = &filter.relationship {
                anyhow::ensure!(
                    matches!(
                        relationship.as_str(),
                        "started_by_you"
                            | "started_you"
                            | "messaged"
                            | "messaged_you"
                            | "same_node"
                            | "allowed_target"
                    ),
                    "invalid relationship"
                );
            }
            for time in [&filter.created_after, &filter.updated_after]
                .into_iter()
                .flatten()
            {
                chrono::DateTime::parse_from_rfc3339(time)
                    .context("time filters require RFC 3339 timestamps")?;
            }
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    caller: SessionScopeCursor,
    filter: SessionFilter,
    doc_id: String,
    created_at: String,
    session_id: String,
    node_did: String,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SessionScopeCursor {
    node_did: String,
    session_id: String,
    requester_did: Option<String>,
}

impl From<&crate::AgentRequest> for SessionScopeCursor {
    fn from(caller: &crate::AgentRequest) -> Self {
        Self {
            node_did: caller.node_did.clone(),
            session_id: caller.session_id.clone(),
            requester_did: caller.requester_did.clone(),
        }
    }
}

fn quoted(value: &str) -> String {
    format!("\"{}\"", escape_graphql_string(value))
}

fn relationship(lineage: &SessionLineage, scope: &SessionScope, caller_node: &str) -> &'static str {
    if lineage.started.iter().any(|link| &link.scope == scope) {
        "started_by_you"
    } else if lineage
        .started_by
        .as_ref()
        .is_some_and(|link| &link.scope == scope)
    {
        "started_you"
    } else if lineage.sent.iter().any(|link| &link.scope == scope) {
        "messaged"
    } else if lineage.received.iter().any(|link| &link.scope == scope) {
        "messaged_you"
    } else if scope.node_did == caller_node {
        "same_node"
    } else {
        "allowed_target"
    }
}

fn last_activity(session: &AgentSession) -> &str {
    session
        .observation
        .as_ref()
        .map_or(&session.created_at, |view| &view.last_activity_at)
}

async fn matches_trigger(
    access: &ConfigAccess,
    session: &AgentSession,
    filter: &SessionFilter,
) -> Result<bool> {
    if filter.caused_by_trigger_id.is_none() && filter.caused_by_correlation.is_none() {
        return Ok(true);
    }
    let mut predicate = crate::session::session_scope_filter(
        &session.node_did,
        &session.session_id,
        session.requester_did.as_deref(),
    );
    for (field, value) in [
        ("caused_by_trigger_id", &filter.caused_by_trigger_id),
        ("caused_by_correlation", &filter.caused_by_correlation),
    ] {
        if let Some(value) = value {
            predicate.push_str(&format!(", {field}: {{_eq: {}}}", quoted(value)));
        }
    }
    let response = access
        .execute(&format!(
            "{{AgentRequest(filter: {{{}}}, limit: 1) {{_docID}}}}",
            crate::session::public_request_filter(&predicate)
        ))
        .await?;
    Ok(!response["data"]["AgentRequest"]
        .as_array()
        .context("trigger query omitted request rows")?
        .is_empty())
}

pub(crate) async fn agent_session_list(
    node: &Arc<EmbeddedNode>,
    caller: &crate::AgentRequest,
    tools: &AgentToolConfig,
    args: &AgentSessionListArgs,
) -> Result<Value> {
    args.validate()?;
    let access = ConfigAccess::local(node.clone());
    let filter = args.filter.clone().unwrap_or_default();
    let created_after = filter
        .created_after
        .as_deref()
        .map(chrono::DateTime::parse_from_rfc3339)
        .transpose()?;
    let updated_after = filter
        .updated_after
        .as_deref()
        .map(chrono::DateTime::parse_from_rfc3339)
        .transpose()?;
    let caller_scope = SessionScopeCursor::from(caller);
    let mut cursor = args
        .cursor
        .as_deref()
        .and_then(|cursor| serde_json::from_str::<Cursor>(cursor).ok())
        .filter(|cursor| {
            cursor.caller == caller_scope
                && cursor.filter == filter
                && !cursor.doc_id.is_empty()
                && !cursor.session_id.is_empty()
                && !cursor.node_did.is_empty()
                && chrono::DateTime::parse_from_rfc3339(&cursor.created_at).is_ok()
        });
    if let Some(anchor) = &cursor {
        let response = access.execute(&format!("{{AgentSession(filter: {{_docID: {{_eq: {}}}, requester_did: {{_eq: {}}}}}, limit: 1) {{_docID created_at session_id node_did}}}}", quoted(&anchor.doc_id), quoted(&caller.node_did))).await?;
        let rows = response["data"]["AgentSession"]
            .as_array()
            .context("cursor query omitted session rows")?;
        if !rows.iter().any(|row| {
            row["created_at"].as_str() == Some(anchor.created_at.as_str())
                && row["session_id"].as_str() == Some(anchor.session_id.as_str())
                && row["node_did"].as_str() == Some(anchor.node_did.as_str())
        }) {
            cursor = None;
        }
    }
    let lineage =
        crate::session::origin::lineage(&access, &SessionScope::of_request(caller)).await?;
    let limit = args.limit.unwrap_or(20);
    let mut result = Vec::new();
    let mut next_cursor = None;
    let mut exhausted = false;
    for _ in 0..SCAN_PAGES {
        let mut predicates = vec![format!(
            "{{requester_did: {{_eq: {}}}, session_id: {{_ne: {}}}}}",
            quoted(&caller.node_did),
            quoted(&caller.session_id)
        )];
        if let Some(agent) = &filter.agent {
            let agent_id = tools
                .target(agent)
                .map_or(agent.as_str(), |target| target.agent_id.as_str());
            predicates.push(format!("{{agent_id: {{_eq: {}}}}}", quoted(agent_id)));
            if let Some(target) = tools.target(agent) {
                predicates.push(format!(
                    "{{node_did: {{_eq: {}}}}}",
                    quoted(&target.target_node_did)
                ));
            }
        }
        if let Some(after) = &cursor {
            predicates.push(format!("{{_or: [{{created_at: {{_lt: {time}}}}}, {{created_at: {{_eq: {time}}}, session_id: {{_lt: {session}}}}}, {{created_at: {{_eq: {time}}}, session_id: {{_eq: {session}}}, node_did: {{_lt: {node}}}}}]}}", time=quoted(&after.created_at), session=quoted(&after.session_id), node=quoted(&after.node_did)));
        }
        let response = access.execute(&format!("{{AgentSession(filter: {{_and: [{}]}}, order: [{{created_at: DESC}}, {{session_id: DESC}}, {{node_did: DESC}}], limit: {PAGE_SIZE}) {{{}}}}}", predicates.join(","), crate::session::AGENT_SESSION_FIELDS)).await?;
        let rows = response["data"]["AgentSession"]
            .as_array()
            .context("session list omitted canonical rows")?;
        if rows.is_empty() {
            exhausted = true;
            break;
        }
        for (index, row) in rows.iter().enumerate() {
            let session = crate::session::decode_session_row(row)?.session;
            let after = Cursor {
                caller: SessionScopeCursor::from(caller),
                filter: filter.clone(),
                doc_id: row["_docID"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .context("session omitted physical identity")?
                    .to_owned(),
                created_at: session.created_at.clone(),
                session_id: session.session_id.clone(),
                node_did: session.node_did.clone(),
            };
            next_cursor = Some(serde_json::to_string(&after)?);
            cursor = Some(after);
            if !session_target_allowed(&session, &caller.node_did, tools) {
                continue;
            }
            super::resolve_send_target(node, &caller.node_did, tools, &session.session_id)
                .await?
                .context("listed session is no longer messageable")?;
            let scope = SessionScope::from(&session);
            let relation = relationship(&lineage, &scope, &caller.node_did);
            if filter
                .relationship
                .as_ref()
                .is_some_and(|value| value != relation)
            {
                continue;
            }
            if let Some(after) = &created_after {
                if &chrono::DateTime::parse_from_rfc3339(&session.created_at)? <= after {
                    continue;
                }
            }
            if let Some(after) = &updated_after {
                if &chrono::DateTime::parse_from_rfc3339(last_activity(&session))? <= after {
                    continue;
                }
            }
            if !matches_trigger(&access, &session, &filter).await? {
                continue;
            }
            let active = crate::request::interrupt::active_session_request(
                node,
                &scope.session_id,
                &scope.node_did,
                scope.requester_did.as_deref(),
            )
            .await?;
            let status = if active.is_some() { "busy" } else { "idle" };
            if filter.status.as_ref().is_some_and(|value| value != status) {
                continue;
            }
            let can_interrupt =
                super::interrupt_refusal(node, &SessionScope::of_request(caller), &scope)
                    .await?
                    .is_none();
            let agent = tools
                .targets
                .iter()
                .find(|target| {
                    target.target_node_did == session.node_did
                        && target.agent_id == session.agent_id
                })
                .map_or(session.agent_id.as_str(), |target| target.name.as_str());
            result.push(json!({
                "session_id": session.session_id, "node_did": session.node_did,
                "agent": agent, "agent_id": session.agent_id,
                "title": session.title.as_ref().map(|title| &title.text),
                "relationship": relation, "status": status,
                "stopped_at": session.stopped_at,
                "request_id": active.as_ref().map(|request| &request.request_id),
                "lifecycle_state": active.as_ref().and_then(|request| request.lifecycle_state),
                "last_activity_at": last_activity(&session),
                "can_message": true, "can_interrupt": can_interrupt
            }));
            if result.len() == limit {
                exhausted = index + 1 == rows.len() && rows.len() < PAGE_SIZE;
                break;
            }
        }
        if result.len() == limit || rows.len() < PAGE_SIZE {
            exhausted |= rows.len() < PAGE_SIZE && result.len() < limit;
            break;
        }
    }
    Ok(
        json!({"ok": true, "sessions": result, "next_cursor": if exhausted { None } else { next_cursor }}),
    )
}
