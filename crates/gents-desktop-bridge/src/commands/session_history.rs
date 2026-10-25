use anyhow::{Context, Result};
use gents_protocol::session::SessionArchiveVisibility;
use serde::{Deserialize, Serialize};

use crate::snapshot::{session_summary_order_key, SessionSummaryOrderKey};
use crate::types::{SessionHistoryPage, SessionHistoryRequest, SessionSummary};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    node_did: String,
    visibility: SessionArchiveVisibility,
    query: String,
    order: SessionSummaryOrderKey,
}

/// Pages follow the live runtime projection. Rows that move ahead of a saved
/// cursor are visible after refreshing the history list.
pub async fn session_history(
    core: &gents_desktop_core::client::ClientCore,
    request: SessionHistoryRequest,
) -> Result<SessionHistoryPage> {
    let snapshot = crate::snapshot::build_runtime_snapshot(core).await;
    let deployment = snapshot
        .deployments
        .into_iter()
        .find(|deployment| deployment.node_did == request.node_did)
        .context("chat history runtime is not connected")?;
    select_page(deployment.sessions, request)
}

/// The existing runtime projection owns scope, ordering and running-state facts.
/// Archive filtering changes only this history view, never the store or selected chat.
fn select_page(
    sessions: Vec<SessionSummary>,
    request: SessionHistoryRequest,
) -> Result<SessionHistoryPage> {
    let limit = request.limit.unwrap_or(30);
    anyhow::ensure!(
        (1..=100).contains(&limit),
        "history limit must be 1 through 100"
    );
    let query = request
        .query
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    let cursor = request
        .cursor
        .as_deref()
        .map(serde_json::from_str::<Cursor>)
        .transpose()
        .context("invalid history cursor")?;
    if let Some(cursor) = &cursor {
        anyhow::ensure!(
            cursor.node_did == request.node_did
                && cursor.visibility == request.visibility
                && cursor.query == query,
            "history cursor belongs to a different runtime or filter"
        );
    }
    let mut sessions = sessions
        .into_iter()
        .filter(|session| {
            session.node_did == request.node_did
                && request.visibility.includes(session.archived_at.is_some())
                && format!(
                    "{} {}",
                    session.title.as_deref().unwrap_or_default(),
                    session.preview_text.as_deref().unwrap_or_default()
                )
                .to_lowercase()
                .contains(&query)
        })
        .collect::<Vec<_>>();
    if let Some(cursor) = cursor {
        sessions.retain(|session| {
            session_summary_order_key(session)
                .cmp(&cursor.order)
                .is_gt()
        });
    }
    let has_more = sessions.len() > limit;
    sessions.truncate(limit);
    let next_cursor = if has_more {
        sessions
            .last()
            .map(|last| {
                serde_json::to_string(&Cursor {
                    node_did: request.node_did,
                    visibility: request.visibility,
                    query,
                    order: session_summary_order_key(last),
                })
            })
            .transpose()?
    } else {
        None
    };
    Ok(SessionHistoryPage {
        sessions,
        next_cursor,
    })
}

#[cfg(test)]
#[path = "session_history_tests.rs"]
mod tests;
