use super::*;

pub(crate) struct MockModelEndpoint {
    endpoint: String,
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

#[derive(Clone)]
pub(crate) enum MockModelMode {
    Text,
    ToolLoop { final_text: String },
}

impl MockModelEndpoint {
    pub(crate) fn start(model_name: &str) -> Result<Self> {
        Self::start_with_mode(model_name, MockModelMode::Text)
    }

    pub(crate) fn start_tool_loop(model_name: &str, final_text: impl Into<String>) -> Result<Self> {
        Self::start_with_mode(
            model_name,
            MockModelMode::ToolLoop {
                final_text: final_text.into(),
            },
        )
    }

    fn start_with_mode(model_name: &str, mode: MockModelMode) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop);
        let model_name = model_name.to_string();
        let mode_for_thread = mode.clone();
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
                        let (status, content_type, body) = if request.method == "GET"
                            && (request.path == "/v1/models" || request.path == "/models")
                        {
                            (
                                "200 OK",
                                "application/json",
                                format!(r#"{{"data":[{{"id":"{model_name}"}}]}}"#),
                            )
                        } else if request.method == "POST"
                            && (request.path == "/v1/chat/completions"
                                || request.path == "/chat/completions")
                        {
                            let body = match &mode_for_thread {
                                MockModelMode::Text => mock_completion_sse("mock response"),
                                MockModelMode::ToolLoop { final_text } => {
                                    if request_has_tool_result_message(&request.body) {
                                        let text = extract_desktop_tool_token(&request.body)
                                            .unwrap_or_else(|| final_text.clone());
                                        mock_completion_sse(&text)
                                    } else {
                                        mock_tool_call_sse("read_file", r#"{"path":"notes.txt"}"#)
                                    }
                                }
                            };
                            ("200 OK", "text/event-stream", body)
                        } else {
                            (
                                "404 Not Found",
                                "application/json",
                                r#"{"error":"not found"}"#.to_string(),
                            )
                        };
                        let _ = write_http_response(&mut stream, status, content_type, &body);
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
            handle: Some(handle),
        })
    }

    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

pub(crate) fn request_has_tool_result_message(body: &str) -> bool {
    body.contains(r#""role":"tool""#) || body.contains(r#""role": "tool""#)
}

pub(crate) fn extract_desktop_tool_token(body: &str) -> Option<String> {
    let marker = "DESKTOP_TOOL_TOKEN_";
    let start = body.find(marker)?;
    let token = body[start..]
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect::<String>();
    (!token.is_empty()).then_some(token)
}

pub(crate) fn mock_tool_call_sse(tool_name: &str, arguments: &str) -> String {
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
        serde_json::to_string(&chunk_1).expect("serialize mock tool chunk 1"),
        serde_json::to_string(&chunk_2).expect("serialize mock tool chunk 2"),
        serde_json::to_string(&chunk_3).expect("serialize mock tool chunk 3"),
    )
}

pub(crate) fn mock_completion_sse(text: &str) -> String {
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
        serde_json::to_string(&chunk_1).expect("serialize mock completion chunk 1"),
        serde_json::to_string(&chunk_2).expect("serialize mock completion chunk 2"),
    )
}

impl Drop for MockModelEndpoint {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
