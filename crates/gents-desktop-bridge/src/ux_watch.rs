use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gents::store::config_client::GraphqlEndpoint;
use serde::{Deserialize, Serialize};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::tauri_commands::ux_plugins::{UxDataCallRequest, UxDataWatchEvent};

const MAX_EVENT_BYTES: usize = 8 * 1024 * 1024;

/// Lean `ClientShell.PackWatch.maxFailures`.
pub(crate) const MAX_FAILURES: u32 = 5;

/// Why a watch stopped for good (Lean `TerminalReason`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum TerminalReason {
    Auth,
    Permanent,
}

impl TerminalReason {
    fn message(self) -> &'static str {
        match self {
            Self::Auth => {
                "Pack updates are unavailable: this desktop is not authorized to read them. Reopen the page after reconnecting the runtime."
            }
            Self::Permanent => "Pack updates are unavailable. Reopen the page to try again.",
        }
    }
}

/// The watch task's phase (Lean `ClientShell.PackWatch.Phase`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum WatchPhase {
    Starting { failures: u32 },
    Streaming,
    Backoff { failures: u32 },
    Terminal { reason: TerminalReason },
    Cancelled,
}

/// What the task observed (Lean `Event`). A selection change reaches the task
/// as the same token cancellation as an unwatch, so Lean's `nodeDeselected`
/// is `Cancel` here (`node_deselected_is_cancel`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum WatchEvent {
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
pub(crate) enum WatchView {
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

fn next_failures(failures: u32) -> u32 {
    failures.saturating_add(1).min(MAX_FAILURES)
}

/// Lean `delayMs`: the minimum spacing between consecutive attempt starts.
pub(crate) fn backoff_delay_ms(failures: u32) -> u64 {
    1000u64
        .saturating_mul(1u64.checked_shl(failures).unwrap_or(u64::MAX))
        .min(30_000)
}

/// Lean `step`.
pub(crate) fn step(phase: WatchPhase, event: WatchEvent) -> WatchPhase {
    use WatchEvent as E;
    use WatchPhase as P;
    match (phase, event) {
        (P::Terminal { reason }, _) => P::Terminal { reason },
        (P::Cancelled, _) | (_, E::Cancel) => P::Cancelled,
        (P::Starting { .. }, E::Connected) => P::Streaming,
        (P::Starting { failures }, E::CleanEnd | E::TransientError) => P::Backoff {
            failures: next_failures(failures),
        },
        (P::Starting { .. }, E::AuthError) => P::Terminal {
            reason: TerminalReason::Auth,
        },
        (P::Starting { .. } | P::Streaming, E::PermanentError) => P::Terminal {
            reason: TerminalReason::Permanent,
        },
        (P::Streaming, E::CleanEnd) => P::Backoff { failures: 0 },
        (P::Streaming, E::TransientError | E::AuthError) => P::Backoff {
            failures: next_failures(0),
        },
        (P::Backoff { failures }, E::Retry) => P::Starting { failures },
        (phase, _) => phase,
    }
}

/// Lean `view`.
pub(crate) fn view(phase: WatchPhase) -> WatchView {
    match phase {
        WatchPhase::Starting { .. } => WatchView::Connecting,
        WatchPhase::Streaming => WatchView::Live,
        WatchPhase::Backoff { failures } => WatchView::Reconnecting {
            delay_ms: backoff_delay_ms(failures),
        },
        WatchPhase::Terminal { reason } => WatchView::Unavailable { reason },
        WatchPhase::Cancelled => WatchView::Closed,
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

#[derive(Clone, Default)]
pub(crate) struct UxWatchRegistry {
    entries: Arc<Mutex<BTreeMap<String, BTreeMap<String, CancellationToken>>>>,
}

impl UxWatchRegistry {
    pub(crate) fn insert(&self, window: &str, id: &str) -> anyhow::Result<CancellationToken> {
        let mut entries = self.entries.lock().expect("ux watch registry poisoned");
        let window_entries = entries.entry(window.to_owned()).or_default();
        anyhow::ensure!(
            window_entries.len() < 16,
            "too many active pack observations"
        );
        anyhow::ensure!(
            !window_entries.contains_key(id),
            "duplicate pack observation"
        );
        let token = CancellationToken::new();
        window_entries.insert(id.to_owned(), token.clone());
        Ok(token)
    }

    pub(crate) fn cancel(&self, window: &str, id: &str) {
        let mut entries = self.entries.lock().expect("ux watch registry poisoned");
        if let Some(window_entries) = entries.get_mut(window) {
            if let Some(token) = window_entries.remove(id) {
                token.cancel();
            }
            if window_entries.is_empty() {
                entries.remove(window);
            }
        }
    }

    pub(crate) fn cancel_window(&self, window: &str) {
        let mut entries = self.entries.lock().expect("ux watch registry poisoned");
        if let Some(window_entries) = entries.remove(window) {
            for token in window_entries.into_values() {
                token.cancel();
            }
        }
    }

    /// Sends only while the watch is still registered, holding the registry
    /// lock across the send: once a cancellation (unwatch, window destruction,
    /// or the selection change made under the bridge lock) returns, no event
    /// of the cancelled watch reaches the window. TLA `PackUxWatch`
    /// `AtomicDeliver`.
    pub(crate) fn deliver<F>(&self, window: &str, id: &str, send: F) -> bool
    where
        F: FnOnce() -> bool,
    {
        let entries = self.entries.lock().expect("ux watch registry poisoned");
        let live = entries
            .get(window)
            .and_then(|window_entries| window_entries.get(id))
            .is_some_and(|token| !token.is_cancelled());
        live && send()
    }
}

/// Transport bounds for one attempt and the unit that scales the modeled
/// backoff milliseconds.
#[derive(Clone, Copy, Debug)]
pub(crate) struct WatchTiming {
    pub(crate) connect: Duration,
    /// Longer than the runtime's 30 s stream lifetime, so a healthy idle
    /// stream ends cleanly before it times out.
    pub(crate) read: Duration,
    pub(crate) send: Duration,
    pub(crate) backoff_unit: Duration,
}

pub(crate) const WATCH_TIMING: WatchTiming = WatchTiming {
    connect: Duration::from_secs(10),
    read: Duration::from_secs(35),
    send: Duration::from_secs(12),
    backoff_unit: Duration::from_millis(1),
};

impl WatchTiming {
    fn backoff(&self, failures: u32) -> Duration {
        let ms = u32::try_from(backoff_delay_ms(failures)).unwrap_or(u32::MAX);
        self.backoff_unit.saturating_mul(ms)
    }
}

/// Runs one watch until it is cancelled or terminal and returns its final
/// phase. `acquire` is called at the start of every attempt, so a restarted
/// runtime or renewed credential is used by the next connect; it classifies
/// its own failure as transient or permanent. `deliver` returns false once
/// the page can no longer receive events. `observe` sees every transition.
pub(crate) async fn run<A, D, O>(
    timing: WatchTiming,
    mut acquire: A,
    request: &UxDataCallRequest,
    mut deliver: D,
    cancel: &CancellationToken,
    mut observe: O,
) -> WatchPhase
where
    A: FnMut() -> Result<GraphqlEndpoint, WatchEvent>,
    D: FnMut(UxDataWatchEvent) -> bool,
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
            WatchPhase::Streaming => match response.take() {
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
        let next = step(phase, event);
        observe(phase, event, next);
        if let WatchView::Unavailable { reason } = view(next) {
            tracing::warn!(?reason, "pack observation stopped");
            deliver(UxDataWatchEvent::Error {
                message: reason.message().to_owned(),
            });
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

async fn connect<A>(
    client: &reqwest::Client,
    timing: WatchTiming,
    acquire: &mut A,
    request: &UxDataCallRequest,
    cancel: &CancellationToken,
) -> Result<reqwest::Response, WatchEvent>
where
    A: FnMut() -> Result<GraphqlEndpoint, WatchEvent>,
{
    if cancel.is_cancelled() {
        return Err(WatchEvent::Cancel);
    }
    let endpoint = acquire()?;
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

async fn stream<D>(
    mut response: reqwest::Response,
    deliver: &mut D,
    cancel: &CancellationToken,
) -> WatchEvent
where
    D: FnMut(UxDataWatchEvent) -> bool,
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
                    if !deliver(UxDataWatchEvent::Snapshot { value }) {
                        return WatchEvent::Cancel;
                    }
                }
                Err(_) => return WatchEvent::PermanentError,
            }
        }
    }
}

#[cfg(test)]
#[path = "ux_watch/tests.rs"]
mod tests;
