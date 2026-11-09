use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::*;

const TIMING: WatchTiming = WatchTiming {
    connect: Duration::from_secs(5),
    read: Duration::from_millis(300),
    send: Duration::from_secs(5),
    backoff_unit: Duration::from_micros(20),
};

type Transition = (WatchPhase, WatchEvent, WatchPhase);

fn lean(input: &str) -> String {
    gents_lean_contract::run_prepared_generator_with_input(
        &gents_lean_contract::proofs_dir().unwrap(),
        "Proofs.Conformance.ClientPackWatch:olean",
        "Proofs/Conformance/ClientPackWatch.lean",
        input,
    )
    .unwrap()
}

/// Executes the Lean owner on observed or enumerated phase/event pairs,
/// checks the native step, view and notice against it, and answers the
/// notices the model sends for them, in order.
fn assert_modeled(transitions: &[Transition]) -> Vec<WatchNotice> {
    assert!(!transitions.is_empty());
    let inputs: Vec<_> = transitions
        .iter()
        .map(|(phase, event, _)| serde_json::json!({"phase": phase, "event": event}))
        .collect();
    let output = lean(&serde_json::to_string(&inputs).unwrap());
    let modeled: Vec<serde_json::Value> = serde_json::from_str(output.trim()).unwrap();
    assert_eq!(modeled.len(), transitions.len());
    let mut notices = Vec::new();
    for (expected, (phase, event, next)) in modeled.into_iter().zip(transitions) {
        let modeled_next: WatchPhase = serde_json::from_value(expected["next"].clone()).unwrap();
        let modeled_view: WatchView = serde_json::from_value(expected["view"].clone()).unwrap();
        let modeled_notice: Option<WatchNotice> =
            serde_json::from_value(expected["notice"].clone()).unwrap();
        assert_eq!(*next, modeled_next, "{phase:?} --{event:?}-->");
        assert_eq!(view(*next), modeled_view, "{phase:?} --{event:?}-->");
        assert_eq!(notice(*next), modeled_notice, "{phase:?} --{event:?}-->");
        if next != phase {
            notices.extend(modeled_notice);
        }
    }
    notices
}

async fn assert_modeled_async(transitions: Vec<Transition>) -> Vec<WatchNotice> {
    tokio::task::spawn_blocking(move || assert_modeled(&transitions))
        .await
        .unwrap()
}

const EVENTS: [WatchEvent; 7] = [
    WatchEvent::Connected,
    WatchEvent::CleanEnd,
    WatchEvent::TransientError,
    WatchEvent::AuthError,
    WatchEvent::PermanentError,
    WatchEvent::Cancel,
    WatchEvent::Retry,
];

#[test]
fn every_phase_and_event_steps_as_the_model() {
    let mut phases = vec![WatchPhase::Cancelled];
    for reason in [
        TerminalReason::Auth,
        TerminalReason::Permanent,
        TerminalReason::Unreachable,
    ] {
        phases.push(WatchPhase::Terminal { reason });
    }
    for failures in 0..RETRY_BUDGET {
        phases.push(WatchPhase::Starting { failures });
        phases.push(WatchPhase::Streaming { failures });
        phases.push(WatchPhase::Backoff { failures });
    }
    let transitions: Vec<_> = phases
        .iter()
        .flat_map(|phase| {
            EVENTS
                .iter()
                .map(move |event| (*phase, *event, step(*phase, *event)))
        })
        .collect();
    assert_eq!(transitions.len(), (4 + 3 * RETRY_BUDGET as usize) * EVENTS.len());
    assert_modeled(&transitions);
}

#[test]
fn the_retry_budget_and_delays_are_the_model_s() {
    let modeled: serde_json::Value = serde_json::from_str(lean("\"budget\"").trim()).unwrap();
    assert_eq!(modeled["retryBudget"], RETRY_BUDGET);
    let delays: Vec<u64> = serde_json::from_value(modeled["delayMs"].clone()).unwrap();
    assert_eq!(delays.len(), RETRY_BUDGET as usize + 2);
    for (failures, delay) in delays.into_iter().enumerate() {
        assert_eq!(backoff_delay_ms(failures as u32), delay);
    }
    assert_eq!(backoff_delay_ms(u32::MAX), 30_000);
}

#[test]
fn decodes_split_events_and_classifies_errors_and_large_frames() {
    let mut scanned = 0;
    let mut buffer = b"event: next\ndata: {\"value\":".to_vec();
    assert!(take_event(&mut buffer, &mut scanned).unwrap().is_none());
    assert_eq!(scanned, buffer.len());
    buffer.extend_from_slice(b"\n");
    assert!(take_event(&mut buffer, &mut scanned).unwrap().is_none());
    buffer.pop();
    buffer.extend_from_slice(b"1}\n\nevent: error\ndata: denied\n\nevent: other\n\n");
    assert_eq!(
        take_event(&mut buffer, &mut scanned).unwrap(),
        Some(Frame::Next(serde_json::json!({"value": 1})))
    );
    assert_eq!(
        take_event(&mut buffer, &mut scanned).unwrap(),
        Some(Frame::ServerError)
    );
    assert!(matches!(
        take_event(&mut buffer, &mut scanned),
        Err(FrameError::Unexpected)
    ));
    let mut too_large = vec![b'x'; MAX_EVENT_BYTES + 1];
    assert!(matches!(
        take_event(&mut too_large, &mut 0),
        Err(FrameError::TooLarge)
    ));
    let mut invalid = b"event: next\ndata: {\n\n".to_vec();
    assert!(matches!(
        take_event(&mut invalid, &mut 0),
        Err(FrameError::Json)
    ));
}

#[derive(Clone)]
enum After {
    Close,
    Hang,
}

#[derive(Clone)]
enum Reply {
    Status(u16),
    Frames(Vec<String>, After),
}

fn snapshot(rev: u64) -> String {
    format!(
        "event: next\ndata: {}\n\n",
        serde_json::json!({"kind": "query", "output": {"rev": rev}})
    )
}

/// A runtime stand-in that answers each connection with the next scripted
/// reply, then with `rest` (or, without one, accepts and never answers).
struct Runtime {
    url: String,
    connections: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Runtime {
    async fn serve(replies: Vec<Reply>) -> Self {
        Self::serve_then(replies, None).await
    }

    async fn serve_then(replies: Vec<Reply>, rest: Option<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
        let connections = Arc::new(AtomicUsize::new(0));
        let replies = Arc::new(Mutex::new(VecDeque::from(replies)));
        let counted = connections.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                counted.fetch_add(1, Ordering::SeqCst);
                let reply = replies.lock().unwrap().pop_front().or_else(|| rest.clone());
                tokio::spawn(answer(socket, reply));
            }
        });
        Self {
            url,
            connections,
            task,
        }
    }

    fn endpoint(&self) -> GraphqlEndpoint {
        GraphqlEndpoint::anonymous(self.url.clone())
    }

    fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn answer(mut socket: TcpStream, reply: Option<Reply>) {
    let mut request = Vec::new();
    let mut chunk = [0u8; 4096];
    let body_start = loop {
        let read = socket.read(&mut chunk).await.unwrap_or(0);
        if read == 0 {
            return;
        }
        request.extend_from_slice(&chunk[..read]);
        if let Some(at) = request.windows(4).position(|part| part == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let head = String::from_utf8_lossy(&request[..body_start]).to_ascii_lowercase();
    let length = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while request.len() < body_start + length {
        let read = socket.read(&mut chunk).await.unwrap_or(0);
        if read == 0 {
            return;
        }
        request.extend_from_slice(&chunk[..read]);
    }
    match reply {
        None => std::future::pending::<()>().await,
        Some(Reply::Status(code)) => {
            let response = format!(
                "HTTP/1.1 {code} Scripted\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
        Some(Reply::Frames(frames, after)) => {
            let head =
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n";
            let _ = socket.write_all(head.as_bytes()).await;
            for frame in frames {
                let _ = socket.write_all(frame.as_bytes()).await;
            }
            let _ = socket.flush().await;
            if matches!(after, After::Hang) {
                std::future::pending::<()>().await;
            }
        }
    }
    let _ = socket.shutdown().await;
}

/// The page's snapshot shape (the bridge's `UxDataCallResult`).
#[derive(Debug, Deserialize)]
struct Row {
    #[allow(dead_code)]
    kind: String,
    output: serde_json::Value,
}

fn request() -> serde_json::Value {
    serde_json::json!({"ux": "ns/pack/page", "nodeDid": "did:key:node"})
}

#[derive(Debug, PartialEq)]
enum Seen {
    Snapshot(u64),
    Notice(WatchNotice),
}

struct Outcome {
    final_phase: WatchPhase,
    seen: Vec<Seen>,
    transitions: Vec<Transition>,
    /// When each transition was observed.
    at: Vec<Instant>,
    started: Instant,
}

impl Outcome {
    fn events(&self) -> Vec<WatchEvent> {
        self.transitions.iter().map(|(_, event, _)| *event).collect()
    }

    fn notices(&self) -> Vec<WatchNotice> {
        self.seen
            .iter()
            .filter_map(|seen| match seen {
                Seen::Notice(notice) => Some(*notice),
                Seen::Snapshot(_) => None,
            })
            .collect()
    }

    /// The delivered notices are exactly the model's, and the transitions are
    /// the model's.
    async fn assert_modeled(&self) {
        let modeled = assert_modeled_async(self.transitions.clone()).await;
        assert_eq!(self.notices(), modeled);
    }
}

/// Runs a watch whose acquisition returns `endpoints` in turn (the last one
/// repeats) and whose page unwatches once it has seen `until`. A page that is
/// not `open` refuses every delivery.
async fn watch(
    endpoints: Vec<Result<GraphqlEndpoint, AcquireFailure>>,
    until: Option<u64>,
    open: bool,
    cancel_on_backoff: bool,
) -> Outcome {
    let cancel = CancellationToken::new();
    let mut endpoints = VecDeque::from(endpoints);
    let acquire = || {
        let next = if endpoints.len() > 1 {
            endpoints.pop_front().unwrap()
        } else {
            endpoints.front().cloned().unwrap()
        };
        next.map_err(|error| error())
    };
    let seen = Mutex::new(Vec::new());
    let deliver = |delivery: WatchDelivery<Row>| {
        if !open {
            return false;
        }
        match delivery {
            WatchDelivery::Snapshot(row) => {
                let rev = row.output["rev"].as_u64().unwrap();
                seen.lock().unwrap().push(Seen::Snapshot(rev));
                if Some(rev) == until {
                    cancel.cancel();
                }
            }
            WatchDelivery::Notice(notice) => seen.lock().unwrap().push(Seen::Notice(notice)),
        }
        true
    };
    let transitions = Mutex::new(Vec::new());
    let at = Mutex::new(Vec::new());
    let observe = |phase, event, next| {
        transitions.lock().unwrap().push((phase, event, next));
        at.lock().unwrap().push(Instant::now());
        if cancel_on_backoff && matches!(next, WatchPhase::Backoff { .. }) {
            cancel.cancel();
        }
    };
    let started = Instant::now();
    let final_phase = tokio::time::timeout(
        Duration::from_secs(30),
        run(TIMING, acquire, &request(), deliver, &cancel, observe),
    )
    .await
    .expect("the watch settles");
    Outcome {
        final_phase,
        seen: seen.into_inner().unwrap(),
        transitions: transitions.into_inner().unwrap(),
        at: at.into_inner().unwrap(),
        started,
    }
}

type AcquireFailure = fn() -> WatchAcquireError;

const RESTARTING: AcquireFailure = || WatchAcquireError::ClientRestarting;

const OTHER_HOME: AcquireFailure =
    || WatchAcquireError::Access(LocalHomeAccessError::DifferentHome);

fn reconnecting(failures: u32) -> Seen {
    Seen::Notice(WatchNotice::Reconnecting {
        delay_ms: backoff_delay_ms(failures),
    })
}

fn unavailable(reason: TerminalReason) -> Seen {
    Seen::Notice(WatchNotice::Unavailable { reason })
}

/// Regression: one read timeout used to emit "Pack updates are unavailable."
/// and end the watch for good (TLA `MCPackUxWatchTransient`).
#[tokio::test]
async fn read_timeout_reconnects_and_delivers_a_fresh_snapshot() {
    let runtime = Runtime::serve(vec![
        Reply::Frames(vec![snapshot(1)], After::Hang),
        Reply::Frames(vec![snapshot(2)], After::Hang),
    ])
    .await;
    let outcome = watch(vec![Ok(runtime.endpoint())], Some(2), true, false).await;
    assert_eq!(
        outcome.seen,
        vec![Seen::Snapshot(1), reconnecting(1), Seen::Snapshot(2)]
    );
    assert_eq!(outcome.final_phase, WatchPhase::Cancelled);
    assert_eq!(runtime.connections(), 2);
    assert_eq!(
        outcome.events(),
        vec![
            WatchEvent::Connected,
            WatchEvent::TransientError,
            WatchEvent::Retry,
            WatchEvent::Connected,
            WatchEvent::Cancel,
        ]
    );
    outcome.assert_modeled().await;
}

/// Regression: the endpoint and authorization were captured at watch start,
/// so a runtime restart on a new port ended the watch (TLA
/// `MCPackUxWatchStaleEndpoint`). A clean end resets the count and sends no
/// notice.
#[tokio::test]
async fn a_restarted_runtime_is_reached_through_the_reacquired_endpoint() {
    let old = Runtime::serve(vec![Reply::Frames(vec![snapshot(1)], After::Close)]).await;
    let old_endpoint = old.endpoint();
    let restarted = Runtime::serve(vec![Reply::Frames(vec![snapshot(2)], After::Hang)]).await;
    let outcome = watch(
        vec![Ok(old_endpoint), Ok(restarted.endpoint())],
        Some(2),
        true,
        false,
    )
    .await;
    assert_eq!(old.connections(), 1);
    assert_eq!(outcome.seen, vec![Seen::Snapshot(1), Seen::Snapshot(2)]);
    assert_eq!(restarted.connections(), 1);
    assert_eq!(
        outcome.events(),
        vec![
            WatchEvent::Connected,
            WatchEvent::CleanEnd,
            WatchEvent::Retry,
            WatchEvent::Connected,
            WatchEvent::Cancel,
        ]
    );
    outcome.assert_modeled().await;
}

/// The runtime's `error` event is a failure of an accepted stream: it keeps
/// the count rather than resetting it.
#[tokio::test]
async fn the_runtime_error_event_and_a_restarting_client_are_retried() {
    let runtime = Runtime::serve(vec![
        Reply::Frames(
            vec!["event: error\ndata: reconnect\n\n".into()],
            After::Hang,
        ),
        Reply::Frames(vec![snapshot(7)], After::Hang),
    ])
    .await;
    let outcome = watch(vec![Err(RESTARTING), Ok(runtime.endpoint())], Some(7), true, false).await;
    assert_eq!(
        outcome.seen,
        vec![reconnecting(1), reconnecting(2), Seen::Snapshot(7)]
    );
    assert_eq!(
        outcome.events(),
        vec![
            WatchEvent::TransientError,
            WatchEvent::Retry,
            WatchEvent::Connected,
            WatchEvent::TransientError,
            WatchEvent::Retry,
            WatchEvent::Connected,
            WatchEvent::Cancel,
        ]
    );
    outcome.assert_modeled().await;
}

/// Regression: a runtime that accepted every connect and then emitted its
/// `error` event reset the failure count on each accept, so it was
/// reconnected every two seconds forever (TLA `MCPackUxWatchResetOnAccept`).
#[tokio::test]
async fn a_runtime_that_fails_every_accepted_stream_ends_the_watch() {
    let runtime = Runtime::serve_then(
        Vec::new(),
        Some(Reply::Frames(
            vec![snapshot(1), "event: error\ndata: reconnect\n\n".into()],
            After::Hang,
        )),
    )
    .await;
    let outcome = watch(vec![Ok(runtime.endpoint())], None, true, false).await;
    assert_eq!(
        outcome.final_phase,
        WatchPhase::Terminal {
            reason: TerminalReason::Unreachable
        }
    );
    assert_eq!(runtime.connections(), RETRY_BUDGET as usize);
    let mut expected = Vec::new();
    for failures in 1..RETRY_BUDGET {
        expected.push(Seen::Snapshot(1));
        expected.push(reconnecting(failures));
    }
    expected.push(Seen::Snapshot(1));
    expected.push(unavailable(TerminalReason::Unreachable));
    assert_eq!(outcome.seen, expected);
    outcome.assert_modeled().await;
}

/// Each attempt after a failure starts no sooner than the modeled delay
/// after the previous attempt started, and a runtime that never recovers
/// ends the watch after the budget and says so (TLA `MCPackUxWatchUnbounded`).
#[tokio::test]
async fn an_unavailable_runtime_is_retried_at_the_modeled_spacing_then_reported() {
    let runtime = Runtime::serve_then(Vec::new(), Some(Reply::Status(503))).await;
    let outcome = watch(vec![Ok(runtime.endpoint())], None, true, false).await;
    assert_eq!(
        outcome.final_phase,
        WatchPhase::Terminal {
            reason: TerminalReason::Unreachable
        }
    );
    assert_eq!(runtime.connections(), RETRY_BUDGET as usize);
    let mut expected: Vec<_> = (1..RETRY_BUDGET).map(reconnecting).collect();
    expected.push(unavailable(TerminalReason::Unreachable));
    assert_eq!(outcome.seen, expected);

    // An attempt starts after its predecessor's retry transition (or the run's
    // start), and its own retry waits until that start plus the delay.
    let mut previous_start = outcome.started;
    let mut retries = 0;
    for ((phase, event, _), at) in outcome.transitions.iter().zip(&outcome.at) {
        if let (WatchPhase::Backoff { failures }, WatchEvent::Retry) = (phase, event) {
            assert!(
                *at >= previous_start + TIMING.backoff(*failures),
                "retry after {failures} failures came {:?} after the attempt started",
                *at - previous_start
            );
            previous_start = *at;
            retries += 1;
        }
    }
    assert_eq!(retries, RETRY_BUDGET - 1);
    let budget: Duration = (1..RETRY_BUDGET).map(|failures| TIMING.backoff(failures)).sum();
    assert!(*outcome.at.last().unwrap() >= outcome.started + budget);
    outcome.assert_modeled().await;
}

#[tokio::test]
async fn an_authorization_refusal_names_its_cause_and_stops() {
    let runtime = Runtime::serve(vec![Reply::Status(401)]).await;
    let outcome = watch(vec![Ok(runtime.endpoint())], None, true, false).await;
    assert_eq!(
        outcome.final_phase,
        WatchPhase::Terminal {
            reason: TerminalReason::Auth
        }
    );
    assert_eq!(outcome.seen, vec![unavailable(TerminalReason::Auth)]);
    assert_eq!(runtime.connections(), 1);
    outcome.assert_modeled().await;
}

/// Regression: every access failure was treated as transient, so a runtime
/// record that names another home was retried forever and silently.
#[tokio::test]
async fn an_unreachable_runtime_record_is_permanent() {
    let outcome = watch(vec![Err(OTHER_HOME)], None, true, false).await;
    assert_eq!(
        outcome.final_phase,
        WatchPhase::Terminal {
            reason: TerminalReason::Permanent
        }
    );
    assert_eq!(outcome.events(), vec![WatchEvent::PermanentError]);
    assert_eq!(outcome.seen, vec![unavailable(TerminalReason::Permanent)]);
    outcome.assert_modeled().await;
}

#[tokio::test]
async fn rejected_queries_and_malformed_events_are_terminal() {
    let oversized = format!("event: next\ndata: {}", "x".repeat(MAX_EVENT_BYTES + 1));
    for reply in [
        Reply::Status(400),
        Reply::Frames(vec!["event: next\ndata: {\n\n".into()], After::Hang),
        Reply::Frames(
            vec!["event: next\ndata: {\"rev\": 1}\n\n".into()],
            After::Hang,
        ),
        Reply::Frames(vec!["event: other\n\n".into()], After::Hang),
        Reply::Frames(vec![oversized], After::Hang),
    ] {
        let runtime = Runtime::serve(vec![reply]).await;
        let outcome = watch(vec![Ok(runtime.endpoint())], None, true, false).await;
        assert_eq!(
            outcome.final_phase,
            WatchPhase::Terminal {
                reason: TerminalReason::Permanent
            }
        );
        assert_eq!(outcome.seen, vec![unavailable(TerminalReason::Permanent)]);
        assert_eq!(runtime.connections(), 1);
        outcome.assert_modeled().await;
    }
}

#[tokio::test]
async fn cancellation_during_backoff_and_a_closed_channel_send_nothing() {
    let runtime = Runtime::serve(vec![Reply::Status(503)]).await;
    let outcome = watch(vec![Ok(runtime.endpoint())], None, true, true).await;
    assert_eq!(outcome.final_phase, WatchPhase::Cancelled);
    assert_eq!(outcome.seen, vec![reconnecting(1)]);
    assert_eq!(runtime.connections(), 1);
    outcome.assert_modeled().await;

    let runtime = Runtime::serve(vec![Reply::Frames(vec![snapshot(1)], After::Hang)]).await;
    let outcome = watch(vec![Ok(runtime.endpoint())], None, false, false).await;
    assert_eq!(outcome.final_phase, WatchPhase::Cancelled);
    assert_eq!(
        outcome.events(),
        vec![WatchEvent::Connected, WatchEvent::Cancel]
    );
    outcome.assert_modeled().await;
}

/// A page whose channel closed while the task backs off is cancelled by the
/// notice it cannot receive, rather than reconnecting for nobody.
#[tokio::test]
async fn a_refused_notice_cancels_the_watch() {
    let runtime = Runtime::serve(vec![Reply::Status(503)]).await;
    let outcome = watch(vec![Ok(runtime.endpoint())], None, false, false).await;
    assert_eq!(outcome.final_phase, WatchPhase::Cancelled);
    assert_eq!(
        outcome.events(),
        vec![WatchEvent::TransientError, WatchEvent::Cancel]
    );
    assert_eq!(runtime.connections(), 1);
    // The model sends the notice the page refused.
    let modeled = assert_modeled_async(outcome.transitions.clone()).await;
    assert_eq!(
        modeled,
        vec![WatchNotice::Reconnecting {
            delay_ms: backoff_delay_ms(1)
        }]
    );
}
