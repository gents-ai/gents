use std::pin::Pin;
use std::time::Duration;

use eventsource_stream::{Event, EventStreamError, Eventsource};
use futures::{Stream, StreamExt};
use serde_json::Value;
use tokio::time::Instant;

use super::{ConfigAccess, ConfigClientError};

type Result<T> = std::result::Result<T, ConfigClientError>;
type Events = Pin<
    Box<dyn Stream<Item = std::result::Result<Event, EventStreamError<reqwest::Error>>> + Send>,
>;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a subscription's stream ended without an error
/// (`RuntimeConnectionObservation.StreamEnd`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionEnd {
    /// Half the locally minted bearer lifetime elapsed. The pinned DefraDB
    /// HTTP server authenticates a subscription only at its handshake, so the
    /// stream ends to re-authenticate; it is not a transport failure.
    RenewalDue,
    /// The runtime sent `complete` or closed the response.
    Completed,
}

/// Pinned DefraDB sends `next` GraphQL envelopes and `complete` over SSE; it
/// provides no initial snapshot. Arm this stream before reading a snapshot and
/// repeat that sequence after every disconnect. Dropping it closes the response.
/// Locally minted bearer expiry ends the stream early so its observer can renew
/// authorization and resnapshot; delegated credentials remain caller-owned.
pub struct GraphqlSubscription {
    events: Option<Events>,
    renew_at: Option<Instant>,
    ended: Option<SubscriptionEnd>,
}

impl GraphqlSubscription {
    /// How the stream ended, once `recv` has returned `Ok(None)`.
    pub fn end(&self) -> Option<SubscriptionEnd> {
        self.ended
    }

    pub async fn recv(&mut self) -> Result<Option<Value>> {
        let Some(events) = self.events.as_mut() else {
            return Ok(None);
        };
        let event = match self.renew_at {
            Some(deadline) => tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {
                    self.events = None;
                    self.ended = Some(SubscriptionEnd::RenewalDue);
                    return Ok(None);
                }
                event = events.next() => event,
            },
            None => events.next().await,
        };
        let Some(event) = event else {
            self.events = None;
            self.ended = Some(SubscriptionEnd::Completed);
            return Ok(None);
        };
        let event = match event {
            Ok(event) => event,
            Err(error) => {
                self.events = None;
                return Err(ConfigClientError::SubscriptionStream(error));
            }
        };
        match event.event.as_str() {
            "complete" => {
                self.events = None;
                self.ended = Some(SubscriptionEnd::Completed);
                Ok(None)
            }
            "next" => {
                let value: Value = serde_json::from_str(&event.data)?;
                if super::graphql::graphql_response_has_errors(&value) {
                    self.events = None;
                    return Err(gents_protocol::database_error::GraphqlFailure::new(
                        "GraphQL subscription",
                        value["errors"].clone(),
                    )
                    .into());
                }
                Ok(Some(value))
            }
            _ => {
                self.events = None;
                Err(ConfigClientError::SubscriptionEvent(event.event))
            }
        }
    }
}

impl ConfigAccess {
    /// Uses the same endpoint and actor as ordinary reads. Reconnection belongs
    /// to the observation owner because a gap requires a new complete snapshot.
    pub async fn subscribe(&self, query: &str) -> Result<GraphqlSubscription> {
        let Self::Graphql(endpoint) = self else {
            return Err(ConfigClientError::RemoteSubscriptionRequired);
        };
        if endpoint.is_anonymous() {
            return Err(ConfigClientError::BearerRequired);
        }
        let client = subscription_client().await?;
        let started_at = Instant::now();
        let request = endpoint
            .authorize(client.post(endpoint.url()))?
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .json(&serde_json::json!({"query": query}));
        let response = tokio::time::timeout(HANDSHAKE_TIMEOUT, request.send())
            .await
            .map_err(ConfigClientError::SubscriptionHandshake)??
            .error_for_status()?;
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        if !content_type.as_deref().is_some_and(|value| {
            value
                .split(';')
                .next()
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"))
        }) {
            return Err(ConfigClientError::SubscriptionContentType(content_type));
        }
        let renew_at = endpoint
            .delegated_authorization
            .is_none()
            .then(|| started_at + crate::store::identity::DEFRADB_BEARER_LIFETIME / 2);
        Ok(GraphqlSubscription {
            events: Some(Box::pin(response.bytes_stream().eventsource())),
            renew_at,
            ended: None,
        })
    }
}

async fn subscription_client() -> Result<&'static reqwest::Client> {
    static CLIENT: tokio::sync::OnceCell<reqwest::Client> = tokio::sync::OnceCell::const_new();
    CLIENT
        .get_or_try_init(|| async {
            tokio::task::spawn_blocking(|| {
                reqwest::Client::builder()
                    .connect_timeout(HANDSHAKE_TIMEOUT)
                    .build()
                    .map_err(ConfigClientError::HttpClientBuild)
            })
            .await
            .map_err(ConfigClientError::HttpClientJoin)?
        })
        .await
}

#[cfg(test)]
mod tests;
