use super::observation::{load_session_request_bindings, RequestBindings};
use super::*;
use gents_inference::views::context::{ContextCallRow, ContextCalls, ContextCursor};

pub const PAGE_SIZE: usize = 128;
pub const CONTEXT_CALL_LIMIT: usize = 10;

pub async fn load_latest_session_context_calls(
    access: &dyn gents_store::config_client::ConfigRead,
    node_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
) -> Result<Vec<Value>> {
    anyhow::ensure!(
        !node_did.trim().is_empty() && !session_id.trim().is_empty(),
        "session accounting requires a node and session"
    );
    let mut selected = indexed_calls(access, node_did, requester_did, session_id).await?;
    let lower_bound = (selected.len() == CONTEXT_CALL_LIMIT)
        .then(|| {
            selected
                .last()?
                .queued_at
                .as_deref()
                .filter(|time| !time.is_empty())
                .map(str::to_owned)
        })
        .flatten();
    let candidates = gents_store::views::read_view(
        access,
        &ContextCalls::retained(node_did.trim(), lower_bound.clone()).limit(1),
    )
    .await?;
    if candidates.is_empty() {
        return values(selected);
    }
    let mut after = None;
    loop {
        let bindings = load_session_request_bindings(
            access,
            node_did,
            requester_did,
            session_id,
            RequestBindings::Page(after.as_deref()),
        )
        .await?;
        if bindings.is_empty() {
            return values(selected);
        }
        let rows = gents_store::views::read_view(
            access,
            &ContextCalls::retained(node_did.trim(), lower_bound.clone())
                .for_requests(bindings.keys().cloned().collect())
                .limit(CONTEXT_CALL_LIMIT as u64),
        )
        .await?;
        for row in rows {
            let physical = row
                .request_doc_id
                .as_deref()
                .context("InferenceCall accounting row has no request_doc_id")?;
            let logical = bindings
                .get(physical)
                .context("InferenceCall has no scoped physical request")?;
            anyhow::ensure!(
                Some(logical.as_str()) == row.request_id.as_deref(),
                "InferenceCall request identity does not match its scoped AgentRequest"
            );
            selected.push(row);
        }
        gents_inference::views::context::retain_latest(&mut selected, CONTEXT_CALL_LIMIT);
        if bindings.len() < PAGE_SIZE {
            return values(selected);
        }
        after = bindings.last_key_value().map(|(id, _)| id.clone());
    }
}

fn values(rows: Vec<ContextCallRow>) -> Result<Vec<Value>> {
    rows.into_iter()
        .map(|row| serde_json::to_value(row).map_err(Into::into))
        .collect()
}

async fn indexed_calls(
    access: &dyn gents_store::config_client::ConfigRead,
    node_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
) -> Result<Vec<ContextCallRow>> {
    let mut selected = Vec::with_capacity(CONTEXT_CALL_LIMIT);
    let mut cursor = None;
    loop {
        let mut view = ContextCalls::indexed(node_did.trim(), session_id.trim())
            .limit(CONTEXT_CALL_LIMIT as u64);
        if let Some(cursor) = cursor {
            view = view.after(cursor);
        }
        let rows = gents_store::views::read_view(access, &view).await?;
        if rows.is_empty() {
            return Ok(selected);
        }
        let ids = rows
            .iter()
            .filter_map(|row| {
                row.request_doc_id
                    .as_deref()
                    .filter(|id| !id.trim().is_empty())
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>();
        let bindings = load_session_request_bindings(
            access,
            node_did,
            requester_did,
            session_id,
            RequestBindings::Physical(&ids),
        )
        .await?;
        let page_len = rows.len();
        cursor = rows.last().map(ContextCursor::from);
        for row in rows {
            let Some(request_id) = row
                .request_doc_id
                .as_deref()
                .and_then(|id| bindings.get(id))
            else {
                continue;
            };
            anyhow::ensure!(
                row.request_id.as_deref() == Some(request_id.as_str()),
                "InferenceCall request identity does not match its scoped AgentRequest"
            );
            selected.push(row);
            if selected.len() == CONTEXT_CALL_LIMIT {
                return Ok(selected);
            }
        }
        if page_len < CONTEXT_CALL_LIMIT {
            return Ok(selected);
        }
    }
}

#[cfg(test)]
mod pushdown_tests;
