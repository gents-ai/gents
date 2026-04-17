use std::io::Write;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;

use super::chat::{completion_text_sse, tool_call_sse};
use super::{read_http_request, request_has_tool_result_message, write_http_response};

pub struct MockOpenAIEndpoint {
    pub endpoint: String,
    pub port: u16,
    pub stop: Arc<AtomicBool>,
    pub captured_chat_requests: Arc<Mutex<Vec<Value>>>,
    pub handle: Option<JoinHandle<()>>,
}

impl MockOpenAIEndpoint {
    pub fn start(model_name: &str, final_token: &str) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).context("binding mock OpenAI port")?;
        listener
            .set_nonblocking(true)
            .context("marking mock OpenAI listener nonblocking")?;
        let port = listener
            .local_addr()
            .context("reading mock OpenAI port")?
            .port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = stop.clone();
        let model_name = model_name.to_string();
        let final_token = final_token.to_string();
        let captured_chat_requests = Arc::new(Mutex::new(Vec::new()));
        let captured_chat_requests_for_thread = captured_chat_requests.clone();

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

                        match (request.method.as_str(), request.path.as_str()) {
                            ("GET", "/v1/models") => {
                                let body = format!(r#"{{"data":[{{"id":"{model_name}"}}]}}"#);
                                let _ = write_http_response(
                                    &mut stream,
                                    "200 OK",
                                    "application/json",
                                    &body,
                                );
                            }
                            ("POST", "/v1/chat/completions") => {
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

                                let sse_body = if request_has_tool_result_message(&request_json) {
                                    completion_text_sse(&final_token)
                                } else {
                                    tool_call_sse("read_file", r#"{"path":"notes.txt"}"#)
                                };
                                let _ = write_http_response(
                                    &mut stream,
                                    "200 OK",
                                    "text/event-stream",
                                    &sse_body,
                                );
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

impl Drop for MockOpenAIEndpoint {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
