//! Authenticated export of exact signed DefraDB block material.
//!
//! The pinned DefraDB embedded-node builder exposes
//! [`defra_node::EmbeddedNode::authorized_signed_block_bytes`] but does not
//! install its block adapter on the HTTP server. This bridge mounts a
//! non-conflicting Gents route and binds the node after construction without
//! keeping it alive through a server-task reference cycle.

use std::sync::{Arc, OnceLock, Weak};

use axum::{
    extract::{Query, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// The Defra-compatible API prefix plus a Gents namespace avoids colliding
/// with DefraDB's currently unconfigured `/block/signed` route.
pub const SIGNED_BLOCK_HTTP_PATH: &str = "/api/v0/gents/block/signed";
pub const SIGNED_BLOCK_HTTP_SUFFIX: &str = "/gents/block/signed";

#[derive(Clone, Default)]
pub struct SignedBlockHttpBridge {
    node: Arc<OnceLock<Weak<defra_node::EmbeddedNode>>>,
}

impl SignedBlockHttpBridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build the extra route before constructing the embedded node.
    pub fn router(&self) -> Router {
        Router::new()
            .route(SIGNED_BLOCK_HTTP_PATH, get(signed_block_handler))
            .with_state(self.clone())
    }

    /// Bind the completed node exactly once. A weak reference prevents the
    /// detached Defra HTTP task from extending the database lifetime.
    pub fn bind(&self, node: &Arc<defra_node::EmbeddedNode>) -> anyhow::Result<()> {
        self.node
            .set(Arc::downgrade(node))
            .map_err(|_| anyhow::anyhow!("signed-block HTTP bridge is already bound"))
    }
}

#[derive(Deserialize)]
struct SignedBlockQuery {
    cid: String,
}

#[derive(Serialize)]
struct SignedBlockResponse {
    cid: String,
    block: String,
    signature: String,
}

type HandlerError = (StatusCode, Json<serde_json::Value>);

fn handler_error(status: StatusCode, message: &'static str) -> HandlerError {
    (status, Json(serde_json::json!({ "error": message })))
}

async fn signed_block_handler(
    State(bridge): State<SignedBlockHttpBridge>,
    identity: defra_http::ExtractIdentity,
    Query(query): Query<SignedBlockQuery>,
) -> Result<Json<SignedBlockResponse>, HandlerError> {
    let caller = identity.did().ok_or_else(|| {
        handler_error(
            StatusCode::UNAUTHORIZED,
            "authenticated node identity required",
        )
    })?;
    let cid = query.cid.trim();
    if cid.is_empty() || cid.len() > 512 {
        return Err(handler_error(
            StatusCode::BAD_REQUEST,
            "invalid signed-block CID",
        ));
    }
    let node = bridge.node.get().and_then(Weak::upgrade).ok_or_else(|| {
        handler_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "signed-block export is not ready",
        )
    })?;
    let node_did = node.node_identity_did().ok_or_else(|| {
        handler_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "signed-block export requires a signing node identity",
        )
    })?;
    if caller.as_str() != node_did {
        return Err(handler_error(
            StatusCode::FORBIDDEN,
            "signed-block export is restricted to the node identity",
        ));
    }

    let (block, signature) = node
        .authorized_signed_block_bytes(cid, Some(caller.as_str()))
        .await
        .map_err(|error| {
            tracing::warn!(%error, %cid, caller = %caller, "signed-block export denied");
            handler_error(
                StatusCode::FORBIDDEN,
                "signed-block material is unavailable to this identity",
            )
        })?;
    let encoder = base64::engine::general_purpose::STANDARD;
    Ok(Json(SignedBlockResponse {
        cid: cid.to_string(),
        block: encoder.encode(block),
        signature: encoder.encode(signature),
    }))
}
