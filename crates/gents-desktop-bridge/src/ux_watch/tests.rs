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

/// Executes the Lean owner on observed or enumerated phase/event pairs and
/// checks the native step and view against it.
fn assert_modeled(transitions: &[Transition]) {
    assert!(!transitions.is_empty());
    let inputs: Vec<_> = transitions
        .iter()
        .map(|(phase, event, _)| serde_json::json!({"phase": phase, "event": event}))
        .collect();
    let output = gents_lean_contract::run_prepared_generator_with_input(
        &gents_lean_contract::proofs_dir().unwrap(),
        "Proofs.Conformance.ClientPackWatch:olean",
        "Proofs/Conformance/ClientPackWatch.lean",
        &serde_json::to_string(&inputs).unwrap(),
    )
    .unwrap();
    let modeled: Vec<serde_json::Value> = serde_json::from_str(output.trim()).unwrap();
    assert_eq!(modeled.len(), transitions.len());
    for (expected, (phase, event, next)) in modeled.into_iter().zip(transitions) {
        let modeled_next: WatchPhase = serde_json::from_value(expected["next"].clone()).unwrap();
        let modeled_view: WatchView = serde_json::from_value(expected["view"].clone()).unwrap();
        assert_eq!(*next, modeled_next, "{phase:?} --{event:?}-->");
        assert_eq!(view(*next), modeled_view, "{phase:?} --{event:?}-->");
    }
}

async fn assert_modeled_async(transitions: Vec<Transition>) {
    tokio::task::spawn_blocking(move || assert_modeled(&transitions))
        .await
        .unwrap();
}

#[test]
fn every_phase_and_event_steps_as_the_model() {
    let mut phases = vec![
        WatchPhase::Streaming,
        WatchPhase::Cancelled,
        WatchPhase::Terminal {
            reason: TerminalReason::Auth,
        },
        WatchPhase::Terminal {
            reason: TerminalReason::Permanent,
        },
    ];
    for failures in 0..=MAX_FAILURES {
        phases.push(WatchPhase::Starting { failures });
        phases.push(WatchPhase::Backoff { failures });
    }
    let events = [
        WatchEvent::Connected,
        WatchEvent::CleanEnd,
        WatchEvent::TransientError,
        WatchEvent::AuthError,
        WatchEvent::PermanentError,
        WatchEvent::Cancel,
        WatchEvent::Retry,
    ];
    let transitions: Vec<_> = phases
        .iter()
        .flat_map(|phase| {
            events
                .iter()
                .map(move |event| (*phase, *event, step(*phase, *event)))
        })
        .collect();
    assert_eq!(transitions.len(), 16 * 7);
    assert_modeled(&transitions);
}

#[test]
fn cancellation_is_scoped_to_the_window_and_observation() {
    let registry = UxWatchRegistry::default();
    let a = registry.insert("one", "a").unwrap();
    let b = registry.insert("one", "b").unwrap();
    let other = registry.insert("two", "a").unwrap();
    registry.cancel("two", "b");
    assert!(!b.is_cancelled());
    registry.cancel("one", "a");
    assert!(a.is_cancelled());
    assert!(!other.is_cancelled());
    registry.cancel_window("one");
    assert!(b.is_cancelled());
    assert!(!other.is_cancelled());
}

#[test]
fn observations_are_bounded_and_ids_cannot_be_reused() {
    let registry = UxWatchRegistry::default();
    for index in 0..16 {
        registry.insert("one", &index.to_string()).unwrap();
    }
    assert!(registry.insert("one", "extra").is_err());
    assert!(registry.insert("one", "0").is_err());
    registry.cancel("one", "0");
    assert!(registry.insert("one", "extra").is_ok());
}

/// Regression: the task checked its token and then sent outside any lock, so
/// a selection change between the two delivered the old node's snapshot to
/// the newly selected window (TLA `MCPackUxWatchUnlockedDeliver`).
#[test]
fn a_cancelled_watch_delivers_nothing() {
    let registry = UxWatchRegistry::default();
    let token = registry.insert("main", "watch").unwrap();
    let mut sent = 0;
    assert!(registry.deliver("main", "watch", || {
        sent += 1;
        true
    }));
    registry.cancel_window("main");
    assert!(token.is_cancelled());
    assert!(!registry.deliver("main", "watch", || {
        sent += 1;
        true
    }));
    assert!(!registry.deliver("main", "other", || {
        sent += 1;
        true
    }));
    assert_eq!(sent, 1);
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

#[test]
fn backoff_is_positive_bounded_and_one_second_after_a_clean_end() {
    assert_eq!(backoff_delay_ms(0), 1000);
    for failures in 0..=200 {
        let delay = backoff_delay_ms(failures);
        assert!(delay > 0 && delay <= 30_000);
    }
    assert_eq!(backoff_delay_ms(u32::MAX), 30_000);
}

enum After {
    Close,
    Hang,
}

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
/// reply; with none left it accepts and never answers.
struct Runtime {
    url: String,
    connections: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Runtime {
    async fn serve(replies: Vec<Reply>) -> Self {
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
                let reply = replies.lock().unwrap().pop_front();
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

fn request() -> UxDataCallRequest {
    UxDataCallRequest {
        ux: "ns/pack/page".into(),
        node_did: "did:key:node".into(),
        surface: "surface".into(),
        tool: "list".into(),
        args: serde_json::json!({}),
    }
}

#[derive(Debug, PartialEq)]
enum Seen {
    Snapshot(u64),
    Error(String),
}

struct Outcome {
    final_phase: WatchPhase,
    seen: Vec<Seen>,
    transitions: Vec<Transition>,
    attempts: Vec<tokio::time::Instant>,
}

/// Runs a watch whose acquisition returns `endpoints` in turn (the last one
/// repeats) and whose page unwatches once it has seen `until`.
async fn watch(
    endpoints: Vec<Result<GraphqlEndpoint, WatchEvent>>,
    until: Option<u64>,
    open: bool,
    cancel_on_backoff: bool,
) -> Outcome {
    let cancel = CancellationToken::new();
    let mut endpoints = VecDeque::from(endpoints);
    let attempts = Mutex::new(Vec::new());
    let acquire = || {
        attempts.lock().unwrap().push(tokio::time::Instant::now());
        if endpoints.len() > 1 {
            endpoints.pop_front().unwrap()
        } else {
            endpoints.front().cloned().unwrap()
        }
    };
    let seen = Mutex::new(Vec::new());
    let deliver = |event: UxDataWatchEvent| {
        if !open {
            return false;
        }
        match event {
            UxDataWatchEvent::Snapshot { value } => {
                let rev = value.output["rev"].as_u64().unwrap();
                seen.lock().unwrap().push(Seen::Snapshot(rev));
                if Some(rev) == until {
                    cancel.cancel();
                }
            }
            UxDataWatchEvent::Error { message } => seen.lock().unwrap().push(Seen::Error(message)),
        }
        true
    };
    let transitions = Mutex::new(Vec::new());
    let observe = |phase, event, next| {
        transitions.lock().unwrap().push((phase, event, next));
        if cancel_on_backoff && matches!(next, WatchPhase::Backoff { .. }) {
            cancel.cancel();
        }
    };
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
        attempts: attempts.into_inner().unwrap(),
    }
}

fn events(outcome: &Outcome) -> Vec<WatchEvent> {
    outcome
        .transitions
        .iter()
        .map(|(_, event, _)| *event)
        .collect()
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
    assert_eq!(outcome.seen, vec![Seen::Snapshot(1), Seen::Snapshot(2)]);
    assert_eq!(outcome.final_phase, WatchPhase::Cancelled);
    assert_eq!(runtime.connections(), 2);
    assert_eq!(
        events(&outcome),
        vec![
            WatchEvent::Connected,
            WatchEvent::TransientError,
            WatchEvent::Retry,
            WatchEvent::Connected,
            WatchEvent::Cancel,
        ]
    );
    assert_modeled_async(outcome.transitions).await;
}

/// Regression: the endpoint and authorization were captured at watch start,
/// so a runtime restart on a new port ended the watch (TLA
/// `MCPackUxWatchStaleEndpoint`).
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
    assert_eq!(outcome.attempts.len(), 2);
    assert_eq!(
        events(&outcome),
        vec![
            WatchEvent::Connected,
            WatchEvent::CleanEnd,
            WatchEvent::Retry,
            WatchEvent::Connected,
            WatchEvent::Cancel,
        ]
    );
    assert_modeled_async(outcome.transitions).await;
}

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
    let outcome = watch(
        vec![Err(WatchEvent::TransientError), Ok(runtime.endpoint())],
        Some(7),
        true,
        false,
    )
    .await;
    assert_eq!(outcome.seen, vec![Seen::Snapshot(7)]);
    assert_eq!(
        events(&outcome),
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
    assert_modeled_async(outcome.transitions).await;
}

#[tokio::test]
async fn an_unavailable_runtime_backs_off_with_capped_spacing() {
    let mut replies: Vec<_> = (0..6).map(|_| Reply::Status(503)).collect();
    replies.push(Reply::Frames(vec![snapshot(3)], After::Hang));
    let runtime = Runtime::serve(replies).await;
    let outcome = watch(vec![Ok(runtime.endpoint())], Some(3), true, false).await;
    assert_eq!(outcome.seen, vec![Seen::Snapshot(3)]);
    let backoffs: Vec<_> = outcome
        .transitions
        .iter()
        .filter_map(|(_, _, next)| match next {
            WatchPhase::Backoff { failures } => Some(*failures),
            _ => None,
        })
        .collect();
    assert_eq!(backoffs, vec![1, 2, 3, 4, 5, 5]);
    assert_eq!(outcome.attempts.len(), 7);
    assert_modeled_async(outcome.transitions).await;
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
    assert_eq!(
        outcome.seen,
        vec![Seen::Error(TerminalReason::Auth.message().into())]
    );
    assert_eq!(runtime.connections(), 1);
    assert_modeled_async(outcome.transitions).await;
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
        assert_eq!(
            outcome.seen,
            vec![Seen::Error(TerminalReason::Permanent.message().into())]
        );
        assert_eq!(runtime.connections(), 1);
        assert_modeled_async(outcome.transitions).await;
    }
}

#[tokio::test]
async fn cancellation_during_backoff_and_a_closed_channel_send_nothing() {
    let runtime = Runtime::serve(vec![Reply::Status(503)]).await;
    let outcome = watch(vec![Ok(runtime.endpoint())], None, true, true).await;
    assert_eq!(outcome.final_phase, WatchPhase::Cancelled);
    assert!(outcome.seen.is_empty());
    assert_eq!(runtime.connections(), 1);
    assert_modeled_async(outcome.transitions).await;

    let runtime = Runtime::serve(vec![Reply::Frames(vec![snapshot(1)], After::Hang)]).await;
    let outcome = watch(vec![Ok(runtime.endpoint())], None, false, false).await;
    assert_eq!(outcome.final_phase, WatchPhase::Cancelled);
    assert_eq!(
        events(&outcome),
        vec![WatchEvent::Connected, WatchEvent::Cancel]
    );
    assert_modeled_async(outcome.transitions).await;
}
