use std::collections::VecDeque;
use std::io::Write;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;

use super::{read_http_request, write_http_response};

pub struct MockChatEndpoint {
    pub endpoint: String,
    pub port: u16,
    pub stop: Arc<AtomicBool>,
    pub captured_chat_requests: Arc<Mutex<Vec<Value>>>,
    pub handle: Option<JoinHandle<()>>,
}

#[derive(Clone)]
enum MockChatCompletion {
    Complete(String),
    RoutedDelayed {
        routes: Vec<(String, String)>,
        default_text: String,
        delay: Duration,
    },
    Hang,
}

impl MockChatEndpoint {
    pub fn start(model_name: &str, final_text: &str) -> Result<Self> {
        Self::start_with_required_bearer(model_name, final_text, None)
    }

    pub fn start_hanging(model_name: &str) -> Result<Self> {
        Self::start_with_completion(model_name, None, MockChatCompletion::Hang)
    }

    pub fn start_routed_delayed(
        model_name: &str,
        routes: Vec<(String, String)>,
        default_text: String,
        delay: Duration,
    ) -> Result<Self> {
        Self::start_with_completion(
            model_name,
            None,
            MockChatCompletion::RoutedDelayed {
                routes,
                default_text,
                delay,
            },
        )
    }

    pub fn start_with_required_bearer(
        model_name: &str,
        final_text: &str,
        required_bearer: Option<&str>,
    ) -> Result<Self> {
        Self::start_with_completion(
            model_name,
            required_bearer,
            MockChatCompletion::Complete(final_text.to_string()),
        )
    }

    fn start_with_completion(
        model_name: &str,
        required_bearer: Option<&str>,
        completion: MockChatCompletion,
    ) -> Result<Self> {
        Self::start_with_completions(model_name, required_bearer, vec![completion])
    }

    fn start_with_completions(
        model_name: &str,
        required_bearer: Option<&str>,
        completions: Vec<MockChatCompletion>,
    ) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).context("binding mock chat port")?;
        listener
            .set_nonblocking(true)
            .context("marking mock chat listener nonblocking")?;
        let port = listener
            .local_addr()
            .context("reading mock chat port")?
            .port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = stop.clone();
        let model_name = model_name.to_string();
        let required_bearer = required_bearer.map(ToOwned::to_owned);
        let captured_chat_requests = Arc::new(Mutex::new(Vec::new()));
        let captured_chat_requests_for_thread = captured_chat_requests.clone();
        let default_completion = completions
            .last()
            .cloned()
            .unwrap_or(MockChatCompletion::Hang);
        let completions = Arc::new(Mutex::new(VecDeque::from(completions)));
        let completions_for_thread = completions.clone();

        let handle = thread::spawn(move || {
            while !stop_for_thread.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let request = match read_http_request(&mut stream) {
                            Ok(request) => request,
                            Err(_) => {
                                let _ = stream.shutdown(Shutdown::Both);
                                continue;
                            }
                        };

                        let authorized = required_bearer.as_ref().is_none_or(|expected| {
                            request
                                .headers
                                .get("authorization")
                                .is_some_and(|value| value == &format!("Bearer {expected}"))
                        });

                        match (request.method.as_str(), request.path.as_str()) {
                            ("GET", "/v1/models") => {
                                let (status, body) = if authorized {
                                    ("200 OK", format!(r#"{{"data":[{{"id":"{model_name}"}}]}}"#))
                                } else {
                                    (
                                        "401 Unauthorized",
                                        r#"{"error":"unauthorized"}"#.to_string(),
                                    )
                                };
                                let _ = write_http_response(
                                    &mut stream,
                                    status,
                                    "application/json",
                                    &body,
                                );
                            }
                            ("POST", "/v1/chat/completions") => {
                                if !authorized {
                                    let _ = write_http_response(
                                        &mut stream,
                                        "401 Unauthorized",
                                        "application/json",
                                        r#"{"error":"unauthorized"}"#,
                                    );
                                    let _ = stream.shutdown(Shutdown::Both);
                                    continue;
                                }
                                let request_json: Value =
                                    match serde_json::from_slice(&request.body) {
                                        Ok(value) => value,
                                        Err(_) => {
                                            let _ = write_http_response(
                                                &mut stream,
                                                "400 Bad Request",
                                                "application/json",
                                                r#"{"error":"invalid json"}"#,
                                            );
                                            let _ = stream.shutdown(Shutdown::Both);
                                            continue;
                                        }
                                    };
                                captured_chat_requests_for_thread
                                    .lock()
                                    .expect("captured chat request mutex poisoned")
                                    .push(request_json.clone());

                                let completion = completions_for_thread
                                    .lock()
                                    .expect("mock chat completion queue mutex poisoned")
                                    .pop_front()
                                    .unwrap_or_else(|| default_completion.clone());

                                match completion {
                                    MockChatCompletion::Complete(final_text) => {
                                        let _ = write_http_response(
                                            &mut stream,
                                            "200 OK",
                                            "text/event-stream",
                                            &completion_text_sse(&final_text),
                                        );
                                    }
                                    MockChatCompletion::RoutedDelayed {
                                        routes,
                                        default_text,
                                        delay,
                                    } => {
                                        let final_text =
                                            routed_completion_text(&request_json, &routes)
                                                .unwrap_or(default_text);
                                        let deadline = std::time::Instant::now() + delay;
                                        while std::time::Instant::now() < deadline
                                            && !stop_for_thread.load(Ordering::Relaxed)
                                        {
                                            thread::sleep(Duration::from_millis(25));
                                        }
                                        let _ = write_http_response(
                                            &mut stream,
                                            "200 OK",
                                            "text/event-stream",
                                            &completion_text_sse(&final_text),
                                        );
                                    }
                                    MockChatCompletion::Hang => {
                                        while !stop_for_thread.load(Ordering::Relaxed) {
                                            thread::sleep(Duration::from_millis(25));
                                        }
                                    }
                                }
                            }
                            _ => {
                                let _ = write_http_response(
                                    &mut stream,
                                    "404 Not Found",
                                    "application/json",
                                    r#"{"error":"not found"}"#,
                                );
                            }
                        }

                        let _ = stream.flush();
                        let _ = stream.shutdown(Shutdown::Both);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(25));
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            endpoint: format!("http://127.0.0.1:{port}/v1"),
            port,
            stop,
            captured_chat_requests,
            handle: Some(handle),
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn captured_chat_requests(&self) -> Vec<Value> {
        self.captured_chat_requests
            .lock()
            .expect("captured chat request mutex poisoned")
            .clone()
    }
}

fn routed_completion_text(request_json: &Value, routes: &[(String, String)]) -> Option<String> {
    let request = request_json.to_string();
    routes
        .iter()
        .find(|(needle, _)| request.contains(needle))
        .map(|(_, response)| response.clone())
}

impl Drop for MockChatEndpoint {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub fn tool_call_sse(tool_name: &str, arguments: &str) -> String {
    let chunk_1 = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call-read-file",
                    "function": {
                        "name": tool_name,
                        "arguments": ""
                    }
                }]
            },
            "finish_reason": null
        }],
        "usage": null
    });
    let chunk_2 = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": null,
                    "function": {
                        "name": null,
                        "arguments": arguments
                    }
                }]
            },
            "finish_reason": null
        }],
        "usage": null
    });
    let chunk_3 = serde_json::json!({
        "choices": [{
            "delta": {
                "content": null,
                "tool_calls": []
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {
            "prompt_tokens": 16,
            "completion_tokens": 4,
            "total_tokens": 20
        }
    });
    format!(
        "data: {}\n\ndata: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        serde_json::to_string(&chunk_1).expect("serialize tool-call chunk 1"),
        serde_json::to_string(&chunk_2).expect("serialize tool-call chunk 2"),
        serde_json::to_string(&chunk_3).expect("serialize tool-call chunk 3"),
    )
}

pub fn completion_text_sse(text: &str) -> String {
    let chunk_1 = serde_json::json!({
        "choices": [{
            "delta": {
                "content": text
            },
            "finish_reason": null
        }],
        "usage": null
    });
    let chunk_2 = serde_json::json!({
        "choices": [{
            "delta": {
                "content": null,
                "tool_calls": []
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 24,
            "completion_tokens": 6,
            "total_tokens": 30
        }
    });
    format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        serde_json::to_string(&chunk_1).expect("serialize completion chunk 1"),
        serde_json::to_string(&chunk_2).expect("serialize completion chunk 2"),
    )
}
