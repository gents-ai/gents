use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Router,
};
use futures_util::{future::BoxFuture, StreamExt};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::watch;

/// Ends the runtime's server-sent event responses once serve shutdown begins.
///
/// External premise: the pinned DefraDB ends its GraphQL subscription and event
/// streams when `Node::shutdown` starts closing the node's request scope,
/// but serve shutdown begins earlier: the runtime task is joined first. The
/// runtime's own event streams (the pack UX watch) belong to no DefraDB
/// request scope, so DefraDB's drain would wait its network drain timeout for
/// them. Ending every stream on the serve shutdown signal lets them finish
/// before the runtime stops; their subscribers (a remote observer arms one
/// per observed collection) resubscribe and resnapshot as after any stream
/// end, and a stream refused during shutdown fails as unavailable.
///
/// It also bounds a GraphQL subscription by its subscriber's authority
/// (`tla/SubscriptionStreamEnd.tla`). The pinned DefraDB checks node read
/// permission only at a subscription's handshake and then delivers every event
/// as that identity, so a captured bearer, or a subscriber whose permission
/// was revoked, could hold a stream open indefinitely. The stream ends at its
/// bearer's expiry, and before each event the subscriber must still hold node
/// read permission; at the first event it does not, nothing more is delivered.
/// Either way the last event, `gents-end`, names why: `expired` (renew) or
/// `revoked` (stop).
#[derive(Clone)]
pub struct ShutdownStreams {
    shutdown: watch::Receiver<bool>,
    member: Arc<OnceLock<MemberCheck>>,
}

/// What a check of a DID's node read permission said. The node supplies the
/// check once it has started; until then only the handshake's check applies.
pub type MemberCheck = Arc<dyn Fn(String) -> BoxFuture<'static, PermissionRead> + Send + Sync>;

/// What the node's ACP said of a subscriber's node read permission (Lean
/// `SubscriptionStream.PermissionRead`; a stream without a check is `None`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionRead {
    Held,
    Lost,
    /// The check failed.
    Unreadable,
}

/// Whether a subscription's next event is delivered, or how the stream ends
/// (Lean `SubscriptionStream.gate`).
fn event_gate(read: Option<PermissionRead>) -> Option<EndReason> {
    match read {
        None | Some(PermissionRead::Held) => None,
        Some(PermissionRead::Lost) => Some(EndReason::Revoked),
        Some(PermissionRead::Unreadable) => Some(EndReason::Expired),
    }
}

/// The path of DefraDB's GraphQL route, whose event streams are
/// subscriptions authorized by a bearer.
const GRAPHQL_PATH_SUFFIX: &str = "/graphql";

impl ShutdownStreams {
    /// `shutdown` turns `true` (or its sender drops) when serve shutdown
    /// begins.
    pub fn new(shutdown: watch::Receiver<bool>) -> Self {
        Self {
            shutdown,
            member: Arc::new(OnceLock::new()),
        }
    }

    /// From now on, each subscription event is delivered only while its
    /// subscriber holds node read permission.
    pub fn admit_members_with(&self, check: MemberCheck) {
        let _ = self.member.set(check);
    }

    pub fn wrap(&self, router: Router) -> Router {
        router.layer(axum::middleware::from_fn_with_state(
            self.clone(),
            bound_event_streams,
        ))
    }

    fn begun(&self) -> bool {
        *self.shutdown.borrow() || self.shutdown.has_changed().is_err()
    }

    fn begins(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let mut shutdown = self.shutdown.clone();
        async move {
            // A dropped sender means serve is gone: the stream ends either way.
            let _ = shutdown.wait_for(|begun| *begun).await;
        }
    }
}

async fn bound_event_streams(
    State(streams): State<ShutdownStreams>,
    request: Request,
    next: Next,
) -> Response {
    if streams.begun() && names_event_stream(request.headers(), header::ACCEPT) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the runtime is shutting down",
        )
            .into_response();
    }
    let subscription = request
        .uri()
        .path()
        .ends_with(GRAPHQL_PATH_SUFFIX)
        .then(|| bearer_claims(request.headers()))
        .flatten();
    let response = next.run(request).await;
    if !names_event_stream(response.headers(), header::CONTENT_TYPE) {
        return response;
    }
    let (parts, body) = response.into_parts();
    let events = body.into_data_stream().take_until(streams.begins());
    let Some(bearer) = subscription else {
        return Response::from_parts(parts, Body::from_stream(events));
    };
    let member = streams.member.get().cloned();
    Response::from_parts(parts, Body::from_stream(bounded(events, bearer, member)))
}

/// Why the runtime ended a subscription, sent as its last event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EndReason {
    /// The bearer it was opened with expired, or its subscriber's permission
    /// could not be read: the subscriber renews, and the renewal's handshake
    /// decides.
    Expired,
    /// Its subscriber lost node read permission: a refusal.
    Revoked,
}

impl EndReason {
    fn event(self) -> axum::body::Bytes {
        let reason = match self {
            Self::Expired => "expired",
            Self::Revoked => "revoked",
        };
        axum::body::Bytes::from(format!(
            "event: gents-end\ndata: {{\"reason\":\"{reason}\"}}\n\n"
        ))
    }
}

/// `events` until the bearer expires or, before an event, its subscriber no
/// longer holds node read permission; then the reason, as a last event.
fn bounded<S>(
    events: S,
    bearer: Bearer,
    member: Option<MemberCheck>,
) -> impl futures_util::Stream<Item = Result<axum::body::Bytes, axum::Error>> + Send + 'static
where
    S: futures_util::Stream<Item = Result<axum::body::Bytes, axum::Error>> + Send + 'static,
{
    let reason: Arc<Mutex<Option<EndReason>>> = Arc::default();
    let expiry = {
        let reason = reason.clone();
        async move {
            tokio::time::sleep(bearer.expires_in).await;
            *reason
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(EndReason::Expired);
        }
    };
    let gate = member.zip(bearer.subscriber);
    let revoked = reason.clone();
    let delivered = futures_util::stream::unfold(
        (Box::pin(events), gate, revoked),
        |(mut events, gate, revoked)| async move {
            let event = events.next().await?;
            let read = match &gate {
                Some((member, did)) => Some(member(did.clone()).await),
                None => None,
            };
            if let Some(end) = event_gate(read) {
                *revoked
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(end);
                return None;
            }
            Some((event, (events, gate, revoked)))
        },
    )
    .take_until(expiry);
    let last = futures_util::stream::once(async move {
        reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .map(|reason| Ok(reason.event()))
    })
    .filter_map(std::future::ready);
    delivered.chain(last)
}

/// A subscription's bearer as the handshake presented it.
struct Bearer {
    expires_in: std::time::Duration,
    /// Its issuer, the subscriber's DID.
    subscriber: Option<String>,
}

/// The bearer's `exp` and `iss` claims. DefraDB has verified the bearer by
/// the time the stream exists, so only the claims are read here; a header
/// that is not a JWT bearer sets no bound.
fn bearer_claims(headers: &HeaderMap) -> Option<Bearer> {
    use base64::Engine as _;
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let token = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))?;
    let claims = token.split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(claims.trim_end_matches('='))
        .ok()?;
    let claims = serde_json::from_slice::<serde_json::Value>(&decoded).ok()?;
    let exp = claims.get("exp")?.as_i64()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let left = u64::try_from(exp).ok()?.saturating_sub(now);
    Some(Bearer {
        expires_in: std::time::Duration::from_secs(left),
        subscriber: claims
            .get("iss")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
    })
}

fn names_event_stream(headers: &HeaderMap, name: header::HeaderName) -> bool {
    headers
        .get_all(name)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| value.to_ascii_lowercase().contains("text/event-stream"))
}

#[cfg(test)]
#[path = "shutdown_streams_tests.rs"]
mod tests;
