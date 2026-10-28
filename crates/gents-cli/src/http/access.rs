use axum::{
    extract::{Request, State},
    http::{header, uri::Authority, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use defra_http::ExtractIdentity;

#[derive(Clone)]
pub(crate) struct HttpAuthority {
    configured: Option<(String, u16)>,
}

impl HttpAuthority {
    pub(crate) fn for_endpoint(endpoint: &str) -> Self {
        let configured = reqwest::Url::parse(endpoint).ok().and_then(|url| {
            Some((
                url.host_str()?.to_ascii_lowercase(),
                url.port_or_known_default()?,
            ))
        });
        Self { configured }
    }

    pub(crate) fn protect_http(
        self,
        config: gents::defra_node::HttpConfig,
    ) -> gents::defra_node::HttpConfig {
        config.with_router_transform(std::sync::Arc::new(move |router| self.wrap(router)))
    }

    fn wrap(&self, router: axum::Router) -> axum::Router {
        router.layer(axum::middleware::from_fn_with_state(
            self.clone(),
            require_authority,
        ))
    }

    fn accepts(&self, request: &Request) -> bool {
        let Some((configured_host, bound_port)) = &self.configured else {
            return false;
        };
        let mut hosts = request.headers().get_all(header::HOST).iter();
        let host = match hosts.next() {
            Some(value) => value.to_str().ok().and_then(parse_authority),
            None => request
                .uri()
                .authority()
                .and_then(|value| parse_authority(value.as_str())),
        };
        if hosts.next().is_some() {
            return false;
        }
        let Some((host, port)) = host else {
            return false;
        };
        if let Some(authority) = request.uri().authority() {
            if parse_authority(authority.as_str()) != Some((host.clone(), port)) {
                return false;
            }
        }
        port == *bound_port
            && (host == *configured_host
                || matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]"))
    }
}

fn parse_authority(value: &str) -> Option<(String, u16)> {
    if value.contains('@') {
        return None;
    }
    let authority = value.parse::<Authority>().ok()?;
    if authority.host().is_empty() {
        return None;
    }
    let suffix = authority.as_str().strip_prefix(authority.host())?;
    let port = if suffix.is_empty() {
        80
    } else {
        let digits = suffix.strip_prefix(':')?;
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        digits.parse::<u16>().ok()?
    };
    Some((authority.host().to_ascii_lowercase(), port))
}

/// A browser's same-origin check does not prevent DNS rebinding. Compare the
/// actual request authority without consulting DNS or proxy-supplied headers.
async fn require_authority(
    State(authority): State<HttpAuthority>,
    request: Request,
    next: Next,
) -> Response {
    if !authority.accepts(&request) {
        return failure(
            StatusCode::MISDIRECTED_REQUEST,
            "This authority does not serve the runtime.",
        );
    }
    next.run(request).await
}

/// These handlers use the node's own database principal, so an arbitrary
/// authenticated DID is insufficient. DefraDB owns bearer verification.
pub(crate) async fn require_operator(
    State(node_did): State<String>,
    identity: ExtractIdentity,
    request: Request,
    next: Next,
) -> Response {
    match identity.did() {
        None => failure(
            StatusCode::UNAUTHORIZED,
            "Runtime operator authorization is required.",
        ),
        Some(actor) if actor.as_ref() != node_did => failure(
            StatusCode::FORBIDDEN,
            "This identity does not own the runtime.",
        ),
        Some(_) => {
            let mut response = next.run(request).await;
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("no-store"),
            );
            response
        }
    }
}

fn failure(status: StatusCode, error: &str) -> Response {
    let mut response = (status, axum::Json(serde_json::json!({ "error": error }))).into_response();
    if status == StatusCode::UNAUTHORIZED {
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            header::HeaderValue::from_static("Bearer"),
        );
    }
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

#[cfg(test)]
#[path = "access_tests.rs"]
mod tests;
