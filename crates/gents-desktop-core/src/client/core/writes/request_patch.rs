use std::hash::{Hash, Hasher};

use anyhow::{Context, Result};
use gents::config_client::ConfigAccess;

use super::super::bootstrap::normalize_required;
use super::{find_request_in_scope, ClientCore, RefreshedRequestProjection};
use crate::client::query::load_chat_patch;
use crate::client::store::ClientStore;

#[cfg(test)]
mod tests;

const REQUEST_PATCH_SIGNATURE_CAPACITY: usize = 2_048;

fn request_patch_cache_reusable(
    memoized: bool,
    same_signature: bool,
    fence_accepted: bool,
    rows_current: bool,
) -> bool {
    memoized && same_signature && fence_accepted && rows_current
}

fn chat_patch_signature(patch: &ClientStore) -> (usize, usize, u64) {
    let rows = patch.row_count();
    match serde_json::to_vec(&patch.to_rows()) {
        Ok(bytes) => {
            let mut hasher = rapidhash::fast::RapidHasher::default();
            bytes.hash(&mut hasher);
            let mut byte_len = bytes.len();
            for row in &patch.transcript_messages {
                row.doc_id.hash(&mut hasher);
                match serde_json::to_vec(&row.message) {
                    Ok(encoded) => {
                        byte_len = byte_len.saturating_add(encoded.len());
                        encoded.hash(&mut hasher);
                    }
                    Err(_) => return (rows, 0, 0),
                }
            }
            for row in &patch.output_segments {
                row.doc_id.hash(&mut hasher);
                match serde_json::to_vec(&row.segment) {
                    Ok(encoded) => {
                        byte_len = byte_len.saturating_add(encoded.len());
                        encoded.hash(&mut hasher);
                    }
                    Err(_) => return (rows, 0, 0),
                }
            }
            (rows, byte_len, hasher.finish())
        }
        Err(_) => (rows, 0, 0),
    }
}

fn strip_cli_operator_hints(message: &str) -> String {
    message
        .lines()
        .filter(|line| {
            !line.contains("gents init")
                && !line.contains("gents server")
                && !line.contains("--graphql")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

impl ClientCore {
    /// Terminal reads refresh operator configuration omitted by the client P2P route.
    /// Catalog replacement must precede the final request publication to retain page rows.
    pub async fn refresh_local_request(
        &self,
        node_did: &str,
        session_id: &str,
        request_id: Option<&str>,
        fallback_request_id: Option<&str>,
        page_request_doc_ids: &[String],
    ) -> Result<Option<RefreshedRequestProjection>> {
        let node_did = node_did.trim();
        let session_id = session_id.trim();
        let request_id = request_id.map(str::trim).filter(|id| !id.is_empty());
        let fallback_request_id = fallback_request_id
            .map(str::trim)
            .filter(|id| !id.is_empty());
        if node_did.is_empty() || session_id.is_empty() {
            return Ok(None);
        }

        let mut captured = self.store.capture_read();
        let (access, source) = match self.operator_observation_graphql(node_did) {
            Some(graphql) => (ConfigAccess::Graphql(graphql), "operator"),
            None => (
                self.store_access
                    .lock()
                    .await
                    .as_ref()
                    .context("local store access is unavailable")?
                    .clone(),
                "local",
            ),
        };
        let mut patch = load_chat_patch(
            &access,
            node_did,
            session_id,
            request_id,
            fallback_request_id,
            page_request_doc_ids,
        )
        .await?;
        let rows = patch.row_count();
        if rows == 0 {
            return Ok(None);
        }
        let mut signature = chat_patch_signature(&patch);
        let project = |patch: &ClientStore| {
            let requester_did = patch
                .sessions
                .first()
                .and_then(|session| session.requester_did.clone());
            let latest_request_id = patch
                .sessions
                .first()
                .and_then(|session| session.observation.as_ref())
                .and_then(|observation| observation.latest_request.as_ref())
                .map(|latest| latest.request_id.clone());
            let legacy_fallback = patch
                .sessions
                .first()
                .filter(|session| session.observation.is_none())
                .and_then(|_| fallback_request_id.map(str::to_owned));
            RefreshedRequestProjection {
                requests: patch.requests.clone(),
                requester_did,
                selected_request_id: request_id
                    .map(str::to_owned)
                    .or(latest_request_id)
                    .or(legacy_fallback),
                published_version: None,
            }
        };
        let cache_key_for = |projection: &RefreshedRequestProjection| {
            let requester = projection.requester_did.as_deref().unwrap_or("<null>");
            let request = projection
                .selected_request_id
                .as_deref()
                .unwrap_or("<none>");
            format!("{source}\0{node_did}\0{session_id}\0{requester}\0{request}")
        };
        let mut projection = project(&patch);
        let mut cache_key = cache_key_for(&projection);
        {
            let mut signatures = self.request_patch_signatures.lock().await;
            let cached_signature = signatures.get(&cache_key) == Some(&signature);
            if cached_signature {
                let (fence_accepted, rows_current) = self
                    .store
                    .current_request_patch_read_status(&captured, &patch);
                if request_patch_cache_reusable(
                    cached_signature,
                    true,
                    fence_accepted,
                    rows_current,
                ) {
                    return Ok(Some(projection));
                }
                signatures.remove(&cache_key);
                if !fence_accepted {
                    self.store.request_resync(Some(node_did));
                    return Ok(None);
                }
            }
        }

        let terminal = projection
            .selected_request_id
            .as_deref()
            .and_then(|request_id| patch.request_row(request_id))
            .is_some_and(|row| row.is_terminal());
        if terminal {
            self.refresh_node(node_did).await?;
            captured = self.store.capture_read();
            patch = load_chat_patch(
                &access,
                node_did,
                session_id,
                request_id,
                fallback_request_id,
                page_request_doc_ids,
            )
            .await?;
            if patch.row_count() == 0 {
                return Ok(None);
            }
            projection = project(&patch);
            cache_key = cache_key_for(&projection);
            signature = chat_patch_signature(&patch);
        }
        let (rows, bytes, _hash) = signature;
        let Some(version) = self.store.merge_chat_patch_at_read(&captured, patch) else {
            self.store.request_resync(Some(node_did));
            return Ok(None);
        };
        let mut signatures = self.request_patch_signatures.lock().await;
        if signatures.len() >= REQUEST_PATCH_SIGNATURE_CAPACITY {
            signatures.clear();
        }
        signatures.insert(cache_key, signature);
        drop(signatures);
        tracing::debug!(
            target: "gents_desktop_core::replication",
            request_id = projection.selected_request_id.as_deref().unwrap_or_default(),
            node_did,
            version,
            rows,
            bytes,
            terminal,
            source,
            "desktop selected request patch merged"
        );
        Ok(Some(RefreshedRequestProjection {
            published_version: Some(version),
            ..projection
        }))
    }

    pub async fn request_output(
        &self,
        node_did: &str,
        session_id: &str,
        request_id: &str,
    ) -> Result<Option<gents::session::CanonicalRequestOutput>> {
        let node_did = normalize_required("node_did", node_did)?;
        let session_id = normalize_required("session_id", session_id)?;
        let request_id = normalize_required("request_id", request_id)?;
        let Some(projection) = self
            .refresh_local_request(node_did, session_id, Some(request_id), None, &[])
            .await?
        else {
            return Ok(None);
        };
        let Some(row) = find_request_in_scope(&projection.requests, request_id, Some(node_did))?
        else {
            return Ok(None);
        };
        anyhow::ensure!(
            row.session_id.as_deref() == Some(session_id),
            "request output crossed the selected session scope"
        );
        let refs = projection.requests.iter().collect::<Vec<_>>();
        let answer = crate::client::store::request_answer_owner(&refs, &row);
        anyhow::ensure!(
            answer.node_did.as_deref() == Some(node_did)
                && answer.session_id.as_deref() == Some(session_id)
                && answer.requester_did == row.requester_did,
            "request answer crossed its admitted scope"
        );
        if !answer.is_terminal() || gents::request::lifecycle::folded_into(answer).is_some() {
            return Ok(None);
        }
        let access = match self.operator_observation_graphql(node_did) {
            Some(graphql) => ConfigAccess::Graphql(graphql),
            None => self
                .store_access
                .lock()
                .await
                .as_ref()
                .context("local store access is unavailable")?
                .clone(),
        };
        gents::session::observe_request_output(&access, answer)
            .await
            .map(Some)
    }

    pub async fn request_timeline(
        &self,
        node_did: &str,
        request_id: &str,
    ) -> Result<gents::run_timeline::RunTimeline> {
        normalize_required("node_did", node_did)?;
        let request_id = normalize_required("request_id", request_id)?;
        let access = gents::config_client::ConfigAccess::local(self.node_arc());
        let timeline = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            gents::run_timeline_fetch::load_run_timeline(&access, request_id),
        )
        .await
        .map_err(|_| anyhow::anyhow!("timed out loading timeline for {request_id}"))?
        .map_err(|error| anyhow::anyhow!("{}", strip_cli_operator_hints(&error.to_string())))?;
        Ok(timeline)
    }
}
