use axum::extract::{FromRequestParts as _, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};

use super::{edge, DocumentAcp, Presented};
use crate::config_client::user_write::Author;

/// The runtime's HTTP edge on a home whose document access control is
/// `provider` (`Identity.UserDelegation.edge`).
#[derive(Clone)]
pub struct HttpEdge {
    provider: DocumentAcp,
    node_did: String,
}

impl HttpEdge {
    pub fn new(provider: DocumentAcp, node_did: impl Into<String>) -> Self {
        Self {
            provider,
            node_did: node_did.into(),
        }
    }

    pub fn wrap(&self, router: axum::Router) -> axum::Router {
        router.layer(axum::middleware::from_fn_with_state(
            self.clone(),
            refuse_user_identity_tokens,
        ))
    }
}

/// External premise: DefraDB's `ExtractIdentity`, outside its own auth
/// middleware, names a caller only for a valid identity token; a Vera
/// delegation is not one, and is judged by DefraDB after this edge.
async fn refuse_user_identity_tokens(
    State(edge_state): State<HttpEdge>,
    request: Request,
    next: Next,
) -> Response {
    if edge_state.provider.is_local() {
        return next.run(request).await;
    }
    let (mut parts, body) = request.into_parts();
    let caller = defra_http::ExtractIdentity::from_request_parts(&mut parts, &())
        .await
        .ok()
        .and_then(defra_http::ExtractIdentity::into_did);
    let request = Request::from_parts(parts, body);
    let Some(caller) = caller else {
        return next.run(request).await;
    };
    let author = if caller.as_str() == edge_state.node_did {
        Author::Node
    } else {
        Author::User
    };
    match edge(edge_state.provider, author, Presented::IdentityToken) {
        Ok(()) => next.run(request).await,
        Err(refusal) => (
            StatusCode::UNAUTHORIZED,
            [
                (header::WWW_AUTHENTICATE, "Bearer"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            axum::Json(serde_json::json!({
                "code": gents_protocol::delegation_audience::DELEGATION_REQUIRED_CODE,
                "error": refusal.to_string(),
            })),
        )
            .into_response(),
    }
}
