use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{FromRequestParts as _, Request, State};
use axum::http::{header, HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use defra_node::Node as EmbeddedNode;
use rapidhash::RapidHashMap;

use super::owner_staged_route::OwnerStagedRoute;

/// Header carrying the detached grant's baseline revocation fence epoch over HTTP.
pub(crate) const DETACHED_GRANT_EPOCH_HEADER: &str = "x-gents-detached-grant-epoch";

/// Response header returning the node's current revocation fence epoch on HTTP
/// transaction begin.
pub(crate) const FENCE_EPOCH_HEADER: &str = "x-gents-relationship-fence-epoch";

const RELATIONSHIP_PATH: &str = "/api/v0/acp/document/relationship";
const TX_PATH: &str = "/api/v0/tx";
const TX_PREFIX: &str = "/api/v0/tx/";
const MAX_RELATIONSHIP_BODY_BYTES: usize = 64 * 1024;

/// Whether a relationship grant is a detached post-commit completion or an
/// explicit direct grant (`Identity.DetachedGrantFence.GrantOrigin`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrantOrigin {
    Detached,
    Explicit,
}

/// What the relationship-grant owner decides under the revocation fence
/// (`Identity.DetachedGrantFence.CompletionDecision`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionDecision {
    Grant,
    SkipRevoked,
    RefuseActorMismatch,
}

/// Pure owner decision matching Lean `Identity.DetachedGrantFence.decideGrant`.
pub fn decide_grant(
    origin: GrantOrigin,
    creator_matches: bool,
    revoked_since_queue: bool,
) -> CompletionDecision {
    if !creator_matches {
        return CompletionDecision::RefuseActorMismatch;
    }
    match origin {
        GrantOrigin::Explicit => CompletionDecision::Grant,
        GrantOrigin::Detached if revoked_since_queue => CompletionDecision::SkipRevoked,
        GrantOrigin::Detached => CompletionDecision::Grant,
    }
}

/// Exact ACP relationship tuple tracked by the per-node revocation fence.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RelationshipTuple {
    pub(crate) collection: String,
    pub(crate) doc_id: String,
    pub(crate) relation: String,
    pub(crate) target: String,
}

/// A transactional revocation whose caller was authorized by DefraDB inside the
/// transaction; retained until commit so any detached grant that landed after
/// the transaction's snapshot is cleared atomically under the fence gate.
pub(crate) struct StagedRevocation {
    pub(crate) tuple: RelationshipTuple,
    pub(crate) context: defra_core::RequestContext,
}

#[derive(Default)]
struct FenceState {
    epoch: u64,
    revoked_epoch: RapidHashMap<RelationshipTuple, u64>,
    detached_grant_seq: u64,
    last_detached_grant: RapidHashMap<RelationshipTuple, u64>,
    http_tx_begin_seq: RapidHashMap<String, u64>,
    pending_http_tx: RapidHashMap<String, Vec<StagedRevocation>>,
}

/// Per-node revocation fence serializing detached grant completion against
/// concurrent owner revocations (`tla/DetachedGrantRevocationFence.tla`).
pub(crate) struct RelationshipRevocationFence {
    gate: Arc<tokio::sync::Mutex<()>>,
    state: Mutex<FenceState>,
}

impl Default for RelationshipRevocationFence {
    fn default() -> Self {
        Self {
            gate: Arc::new(tokio::sync::Mutex::new(())),
            state: Mutex::new(FenceState::default()),
        }
    }
}

impl RelationshipRevocationFence {
    pub(crate) fn for_node(node: &Arc<EmbeddedNode>) -> Arc<Self> {
        crate::node_extension::attached_or_default::<Self>(node)
    }

    pub(crate) fn current_epoch(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .epoch
    }

    pub(crate) fn detached_grant_seq(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .detached_grant_seq
    }

    pub(crate) fn is_revoked_since(&self, tuple: &RelationshipTuple, baseline_epoch: u64) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .revoked_epoch
            .get(tuple)
            .copied()
            .unwrap_or(0)
            > baseline_epoch
    }

    pub(crate) fn record_detached_grant(&self, tuple: &RelationshipTuple) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.detached_grant_seq = state.detached_grant_seq.saturating_add(1);
        let seq = state.detached_grant_seq;
        state.last_detached_grant.insert(tuple.clone(), seq);
    }

    pub(crate) fn record_revocation(&self, tuple: RelationshipTuple) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.epoch = state.epoch.saturating_add(1);
        let next = state.epoch;
        state.revoked_epoch.insert(tuple, next);
    }

    fn detached_grant_after(&self, tuple: &RelationshipTuple, tx_begin_seq: u64) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .last_detached_grant
            .get(tuple)
            .copied()
            .unwrap_or(0)
            > tx_begin_seq
    }

    /// Records committed transactional revocations under a held fence gate and
    /// clears any detached grant that landed after the transaction's snapshot.
    pub(crate) async fn reconcile_committed_revocations(
        &self,
        node: &Arc<EmbeddedNode>,
        tx_begin_seq: u64,
        staged: Vec<StagedRevocation>,
    ) {
        for revocation in staged {
            let needs_clear = self.detached_grant_after(&revocation.tuple, tx_begin_seq);
            self.record_revocation(revocation.tuple.clone());
            if needs_clear {
                if let Err(error) = node
                    .delete_dac_actor_relationship_as(
                        &revocation.context,
                        &revocation.tuple.collection,
                        &revocation.tuple.doc_id,
                        &revocation.tuple.relation,
                        &revocation.tuple.target,
                    )
                    .await
                {
                    tracing::warn!(
                        %error,
                        collection = %revocation.tuple.collection,
                        doc_id = %revocation.tuple.doc_id,
                        relation = %revocation.tuple.relation,
                        "failed to clear detached grant racing a committed revocation transaction"
                    );
                }
            }
        }
    }

    pub(crate) async fn lock_gate(&self) -> tokio::sync::OwnedMutexGuard<()> {
        Arc::clone(&self.gate).lock_owned().await
    }

    fn record_http_tx_begin(&self, tx_id: String, begin_seq: u64) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .http_tx_begin_seq
            .insert(tx_id, begin_seq);
    }

    fn stage_http_tx_revocation(&self, tx_id: &str, revocation: StagedRevocation) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending_http_tx
            .entry(tx_id.to_owned())
            .or_default()
            .push(revocation);
    }

    fn take_http_tx_revocations(&self, tx_id: &str) -> (u64, Vec<StagedRevocation>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let begin_seq = state.http_tx_begin_seq.remove(tx_id).unwrap_or(0);
        let staged = state.pending_http_tx.remove(tx_id).unwrap_or_default();
        (begin_seq, staged)
    }

    fn discard_http_tx(&self, tx_id: &str) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.http_tx_begin_seq.remove(tx_id);
        state.pending_http_tx.remove(tx_id);
    }
}

#[derive(serde::Deserialize)]
struct RelationshipBody {
    collection: String,
    doc_id: String,
    relation: String,
    target_actor: String,
}

impl RelationshipBody {
    fn into_tuple(self) -> RelationshipTuple {
        RelationshipTuple {
            collection: self.collection,
            doc_id: self.doc_id,
            relation: self.relation,
            target: self.target_actor,
        }
    }
}

pub(crate) fn parse_detached_epoch(headers: &axum::http::HeaderMap) -> Option<u64> {
    headers
        .get(DETACHED_GRANT_EPOCH_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
}

pub(super) fn wrap_relationship_fence(
    route: OwnerStagedRoute,
    router: axum::Router,
) -> axum::Router {
    router.layer(axum::middleware::from_fn_with_state(
        route,
        fence_relationship_http,
    ))
}

async fn fence_relationship_http(
    State(route): State<OwnerStagedRoute>,
    request: Request,
    next: Next,
) -> Response {
    let Some((node, _)) = route.node() else {
        return next.run(request).await;
    };
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    if method == Method::POST && path == TX_PATH {
        return handle_http_tx_begin(&node, request, next).await;
    }
    if let Some(tx_id) = path.strip_prefix(TX_PREFIX) {
        let fence = RelationshipRevocationFence::for_node(&node);
        if method == Method::POST {
            let (begin_seq, staged) = fence.take_http_tx_revocations(tx_id);
            if staged.is_empty() {
                return next.run(request).await;
            }
            let _gate = fence.lock_gate().await;
            let response = next.run(request).await;
            if response.status().is_success() {
                fence
                    .reconcile_committed_revocations(&node, begin_seq, staged)
                    .await;
            }
            return response;
        }
        if method == Method::DELETE {
            fence.discard_http_tx(tx_id);
        }
        return next.run(request).await;
    }
    if path != RELATIONSHIP_PATH {
        return next.run(request).await;
    }
    if method == Method::DELETE {
        return handle_http_revocation(&node, request, next).await;
    }
    if method == Method::POST {
        return handle_http_detached_grant(&node, request, next).await;
    }
    next.run(request).await
}

async fn handle_http_tx_begin(node: &Arc<EmbeddedNode>, request: Request, next: Next) -> Response {
    let fence = RelationshipRevocationFence::for_node(node);
    let epoch = fence.current_epoch();
    let begin_seq = fence.detached_grant_seq();
    let response = next.run(request).await;
    let (mut parts, body) = response.into_parts();
    if let Ok(value) = HeaderValue::from_str(&epoch.to_string()) {
        parts
            .headers
            .insert(HeaderName::from_static(FENCE_EPOCH_HEADER), value);
    }
    if !parts.status.is_success() {
        return Response::from_parts(parts, body);
    }
    let Ok(bytes) = axum::body::to_bytes(body, MAX_RELATIONSHIP_BODY_BYTES).await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    if let Some(tx_id) = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|body| {
            body.get("id").and_then(|value| {
                value
                    .as_str()
                    .map(ToOwned::to_owned)
                    .or_else(|| value.as_u64().map(|id| id.to_string()))
            })
        })
    {
        fence.record_http_tx_begin(tx_id, begin_seq);
    }
    Response::from_parts(parts, Body::from(bytes))
}

async fn handle_http_revocation(
    node: &Arc<EmbeddedNode>,
    request: Request,
    next: Next,
) -> Response {
    let tx_id = request
        .headers()
        .get("x-defradb-tx")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let (mut parts, body) = request.into_parts();
    let bearer_token = parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
        })
        .map(|token| token.trim().to_owned());
    let caller_context = defra_http::ExtractIdentity::from_request_parts(&mut parts, &())
        .await
        .ok()
        .map(|extracted| {
            let did = extracted.did().map(ToString::to_string);
            let context = extracted.context().with_identity(did.clone());
            match (did, bearer_token.clone()) {
                (Some(actor), Some(token)) => context.with_bearer(Some(actor), Some(token)),
                _ => context,
            }
        });
    let Ok(bytes) = axum::body::to_bytes(body, MAX_RELATIONSHIP_BODY_BYTES).await else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let tuple = serde_json::from_slice::<RelationshipBody>(&bytes)
        .ok()
        .map(RelationshipBody::into_tuple);
    let request = Request::from_parts(parts, Body::from(bytes));
    let Some(tuple) = tuple else {
        return next.run(request).await;
    };
    let fence = RelationshipRevocationFence::for_node(node);
    if let Some(tx_id) = tx_id {
        let response = next.run(request).await;
        if response.status().is_success() {
            fence.stage_http_tx_revocation(
                &tx_id,
                StagedRevocation {
                    tuple,
                    context: caller_context.unwrap_or_default(),
                },
            );
        }
        return response;
    }
    let _gate = fence.lock_gate().await;
    let response = next.run(request).await;
    if response.status().is_success() {
        fence.record_revocation(tuple);
    }
    response
}

async fn handle_http_detached_grant(
    node: &Arc<EmbeddedNode>,
    request: Request,
    next: Next,
) -> Response {
    let Some(baseline_epoch) = parse_detached_epoch(request.headers()) else {
        return next.run(request).await;
    };
    if !request.headers().contains_key(header::AUTHORIZATION) {
        return next.run(request).await;
    }
    let (parts, body) = request.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_RELATIONSHIP_BODY_BYTES).await else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let tuple = serde_json::from_slice::<RelationshipBody>(&bytes)
        .ok()
        .map(RelationshipBody::into_tuple);
    let request = Request::from_parts(parts, Body::from(bytes));
    let Some(tuple) = tuple else {
        return next.run(request).await;
    };
    let fence = RelationshipRevocationFence::for_node(node);
    let _gate = fence.lock_gate().await;
    let revoked = fence.is_revoked_since(&tuple, baseline_epoch);
    match decide_grant(GrantOrigin::Detached, true, revoked) {
        CompletionDecision::SkipRevoked => {
            axum::Json(serde_json::json!({"ExistedAlready": false, "FencedRevoked": true}))
                .into_response()
        }
        CompletionDecision::Grant => {
            let response = next.run(request).await;
            if response.status().is_success() {
                fence.record_detached_grant(&tuple);
            }
            response
        }
        CompletionDecision::RefuseActorMismatch => StatusCode::FORBIDDEN.into_response(),
    }
}
