//! Read-only session discovery for the stock history picker and resume.
//!
//! A session ID is not an authorization boundary. Every request is scoped
//! to the bound principal/requester, and every session is checked against
//! its persisted owner before any summary is returned. There is no shim
//! history cache or second transcript store.

use std::collections::BTreeMap;

use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use gents::graphql::{ensure_no_errors, escape_graphql_string};
use gents_protocol::row::AgentRequestRow;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const PAGE_SIZE: usize = 128;

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ListParams {
    pub query: Option<String>,
    pub limit: Option<usize>,
    pub cursor: Option<String>,
    pub headless: Option<String>,
    #[serde(default, rename = "_meta")]
    pub meta: Value,
}

impl ListParams {
    pub(super) fn validate(&self) -> Result<()> {
        ensure!(self.limit != Some(0), "session list limit must be positive");
        ensure!(
            self.headless
                .as_deref()
                .is_none_or(|v| matches!(v, "include" | "exclude" | "only")),
            "unknown headless policy"
        );
        if let Some(filters) = self.meta.get("x.ai/facetFilters") {
            let filters = filters
                .as_object()
                .context("facetFilters must be an object")?;
            ensure!(
                filters.keys().all(|key| key == "kind"),
                "unsupported session facet filter"
            );
            if let Some(kind) = filters.get("kind") {
                ensure!(
                    kind.is_string()
                        || kind
                            .as_array()
                            .is_some_and(|items| items.iter().all(Value::is_string)),
                    "kind must be a string or string array"
                );
            }
        }
        self.boundary()?;
        Ok(())
    }

    fn query(&self) -> String {
        self.query
            .as_deref()
            .or_else(|| self.meta.get("x.ai/query").and_then(Value::as_str))
            .unwrap_or_default()
            .trim()
            .to_lowercase()
    }

    fn boundary(&self) -> Result<Option<Cursor>> {
        self.cursor
            .as_deref()
            .map(|value| {
                let cursor: Cursor =
                    serde_json::from_str(value).context("invalid session cursor")?;
                ensure!(
                    cursor.query == self.query(),
                    "session cursor belongs to another search"
                );
                Ok(cursor)
            })
            .transpose()
    }

    fn excludes_build(&self) -> bool {
        match self.meta.pointer("/x.ai~1facetFilters/kind") {
            Some(Value::String(kind)) => kind != "build",
            Some(Value::Array(kinds)) if !kinds.is_empty() => {
                !kinds.iter().any(|kind| kind == "build")
            }
            _ => false,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Cursor {
    updated_at: DateTime<Utc>,
    session_id: String,
    query: String,
}

/// Scan bounded pages without silently dropping an older session behind a
/// fixed request-count ceiling. Request IDs are the stable scan key; the
/// result is ordered by actual persisted activity, not lexical UUID order.
pub(super) async fn requests(
    node: &EmbeddedNode,
    principal: &str,
    session: Option<&str>,
) -> Result<Vec<AgentRequestRow>> {
    ensure!(
        !principal.trim().is_empty(),
        "session history requires a principal"
    );
    let agent = escape_graphql_string(principal);
    let session_filter = session
        .map(|id| format!(r#"session_id: {{_eq: "{}"}},"#, escape_graphql_string(id)))
        .unwrap_or_default();
    let mut after = String::new();
    let mut result = Vec::new();
    loop {
        let response = node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{
            agent_did: {{_eq: "{agent}"}}, requester_did: {{_eq: "{agent}"}},
            {session_filter} request_id: {{_gt: "{}"}}
        }}, order: {{request_id: ASC}}, limit: {PAGE_SIZE}) {{
            _docID request_id session_id agent_did requester_did behavior_id
            content metadata created_at terminalized_at lifecycle_state runtime_source_kind
            caused_by_parent_request_id caused_by_parent_request_doc_id
        }} }}"#,
                escape_graphql_string(&after)
            ))
            .await;
        ensure_no_errors(&response, "Grok session request history")?;
        let page: Vec<AgentRequestRow> = serde_json::from_value(
            response
                .data
                .as_ref()
                .and_then(|data| data.get("AgentRequest"))
                .cloned()
                .context("missing session request rows")?,
        )?;
        let done = page.len() < PAGE_SIZE;
        if let Some(last) = page.last() {
            ensure!(
                last.request_id > after,
                "session request pagination did not advance"
            );
            after = last.request_id.clone();
        }
        ensure!(
            page.iter()
                .all(|row| row.doc_id.as_deref().is_some_and(|id| !id.is_empty())),
            "history request lacks physical identity"
        );
        result.extend(page);
        if done {
            break;
        }
    }
    result.sort_by(|a, b| {
        (timestamp(a.created_at.as_deref()), &a.request_id)
            .cmp(&(timestamp(b.created_at.as_deref()), &b.request_id))
    });
    Ok(result)
}

/// Legacy shim sessions omitted requester_did. Only the exact request scope
/// above can authorize their content; null here never widens that scope.
fn readable_owner(row: &Value, principal: &str, behavior: &str) -> bool {
    row.get("agent_did").and_then(Value::as_str) == Some(principal)
        && row.get("behavior_id").and_then(Value::as_str) == Some(behavior)
        && row
            .get("requester_did")
            .and_then(Value::as_str)
            .is_none_or(|did| did == principal)
}

pub(super) async fn load(
    node: &EmbeddedNode,
    principal: &str,
    behavior: &str,
    session: &str,
) -> Result<Vec<AgentRequestRow>> {
    ensure!(!session.trim().is_empty(), "session ID must not be empty");
    let response = node
        .execute(&format!(
            r#"{{ AgentSession(filter: {{session_id: {{_eq: "{}"}}}}, limit: 2) {{
        session_id agent_did requester_did behavior_id
    }} }}"#,
            escape_graphql_string(session)
        ))
        .await;
    ensure_no_errors(&response, "Grok session owner")?;
    let owners = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentSession"))
        .and_then(Value::as_array)
        .context("missing session owner rows")?;
    ensure!(
        owners.len() == 1 && readable_owner(&owners[0], principal, behavior),
        "session is not readable by this bound behavior"
    );
    let rows = requests(node, principal, Some(session)).await?;
    ensure!(
        owners[0].get("requester_did").and_then(Value::as_str) == Some(principal)
            || !rows.is_empty(),
        "legacy session has no authorized request history"
    );
    Ok(rows)
}

fn timestamp(value: Option<&str>) -> Option<DateTime<Utc>> {
    value
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|time| time.with_timezone(&Utc))
}

pub(super) async fn list(
    node: &EmbeddedNode,
    principal: &str,
    behavior: &str,
    params: ListParams,
) -> Result<Value> {
    let mut grouped: BTreeMap<String, Vec<AgentRequestRow>> = BTreeMap::new();
    if !params.excludes_build() && params.headless.as_deref() != Some("only") {
        for row in requests(node, principal, None).await? {
            if let Some(session) = row.session_id.clone().filter(|id| !id.is_empty()) {
                grouped.entry(session).or_default().push(row);
            }
        }
    }
    let query = params.query();
    let boundary = params.boundary()?;
    let mut entries = Vec::new();
    // Batch owner validation: no N+1 per-request or per-session DB query.
    let ids: Vec<_> = grouped.keys().cloned().collect();
    for page in ids.chunks(PAGE_SIZE) {
        let ids = page
            .iter()
            .map(|id| format!("\"{}\"", escape_graphql_string(id)))
            .collect::<Vec<_>>()
            .join(",");
        let response = node
            .execute(&format!(
                r#"{{ AgentSession(filter: {{session_id: {{_in: [{ids}]}}}}) {{
            session_id agent_did requester_did behavior_id
        }} }}"#
            ))
            .await;
        ensure_no_errors(&response, "Grok history session owners")?;
        let owners = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentSession"))
            .and_then(Value::as_array)
            .context("missing history session owners")?;
        for owner in owners {
            if !readable_owner(owner, principal, behavior) {
                continue;
            }
            let Some(id) = owner.get("session_id").and_then(Value::as_str) else {
                continue;
            };
            let Some(rows) = grouped.remove(id) else {
                continue;
            };
            let first = &rows[0];
            // Children are hydrated under their parent, not offered as roots.
            if first.caused_by_parent_request_id.is_some()
                || first.caused_by_parent_request_doc_id.is_some()
            {
                continue;
            }
            let first_prompt = first.content.as_deref().unwrap_or_default().trim();
            if first_prompt.is_empty() {
                continue;
            }
            if !query.is_empty()
                && !id.to_lowercase().contains(&query)
                && !rows.iter().any(|row| {
                    row.content
                        .as_deref()
                        .unwrap_or_default()
                        .to_lowercase()
                        .contains(&query)
                })
            {
                continue;
            }
            let created = timestamp(first.created_at.as_deref());
            let Some(updated) = rows
                .iter()
                .flat_map(|row| {
                    [
                        timestamp(row.created_at.as_deref()),
                        timestamp(row.terminalized_at.as_deref()),
                    ]
                })
                .flatten()
                .max()
            else {
                continue;
            };
            if boundary.as_ref().is_some_and(|boundary| {
                (updated, id) >= (boundary.updated_at, boundary.session_id.as_str())
            }) {
                continue;
            }
            let summary: String = first_prompt
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .take(240)
                .collect();
            entries.push((
                updated,
                id.to_owned(),
                json!({
                    "sessionId": id, "summary": summary, "firstPrompt": first_prompt,
                "createdAt": created, "updatedAt": updated, "lastActiveAt": updated,
                "source": "gents", "_meta": {"x.ai/session": {"kind": "build", "facets": {}},
                    "gents/activity": if rows.iter().any(|row| row.lifecycle_state.is_some_and(|state| !state.is_terminal())) {"working"} else {"idle"}}
                }),
            ));
        }
    }
    entries.sort_by(|a, b| (&b.0, &b.1).cmp(&(&a.0, &a.1)));
    let limit = params.limit.unwrap_or(30).min(1000);
    let more = entries.len() > limit;
    entries.truncate(limit);
    let next = if more {
        entries
            .last()
            .map(|(time, id, _)| {
                serde_json::to_string(&Cursor {
                    updated_at: *time,
                    session_id: id.clone(),
                    query,
                })
            })
            .transpose()?
    } else {
        None
    };
    Ok(
        json!({"sessions": entries.into_iter().map(|(_, _, value)| value).collect::<Vec<_>>(),
            "nextCursor": next,
            "_meta": {"x.ai/listScope": "all", "x.ai/partial": {"conversations": false},
                "gents/historyScope": "bound-principal-and-behavior",
                "gents/historyLimitations": ["Historical cwd/model and stock headless kind are not persisted; unclassified Gents sessions are build entries."]}
        }),
    )
}

/// The leader dashboard is the stock client's server-hosted resume surface.
/// Unlike its local-filesystem history picker, selecting a roster entry
/// directly sends session/load without requiring a local JSONL file.
pub(super) async fn roster(node: &EmbeddedNode, principal: &str, behavior: &str) -> Result<Value> {
    let mut cursor = None;
    let mut sessions = Vec::new();
    loop {
        let page = list(
            node,
            principal,
            behavior,
            ListParams {
                limit: Some(1000),
                cursor,
                ..Default::default()
            },
        )
        .await?;
        for row in page["sessions"]
            .as_array()
            .context("missing roster source rows")?
        {
            sessions.push(json!({"sessionId":row["sessionId"], "title":row["summary"],
                "cwd":"", "activity":row["_meta"]["gents/activity"],
                "lastChangeUnixMs":timestamp(row["updatedAt"].as_str()).map(|time| time.timestamp_millis()),
                "origin":{"kind":"local"}}));
        }
        cursor = page["nextCursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    Ok(json!({"sessions":sessions}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_filters_validate_and_cursor_is_bound_to_search() {
        let mut params: ListParams = serde_json::from_value(
            json!({"query":"  HELLO ", "_meta":{"x.ai/facetFilters":{"kind":["build"]}}}),
        )
        .unwrap();
        params.validate().unwrap();
        assert!(!params.excludes_build());
        params.cursor = Some(
            serde_json::to_string(&Cursor {
                updated_at: Utc::now(),
                session_id: "s".into(),
                query: "hello".into(),
            })
            .unwrap(),
        );
        params.validate().unwrap();
        params.query = Some("another".into());
        assert!(params.validate().is_err());
        for invalid in [
            json!({"limit":0}),
            json!({"headless":"unknown"}),
            json!({"_meta":{"x.ai/facetFilters":{"cwd":"/tmp"}}}),
        ] {
            assert!(serde_json::from_value::<ListParams>(invalid)
                .unwrap()
                .validate()
                .is_err());
        }
    }
}
