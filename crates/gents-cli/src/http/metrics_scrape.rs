//! The dedicated Prometheus scrape listener. Its access decisions are the
//! executable `RuntimeReconcile.MetricsScrapeAccess` model; this module only
//! classifies sockets and requests into that model's inputs.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::Next,
    response::Response,
    Router,
};
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tokio::sync::watch;

use crate::http::access::{failure, HttpAuthority};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrapeBind {
    Loopback,
    Interface,
    Unspecified,
}

impl ScrapeBind {
    pub(crate) fn classify(ip: IpAddr) -> Self {
        if ip.is_unspecified() {
            Self::Unspecified
        } else if ip.is_loopback() {
            Self::Loopback
        } else {
            Self::Interface
        }
    }

    pub(crate) fn allowed(self, token_configured: bool) -> bool {
        match self {
            Self::Unspecified => false,
            Self::Loopback => true,
            Self::Interface => token_configured,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrapeDecision {
    Misdirected,
    Unauthorized,
    Metrics,
}

pub(crate) fn decide(
    authority_matches: bool,
    token_configured: bool,
    token_presented: bool,
) -> ScrapeDecision {
    if !authority_matches {
        ScrapeDecision::Misdirected
    } else if token_configured && !token_presented {
        ScrapeDecision::Unauthorized
    } else {
        ScrapeDecision::Metrics
    }
}

pub(crate) fn validate_listen_addr(addr: SocketAddr, token: Option<&str>) -> Result<()> {
    match ScrapeBind::classify(addr.ip()) {
        bind if bind.allowed(token.is_some()) => Ok(()),
        ScrapeBind::Unspecified => anyhow::bail!(
            "refusing to serve metrics on unspecified address {addr}; bind loopback or a specific interface instead"
        ),
        _ => anyhow::bail!(
            "refusing to serve unauthenticated metrics on {addr}; set --metrics-auth-token-env or bind loopback"
        ),
    }
}

#[derive(Clone)]
struct ScrapeAccess {
    authority: HttpAuthority,
    token: Option<Arc<str>>,
}

fn token_presented(headers: &HeaderMap, expected: &str) -> bool {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let presented = values
        .next()
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|provided| bool::from(expected.as_bytes().ct_eq(provided.as_bytes())));
    presented && values.next().is_none()
}

async fn require_scrape_access(
    State(access): State<ScrapeAccess>,
    request: Request,
    next: Next,
) -> Response {
    let token = access.token.as_deref();
    let decision = decide(
        access.authority.accepts(&request),
        token.is_some(),
        token.is_some_and(|expected| token_presented(request.headers(), expected)),
    );
    match decision {
        ScrapeDecision::Misdirected => failure(
            StatusCode::MISDIRECTED_REQUEST,
            "This authority does not serve gents metrics.",
        ),
        ScrapeDecision::Unauthorized => failure(
            StatusCode::UNAUTHORIZED,
            "Metrics scrape authorization is required.",
        ),
        ScrapeDecision::Metrics => {
            let mut response = next.run(request).await;
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("no-store"),
            );
            response
        }
    }
}

/// Applies the scrape access decision to every route of `router`.
pub(crate) fn protect(router: Router, addr: SocketAddr, token: Option<String>) -> Router {
    let access = ScrapeAccess {
        authority: HttpAuthority::for_endpoint(&format!("http://{addr}")),
        token: token.map(Arc::from),
    };
    router.layer(axum::middleware::from_fn_with_state(
        access,
        require_scrape_access,
    ))
}

pub(crate) struct MetricsScrapeListener {
    listener: TcpListener,
    addr: SocketAddr,
    token: Option<String>,
}

impl MetricsScrapeListener {
    /// Binds before the node opens so a bad address fails `serve` at once.
    pub(crate) async fn bind(addr: SocketAddr, token: Option<String>) -> Result<Self> {
        validate_listen_addr(addr, token.as_deref())?;
        let listener = TcpListener::bind(addr)
            .await
            .with_context(|| format!("binding metrics listener on {addr}"))?;
        let addr = listener
            .local_addr()
            .context("reading metrics listener address")?;
        Ok(Self {
            listener,
            addr,
            token,
        })
    }

    pub(crate) fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub(crate) fn auth_required(&self) -> bool {
        self.token.is_some()
    }

    pub(crate) fn spawn(
        self,
        state: crate::http::router::RuntimeHttpState,
        mut shutdown: watch::Receiver<bool>,
    ) -> tokio::task::JoinHandle<Result<()>> {
        let router = protect(
            crate::http::router::metrics_scrape_router(state),
            self.addr,
            self.token,
        );
        tokio::spawn(async move {
            axum::serve(self.listener, router)
                .with_graceful_shutdown(async move {
                    let _ = shutdown.wait_for(|stopping| *stopping).await;
                })
                .await
                .context("serving metrics listener")
                .inspect_err(|error| tracing::warn!("metrics listener stopped: {error:#}"))
        })
    }
}

#[cfg(test)]
#[path = "metrics_scrape_tests.rs"]
mod tests;
