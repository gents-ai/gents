use anyhow::{Context, Result};
use axum::{
    http::{header, HeaderMap, StatusCode},
    response::Response,
};
use defra_http::ExtractIdentity;
use gents::store::config_client::{ConfigAccess, GraphqlEndpoint};

use super::access::failure;
use super::router::RuntimeHttpState;

/// HttpAccess.lean `readsAs`: a private runtime snapshot is read with the
/// caller's own forwarded bearer, never the served Node's principal. The
/// caller must be the served Node or a DID native NAC reports as its owner or
/// `admin`; the admin observation is made as the caller and confers no
/// authority beyond what native ACP then applies to the caller's reads.
pub(crate) async fn authorize_private_reader(
    state: &RuntimeHttpState,
    identity: &ExtractIdentity,
    headers: &HeaderMap,
) -> std::result::Result<GraphqlEndpoint, Response> {
    let Some(actor) = identity.did() else {
        return Err(failure(
            StatusCode::UNAUTHORIZED,
            "Runtime operator authorization is required.",
        ));
    };
    let forbidden = || {
        failure(
            StatusCode::FORBIDDEN,
            "This identity does not own the runtime.",
        )
    };
    let Ok(actor) = identity::Did::new(actor.as_ref()) else {
        return Err(forbidden());
    };
    let endpoint = match caller_endpoint(state, headers, actor.clone()) {
        Ok(endpoint) => endpoint,
        Err(error) => {
            tracing::debug!(error = %format!("{error:#}"), "private reader bearer unusable");
            return Err(forbidden());
        }
    };
    if actor.as_str() == state.node_did {
        return Ok(endpoint);
    }
    match ConfigAccess::Graphql(endpoint.clone())
        .actor_is_native_admin()
        .await
    {
        Ok(true) => Ok(endpoint),
        Ok(false) => Err(forbidden()),
        Err(error) => {
            tracing::debug!(error = %error, "native admin observation refused");
            Err(forbidden())
        }
    }
}

/// HttpAuthority has already restricted Host to this runtime. Preserve that
/// authority when forwarding the audience-bound bearer; use neither proxy
/// headers nor a new key.
pub(crate) fn caller_endpoint(
    state: &RuntimeHttpState,
    headers: &HeaderMap,
    actor: identity::Did,
) -> Result<GraphqlEndpoint> {
    let authorization = headers
        .get(header::AUTHORIZATION)
        .context("caller bearer is missing")?
        .to_str()?;
    let host = headers
        .get(header::HOST)
        .context("caller authority is missing")?
        .to_str()?;
    let configured = reqwest::Url::parse(state.p2p_graphql.url())?;
    let mut endpoint = reqwest::Url::parse(&format!("{}://{host}", configured.scheme()))?;
    endpoint.set_path(configured.path());
    Ok(GraphqlEndpoint::with_authenticated_delegation(
        endpoint.to_string(),
        authorization,
        actor,
    )?)
}

/// Mark a private response as uncacheable, as the operator layer does.
pub(crate) fn private(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}
