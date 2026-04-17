use std::io::Read;
use std::net::TcpStream;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

pub mod chat;
pub mod model;
pub mod openai;

pub use chat::MockChatEndpoint;
pub use model::MockModelEndpoint;
pub use openai::MockOpenAIEndpoint;

// Re-export SSE helpers so callers that use `mocks::tool_call_sse` etc. continue to work.
pub use chat::{completion_text_sse, tool_call_sse};

pub struct HttpRequestData {
    pub method: String,
    pub path: String,
    pub headers: std::collections::HashMap<String, String>,
    pub body: Vec<u8>,
}

pub fn read_http_request(stream: &mut TcpStream) -> Result<HttpRequestData> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    let mut header_end = None;
    let mut content_length = 0_usize;

    loop {
        let read = stream.read(&mut chunk).context("reading mock request")?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);

        if header_end.is_none() {
            if let Some(offset) = find_header_end(&buffer) {
                let end = offset + 4;
                let headers = String::from_utf8_lossy(&buffer[..end]);
                header_end = Some(end);
                content_length = parse_content_length(&headers).unwrap_or(0);
                if buffer.len() >= end + content_length {
                    break;
                }
            }
        } else if buffer.len() >= header_end.expect("header_end should be set") + content_length {
            break;
        }
    }

    let header_end = header_end.ok_or_else(|| anyhow!("missing request headers"))?;
    let header_text = String::from_utf8_lossy(&buffer[..header_end]);
    let request_line = header_text
        .lines()
        .next()
        .ok_or_else(|| anyhow!("missing request line"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| anyhow!("missing HTTP method"))?
        .to_string();
    let path = parts
        .next()
        .ok_or_else(|| anyhow!("missing HTTP path"))?
        .to_string();
    let headers = header_text
        .lines()
        .skip(1)
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_string()))
        })
        .collect();
    let body_end = header_end + content_length;
    let body = if buffer.len() >= body_end {
        buffer[header_end..body_end].to_vec()
    } else {
        Vec::new()
    };

    Ok(HttpRequestData {
        method,
        path,
        headers,
        body,
    })
}

pub fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

pub fn parse_content_length(headers: &str) -> Option<usize> {
    headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.eq_ignore_ascii_case("content-length") {
            value.trim().parse().ok()
        } else {
            None
        }
    })
}

pub fn write_http_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) -> Result<()> {
    use std::io::Write;
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .context("writing mock response")?;
    Ok(())
}

pub fn request_has_tool_result_message(request: &Value) -> bool {
    request
        .get("messages")
        .and_then(Value::as_array)
        .is_some_and(|messages| {
            messages
                .iter()
                .any(|message| message.get("role").and_then(Value::as_str) == Some("tool"))
        })
}

pub fn request_tool_names(request: &Value) -> Vec<String> {
    request
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| {
                    tool.get("function")
                        .and_then(|function| function.get("name"))
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

pub fn request_system_message(request: &Value) -> Option<&str> {
    request
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| {
            messages.iter().find_map(|message| {
                if message.get("role").and_then(Value::as_str) != Some("system") {
                    return None;
                }
                match message.get("content") {
                    Some(Value::String(content)) => Some(content.as_str()),
                    Some(Value::Array(parts)) => parts
                        .iter()
                        .find_map(|part| part.get("text").and_then(Value::as_str)),
                    _ => None,
                }
            })
        })
}

pub fn request_tool_result_text(request: &Value) -> Option<String> {
    request
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| {
            messages.iter().find_map(|message| {
                if message.get("role").and_then(Value::as_str) != Some("tool") {
                    return None;
                }
                match message.get("content") {
                    Some(Value::String(content)) => Some(content.to_string()),
                    Some(Value::Array(parts)) => {
                        let text = parts
                            .iter()
                            .filter_map(|part| part.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("\n");
                        Some(text)
                    }
                    _ => None,
                }
            })
        })
}

pub fn request_contains_role_text(request: &Value, role: &str, needle: &str) -> bool {
    request
        .get("messages")
        .and_then(Value::as_array)
        .is_some_and(|messages| {
            messages.iter().any(|message| {
                if message.get("role").and_then(Value::as_str) != Some(role) {
                    return false;
                }
                match message.get("content") {
                    Some(Value::String(content)) => content.contains(needle),
                    Some(Value::Array(parts)) => parts.iter().any(|part| {
                        part.get("text")
                            .and_then(Value::as_str)
                            .is_some_and(|text| text.contains(needle))
                    }),
                    _ => false,
                }
            })
        })
}
