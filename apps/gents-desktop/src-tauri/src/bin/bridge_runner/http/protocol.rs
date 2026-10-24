use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const READ_TIMEOUT: Duration = Duration::from_secs(2);

type Result<T, E = ProtocolError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub(super) enum ProtocolError {
    #[error("timed out reading the request")]
    ReadTimeout(#[source] tokio::time::error::Elapsed),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("connection closed before headers")]
    ClosedBeforeHeaders,
    #[error("missing request line")]
    MissingRequestLine,
    #[error("missing request method")]
    MissingRequestMethod,
    #[error("missing request path")]
    MissingRequestPath,
}

#[derive(Debug)]
pub(super) struct HttpRequestData {
    pub(super) method: String,
    pub(super) path: String,
    pub(super) body: String,
}

pub(super) struct HttpResponse {
    pub(super) status: &'static str,
    pub(super) content_type: &'static str,
    pub(super) body: String,
}

impl HttpResponse {
    pub(super) fn empty(status: &'static str) -> Self {
        Self {
            status,
            content_type: "text/plain; charset=utf-8",
            body: String::new(),
        }
    }

    pub(super) fn json_ok(body: String) -> Self {
        Self {
            status: "200 OK",
            content_type: "application/json",
            body,
        }
    }

    pub(super) fn json_error(status: &'static str, error: &str) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: serde_json::json!({ "error": error }).to_string(),
        }
    }
}

async fn read_some(stream: &mut TcpStream, buffer: &mut [u8]) -> Result<usize> {
    let read = tokio::time::timeout(READ_TIMEOUT, stream.read(buffer))
        .await
        .map_err(ProtocolError::ReadTimeout)?;
    Ok(read?)
}

pub(super) async fn read_http_request(stream: &mut TcpStream) -> Result<HttpRequestData> {
    let mut buffer = Vec::new();
    let mut temp = [0_u8; 1024];
    let header_end = loop {
        let read = read_some(stream, &mut temp).await?;
        if read == 0 {
            return Err(ProtocolError::ClosedBeforeHeaders);
        }
        buffer.extend_from_slice(&temp[..read]);
        if let Some(index) = find_subslice(&buffer, b"\r\n\r\n") {
            break index + 4;
        }
    };
    let header_text = String::from_utf8_lossy(&buffer[..header_end]);
    let mut lines = header_text.split("\r\n").filter(|line| !line.is_empty());
    let request_line = lines.next().ok_or(ProtocolError::MissingRequestLine)?;
    let mut content_length = 0_usize;
    for line in lines.clone() {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or_default();
            }
        }
    }
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or(ProtocolError::MissingRequestMethod)?
        .to_string();
    let path = parts
        .next()
        .ok_or(ProtocolError::MissingRequestPath)?
        .to_string();
    while buffer.len() < header_end + content_length {
        let read = read_some(stream, &mut temp).await?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&temp[..read]);
    }
    let body =
        String::from_utf8_lossy(&buffer[header_end..buffer.len().min(header_end + content_length)])
            .to_string();

    Ok(HttpRequestData { method, path, body })
}

pub(super) async fn write_http_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) -> Result<()> {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: content-type\r\nAccess-Control-Max-Age: 600\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
