//! Deterministic OpenAI-compatible streaming backend for full-daemon tests.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::json;

use super::http_mock::{read_http_request, write_http_response};

#[derive(Clone, Debug)]
pub struct StreamScript {
    marker: String,
    chunks: Vec<String>,
    pause_after_chunks: bool,
}

impl StreamScript {
    pub fn paused(
        marker: impl Into<String>,
        chunks: impl IntoIterator<Item = &'static str>,
    ) -> Self {
        Self {
            marker: marker.into(),
            chunks: chunks.into_iter().map(ToOwned::to_owned).collect(),
            pause_after_chunks: true,
        }
    }

    pub fn completes(
        marker: impl Into<String>,
        chunks: impl IntoIterator<Item = &'static str>,
    ) -> Self {
        Self {
            marker: marker.into(),
            chunks: chunks.into_iter().map(ToOwned::to_owned).collect(),
            pause_after_chunks: false,
        }
    }
}

pub struct MockStreamingBackend {
    endpoint: String,
    port: u16,
    stop: Arc<AtomicBool>,
    state: Arc<StreamingState>,
    handle: Option<JoinHandle<()>>,
}

impl MockStreamingBackend {
    pub fn start(model_name: &str, scripts: Vec<StreamScript>) -> anyhow::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let state = Arc::new(StreamingState::new(scripts));
        let model_name = model_name.to_string();
        let stop_for_thread = stop.clone();
        let state_for_thread = state.clone();
        let handle = thread::spawn(move || {
            while !stop_for_thread.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let stop = stop_for_thread.clone();
                        let state = state_for_thread.clone();
                        let model_name = model_name.clone();
                        thread::spawn(move || {
                            let _ = handle_connection(stream, &model_name, state, stop);
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            endpoint: format!("http://127.0.0.1:{port}/v1"),
            port,
            stop,
            state,
            handle: Some(handle),
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn release(&self, marker: &str) {
        self.state.release(marker);
    }

    pub fn observed_chunks(&self, marker: &str) -> usize {
        self.state.chunk_count(marker)
    }

    pub async fn wait_for_chunks(&self, marker: &str, expected: usize) {
        let state = self.state.clone();
        let marker = marker.to_string();
        let marker_for_wait = marker.clone();
        let observed = tokio::task::spawn_blocking(move || {
            state.wait_for_chunk_count(&marker_for_wait, expected, Duration::from_secs(5))
        })
        .await
        .expect("streaming backend chunk wait task should join");
        assert!(
            observed >= expected,
            "timed out waiting for {expected} chunk(s) for marker {marker}, observed {observed}"
        );
    }
}

impl Drop for MockStreamingBackend {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.state.notify_all();
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct StreamingState {
    scripts: Vec<StreamScript>,
    inner: Mutex<StreamingStateInner>,
    condvar: Condvar,
}

#[derive(Default)]
struct StreamingStateInner {
    chunk_counts: HashMap<String, usize>,
    releases: HashSet<String>,
}

impl StreamingState {
    fn new(scripts: Vec<StreamScript>) -> Self {
        Self {
            scripts,
            inner: Mutex::new(StreamingStateInner::default()),
            condvar: Condvar::new(),
        }
    }

    fn find_script(&self, body: &str) -> Option<StreamScript> {
        self.scripts
            .iter()
            .find(|script| body.contains(&script.marker))
            .cloned()
    }

    fn record_chunk(&self, marker: &str) {
        let mut inner = self.inner.lock().expect("streaming backend mutex poisoned");
        *inner.chunk_counts.entry(marker.to_string()).or_default() += 1;
        self.condvar.notify_all();
    }

    fn chunk_count(&self, marker: &str) -> usize {
        self.inner
            .lock()
            .expect("streaming backend mutex poisoned")
            .chunk_counts
            .get(marker)
            .copied()
            .unwrap_or_default()
    }

    fn wait_for_chunk_count(&self, marker: &str, expected: usize, timeout: Duration) -> usize {
        let started = Instant::now();
        let mut inner = self.inner.lock().expect("streaming backend mutex poisoned");
        loop {
            let actual = inner.chunk_counts.get(marker).copied().unwrap_or_default();
            if actual >= expected || started.elapsed() >= timeout {
                return actual;
            }
            let remaining = timeout
                .checked_sub(started.elapsed())
                .unwrap_or(Duration::ZERO);
            inner = self
                .condvar
                .wait_timeout(inner, remaining.min(Duration::from_millis(25)))
                .expect("streaming backend condvar poisoned")
                .0;
        }
    }

    fn release(&self, marker: &str) {
        self.inner
            .lock()
            .expect("streaming backend mutex poisoned")
            .releases
            .insert(marker.to_string());
        self.condvar.notify_all();
    }

    fn wait_for_release_or_stop(&self, marker: &str, stop: &AtomicBool) {
        let mut inner = self.inner.lock().expect("streaming backend mutex poisoned");
        while !stop.load(Ordering::Relaxed) && !inner.releases.contains(marker) {
            inner = self
                .condvar
                .wait_timeout(inner, Duration::from_millis(25))
                .expect("streaming backend condvar poisoned")
                .0;
        }
    }

    fn notify_all(&self) {
        self.condvar.notify_all();
    }
}

fn handle_connection(
    mut stream: TcpStream,
    model_name: &str,
    state: Arc<StreamingState>,
    stop: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let request = match read_http_request(&mut stream) {
        Ok(request) => request,
        Err(error) => {
            let _ = stream.shutdown(Shutdown::Both);
            return Err(error);
        }
    };

    if request.method == "GET" && (request.path == "/v1/models" || request.path == "/models") {
        let body = format!(r#"{{"data":[{{"id":"{model_name}"}}]}}"#);
        write_http_response(&mut stream, "200 OK", "application/json", &body)?;
        let _ = stream.shutdown(Shutdown::Both);
        return Ok(());
    }

    if request.method == "POST"
        && (request.path == "/v1/chat/completions" || request.path == "/chat/completions")
    {
        if request_is_streaming(&request.body) {
            let script = state.find_script(&request.body).unwrap_or_else(|| {
                StreamScript::completes("__default__", ["mock streamed response"])
            });
            write_streaming_response(&mut stream, &script, state, stop)?;
        } else {
            write_completion_response(&mut stream, model_name)?;
        }
        let _ = stream.shutdown(Shutdown::Both);
        return Ok(());
    }

    write_http_response(
        &mut stream,
        "404 Not Found",
        "application/json",
        r#"{"error":"not found"}"#,
    )?;
    let _ = stream.shutdown(Shutdown::Both);
    Ok(())
}

fn request_is_streaming(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("stream").and_then(serde_json::Value::as_bool))
        .unwrap_or(false)
}

fn write_completion_response(stream: &mut TcpStream, model_name: &str) -> anyhow::Result<()> {
    let body = json!({
        "id": "chatcmpl-title",
        "object": "chat.completion",
        "created": 1710000000_u64,
        "model": model_name,
        "choices": [{
            "index": 0,
            "finish_reason": "stop",
            "message": {
                "role": "assistant",
                "content": "mock-title",
                "refusal": null,
                "reasoning": null
            }
        }],
        "usage": {
            "prompt_tokens": 4,
            "completion_tokens": 1,
            "total_tokens": 5
        }
    })
    .to_string();
    write_http_response(stream, "200 OK", "application/json", &body)
}

fn write_streaming_response(
    stream: &mut TcpStream,
    script: &StreamScript,
    state: Arc<StreamingState>,
    stop: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
    )?;
    stream.flush()?;

    for chunk in &script.chunks {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        write_sse_chunk(stream, chunk)?;
        state.record_chunk(&script.marker);
    }

    if script.pause_after_chunks {
        state.wait_for_release_or_stop(&script.marker, &stop);
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
    }

    write_sse_usage(stream)?;
    stream.write_all(b"data: [DONE]\n\n")?;
    stream.flush()?;
    Ok(())
}

fn write_sse_chunk(stream: &mut TcpStream, content: &str) -> anyhow::Result<()> {
    let data = json!({
        "choices": [{
            "delta": {
                "content": content,
                "tool_calls": []
            },
            "finish_reason": null
        }],
        "usage": null
    });
    write!(stream, "data: {data}\n\n")?;
    stream.flush()?;
    Ok(())
}

fn write_sse_usage(stream: &mut TcpStream) -> anyhow::Result<()> {
    let data = json!({
        "choices": [],
        "usage": {
            "prompt_tokens": 8,
            "completion_tokens": 3,
            "total_tokens": 11
        }
    });
    write!(stream, "data: {data}\n\n")?;
    stream.flush()?;
    Ok(())
}
