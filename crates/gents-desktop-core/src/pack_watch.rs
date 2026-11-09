//! A pack page's live observation of one datastore tool: the client side of
//! the runtime's `POST /packs/ux/watch` server-sent events
//! (`gents-cli::http::pack_watch::watch`). Lean `ClientShell.PackWatch` owns
//! the phases; TLA `PackUxWatch` owns the races with the desktop shell, whose
//! window registry and IPC channel stay in the Tauri bridge.

use std::time::Duration;

use gents::store::config_client::GraphqlEndpoint;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::client::LocalHomeAccessError;

pub const MAX_EVENT_BYTES: usize = 8 * 1024 * 1024;

/// Lean `ClientShell.PackWatch.retryBudget`.
pub const RETRY_BUDGET: u32 = 8;

/// Why a watch stopped for good (Lean `TerminalReason`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TerminalReason {
    Auth,
    Permanent,
    Unreachable,
}

impl TerminalReason {
    pub fn message(self) -> &'static str {
        match self {
            Self::Auth => {
                "Pack updates are unavailable: this desktop is not authorized to read them. Reopen the page after reconnecting the runtime."
            }
            Self::Permanent => "Pack updates are unavailable. Reopen the page to try again.",
            Self::Unreachable => {
                "Pack updates stopped: the runtime has not answered for about two minutes. Reopen the page once it is running."
            }
        }
    }
}

/// The watch task's phase (Lean `ClientShell.PackWatch.Phase`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum WatchPhase {
    Starting { failures: u32 },
    Streaming { failures: u32 },
    Backoff { failures: u32 },
    Terminal { reason: TerminalReason },
    Cancelled,
}

impl WatchPhase {
    pub fn is_final(self) -> bool {
        matches!(self, Self::Terminal { .. } | Self::Cancelled)
    }
}

/// What the task observed (Lean `Event`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WatchEvent {
    Connected,
    CleanEnd,
    TransientError,
    AuthError,
    PermanentError,
    Cancel,
    Retry,
}

/// What the page shows (Lean `View`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum WatchView {
    Connecting,
    Live,
    Reconnecting {
        #[serde(rename = "delayMs")]
        delay_ms: u64,
    },
    Unavailable {
        reason: TerminalReason,
    },
    Closed,
}

/// What the task sends the page on entering a phase (Lean `Notice`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum WatchNotice {
    Reconnecting {
        #[serde(rename = "delayMs")]
        delay_ms: u64,
    },
    Unavailable {
        reason: TerminalReason,
    },
}

/// One thing the task hands the page's channel.
#[derive(Debug, PartialEq)]
pub enum WatchDelivery<T> {
    Snapshot(T),
    Notice(WatchNotice),
}

/// Why an attempt could not acquire the runtime endpoint. The variant is the
/// classification: a client core that is being replaced heals on the next
/// attempt; a runtime record this desktop cannot reach does not.
#[derive(Debug, thiserror::Error)]
pub enum WatchAcquireError {
    #[error("the desktop client is not started")]
    ClientRestarting,
    #[error(transparent)]
    Access(#[from] LocalHomeAccessError),
}

impl WatchAcquireError {
    fn event(&self) -> WatchEvent {
        match self {
            Self::ClientRestarting => WatchEvent::TransientError,
            Self::Access(_) => WatchEvent::PermanentError,
        }
    }
}

/// Lean `delayMs`: the minimum spacing between consecutive attempt starts.
pub fn backoff_delay_ms(failures: u32) -> u64 {
    1000u64
        .saturating_mul(1u64.checked_shl(failures).unwrap_or(u64::MAX))
        .min(30_000)
}

/// Lean `fail`.
fn fail(failures: u32) -> WatchPhase {
    if failures.saturating_add(1) < RETRY_BUDGET {
        WatchPhase::Backoff {
            failures: failures + 1,
        }
    } else {
        WatchPhase::Terminal {
            reason: TerminalReason::Unreachable,
        }
    }
}

/// Lean `step`.
pub fn step(phase: WatchPhase, event: WatchEvent) -> WatchPhase {
    use WatchEvent as E;
    use WatchPhase as P;
    match (phase, event) {
        (P::Terminal { reason }, _) => P::Terminal { reason },
        (P::Cancelled, _) | (_, E::Cancel) => P::Cancelled,
        (P::Starting { failures }, E::Connected) => P::Streaming { failures },
        (P::Starting { failures }, E::CleanEnd | E::TransientError) => fail(failures),
        (P::Starting { .. }, E::AuthError) => P::Terminal {
            reason: TerminalReason::Auth,
        },
        (P::Starting { .. } | P::Streaming { .. }, E::PermanentError) => P::Terminal {
            reason: TerminalReason::Permanent,
        },
        (P::Streaming { .. }, E::CleanEnd) => P::Backoff { failures: 0 },
        (P::Streaming { failures }, E::TransientError | E::AuthError) => fail(failures),
        (P::Backoff { failures }, E::Retry) => P::Starting { failures },
        (phase, _) => phase,
    }
}

/// Lean `view`.
pub fn view(phase: WatchPhase) -> WatchView {
    match phase {
        WatchPhase::Starting { .. } => WatchView::Connecting,
        WatchPhase::Streaming { .. } => WatchView::Live,
        WatchPhase::Backoff { failures } => WatchView::Reconnecting {
            delay_ms: backoff_delay_ms(failures),
        },
        WatchPhase::Terminal { reason } => WatchView::Unavailable { reason },
        WatchPhase::Cancelled => WatchView::Closed,
    }
}

/// Lean `notice`.
pub fn notice(phase: WatchPhase) -> Option<WatchNotice> {
    match phase {
        WatchPhase::Backoff { failures } if failures > 0 => Some(WatchNotice::Reconnecting {
            delay_ms: backoff_delay_ms(failures),
        }),
        WatchPhase::Terminal { reason } => Some(WatchNotice::Unavailable { reason }),
        _ => None,
    }
}

/// One decoded server-sent event.
#[derive(Debug, PartialEq)]
pub(crate) enum Frame {
    Next(serde_json::Value),
    /// The runtime's `error` event: its watcher failed and it asks the client
    /// to reconnect (`pack_watch::watch`).
    ServerError,
}

#[derive(Debug)]
pub(crate) enum FrameError {
    TooLarge,
    NotUtf8,
    Json,
    Unexpected,
}

/// Takes the next complete event from `buffer`. `scanned` is how much of the
/// buffer earlier calls already searched for a delimiter, so a large event
/// arriving in many chunks is searched once.
pub(crate) fn take_event(
    buffer: &mut Vec<u8>,
    scanned: &mut usize,
) -> Result<Option<Frame>, FrameError> {
    let from = scanned.saturating_sub(3).min(buffer.len());
    let delimiter = [
        buffer[from..]
            .windows(2)
            .position(|part| part == b"\n\n")
            .map(|at| (from + at, 2)),
        buffer[from..]
            .windows(4)
            .position(|part| part == b"\r\n\r\n")
            .map(|at| (from + at, 4)),
    ]
    .into_iter()
    .flatten()
    .min_by_key(|(at, _)| *at);
    let Some((end, width)) = delimiter else {
        *scanned = buffer.len();
        if buffer.len() > MAX_EVENT_BYTES {
            return Err(FrameError::TooLarge);
        }
        return Ok(None);
    };
    if end > MAX_EVENT_BYTES {
        return Err(FrameError::TooLarge);
    }
    *scanned = 0;
    let bytes: Vec<_> = buffer.drain(..end + width).collect();
    let text = std::str::from_utf8(&bytes[..end]).map_err(|_| FrameError::NotUtf8)?;
    let mut kind = "message";
    let mut data = Vec::new();
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("event:") {
            kind = value.trim();
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.strip_prefix(' ').unwrap_or(value));
        }
    }
    match kind {
        "next" => serde_json::from_str(&data.join("\n"))
            .map(|value| Some(Frame::Next(value)))
            .map_err(|_| FrameError::Json),
        "error" => Ok(Some(Frame::ServerError)),
        _ => Err(FrameError::Unexpected),
    }
}

/// Transport bounds for one attempt and the unit that scales the modeled
/// backoff milliseconds.
#[derive(Clone, Copy, Debug)]
pub struct WatchTiming {
    pub connect: Duration,
    /// Longer than the runtime's 30 s stream lifetime, so a healthy idle
    /// stream ends cleanly before it times out.
    pub read: Duration,
    pub send: Duration,
    pub backoff_unit: Duration,
}

pub const WATCH_TIMING: WatchTiming = WatchTiming {
    connect: Duration::from_secs(10),
    read: Duration::from_secs(35),
    send: Duration::from_secs(12),
    backoff_unit: Duration::from_millis(1),
};

impl WatchTiming {
    pub fn backoff(&self, failures: u32) -> Duration {
        let ms = u32::try_from(backoff_delay_ms(failures)).unwrap_or(u32::MAX);
        self.backoff_unit.saturating_mul(ms)
    }
}

/// Runs one watch until it is cancelled or terminal and returns its final
/// phase. `acquire` is called at the start of every attempt, so a restarted
/// runtime or renewed credential is used by the next connect. `deliver`
/// receives each snapshot and each notice (Lean `notice`) and returns false
/// once the page can no longer receive them, which cancels the watch.
/// `observe` sees every transition.
pub async fn run<T, R, A, D, O>(
    timing: WatchTiming,
    mut acquire: A,
    request: &R,
    mut deliver: D,
    cancel: &CancellationToken,
    mut observe: O,
) -> WatchPhase
where
    T: DeserializeOwned,
    R: Serialize,
    A: FnMut() -> Result<GraphqlEndpoint, WatchAcquireError>,
    D: FnMut(WatchDelivery<T>) -> bool,
    O: FnMut(WatchPhase, WatchEvent, WatchPhase),
{
    let client = reqwest::Client::builder()
        .connect_timeout(timing.connect)
        .read_timeout(timing.read)
        .build();
    let mut phase = WatchPhase::Starting { failures: 0 };
    let mut response = None;
    let mut attempt_started = Instant::now();
    loop {
        let event = match phase {
            WatchPhase::Terminal { .. } | WatchPhase::Cancelled => return phase,
            WatchPhase::Starting { .. } => {
                attempt_started = Instant::now();
                match &client {
                    Ok(client) => {
                        match connect(client, timing, &mut acquire, request, cancel).await {
                            Ok(accepted) => {
                                response = Some(accepted);
                                WatchEvent::Connected
                            }
                            Err(event) => event,
                        }
                    }
                    Err(_) => WatchEvent::PermanentError,
                }
            }
            WatchPhase::Streaming { .. } => match response.take() {
                Some(accepted) => stream(accepted, &mut deliver, cancel).await,
                None => WatchEvent::TransientError,
            },
            WatchPhase::Backoff { failures } => {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => WatchEvent::Cancel,
                    _ = tokio::time::sleep_until(attempt_started + timing.backoff(failures)) => {
                        WatchEvent::Retry
                    }
                }
            }
        };
        let mut next = step(phase, event);
        observe(phase, event, next);
        if let Some(sent) = notice(next) {
            if let WatchNotice::Unavailable { reason } = sent {
                tracing::warn!(?reason, "pack observation stopped");
            }
            if !deliver(WatchDelivery::Notice(sent)) && !next.is_final() {
                let closed = step(next, WatchEvent::Cancel);
                observe(next, WatchEvent::Cancel, closed);
                next = closed;
            }
        }
        phase = next;
    }
}

fn watch_url(endpoint: &GraphqlEndpoint) -> Result<reqwest::Url, WatchEvent> {
    let mut url = reqwest::Url::parse(endpoint.url()).map_err(|_| WatchEvent::PermanentError)?;
    url.set_path("/packs/ux/watch");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

async fn connect<A, R>(
    client: &reqwest::Client,
    timing: WatchTiming,
    acquire: &mut A,
    request: &R,
    cancel: &CancellationToken,
) -> Result<reqwest::Response, WatchEvent>
where
    A: FnMut() -> Result<GraphqlEndpoint, WatchAcquireError>,
    R: Serialize,
{
    if cancel.is_cancelled() {
        return Err(WatchEvent::Cancel);
    }
    let endpoint = acquire().map_err(|error| {
        tracing::debug!(error = %error, "pack observation could not acquire its runtime");
        error.event()
    })?;
    let url = watch_url(&endpoint)?;
    let builder = endpoint
        .authorize(client.post(url).json(request))
        .map_err(|_| WatchEvent::PermanentError)?;
    let response = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(WatchEvent::Cancel),
        result = tokio::time::timeout(timing.send, builder.send()) => match result {
            Ok(Ok(response)) => response,
            Ok(Err(_)) | Err(_) => return Err(WatchEvent::TransientError),
        },
    };
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    Err(match status.as_u16() {
        401 | 403 => WatchEvent::AuthError,
        408 | 425 | 429 | 500..=599 => WatchEvent::TransientError,
        _ => WatchEvent::PermanentError,
    })
}

async fn stream<T, D>(
    mut response: reqwest::Response,
    deliver: &mut D,
    cancel: &CancellationToken,
) -> WatchEvent
where
    T: DeserializeOwned,
    D: FnMut(WatchDelivery<T>) -> bool,
{
    let mut buffer = Vec::new();
    let mut scanned = 0;
    loop {
        let next = tokio::select! {
            biased;
            _ = cancel.cancelled() => return WatchEvent::Cancel,
            result = response.chunk() => result,
        };
        let chunk = match next {
            Ok(Some(chunk)) => chunk,
            // A stream cut inside an event is a broken connection.
            Ok(None) if !buffer.is_empty() => return WatchEvent::TransientError,
            Ok(None) => return WatchEvent::CleanEnd,
            Err(_) => return WatchEvent::TransientError,
        };
        buffer.extend_from_slice(&chunk);
        loop {
            match take_event(&mut buffer, &mut scanned) {
                Ok(None) => break,
                Ok(Some(Frame::ServerError)) => return WatchEvent::TransientError,
                Ok(Some(Frame::Next(value))) => {
                    let Ok(value) = serde_json::from_value(value) else {
                        return WatchEvent::PermanentError;
                    };
                    if !deliver(WatchDelivery::Snapshot(value)) {
                        return WatchEvent::Cancel;
                    }
                }
                Err(_) => return WatchEvent::PermanentError,
            }
        }
    }
}

#[cfg(test)]
#[path = "pack_watch/tests.rs"]
mod tests;
