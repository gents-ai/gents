//! HTTP proxy support: environment parsing (`HTTPS_PROXY` / `NO_PROXY`) and the
//! `CONNECT` tunnel handshake that precedes TLS.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use bytes::{Buf, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::error::TransportError;

/// Largest proxy reply head accepted.
const MAX_PROXY_REPLY: usize = 16 * 1024;

/// An HTTP proxy that tunnels TLS connections with `CONNECT`.
#[derive(Clone, Debug)]
pub struct Proxy {
    /// Proxy host name or address literal.
    pub host: String,
    /// Proxy TCP port.
    pub port: u16,
    /// Full `Proxy-Authorization` header value, such as `Basic ...`.
    pub authorization: Option<String>,
}

/// A proxy setting that cannot be used.
#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    /// The proxy value is not a URL with a host.
    #[error("the proxy setting is not a valid URL")]
    Invalid,
    /// Only `http://` proxies can tunnel TLS here.
    #[error("the proxy setting must start with http://")]
    UnsupportedScheme,
}

/// Picks the proxy for `host:port` from the values of `HTTPS_PROXY` (or its
/// lower case twin) and `NO_PROXY`, which the caller reads from the environment.
///
/// `Ok(None)` means connect directly: no proxy configured (absent or blank) or
/// `no_proxy` matches (comma list; `*`; domain suffix; `host:port` entries
/// match the port too). A configured value that is not an `http://` proxy URL
/// is an error, never a silent direct connection.
pub fn proxy_from_env(
    host: &str,
    port: u16,
    https_proxy: Option<&str>,
    no_proxy: Option<&str>,
) -> Result<Option<Proxy>, ProxyError> {
    let raw = https_proxy.map_or("", str::trim);
    if raw.is_empty() || no_proxy.is_some_and(|list| bypassed(host, port, list)) {
        return Ok(None);
    }
    let url = url::Url::parse(raw).map_err(|_| ProxyError::Invalid)?;
    if url.scheme() != "http" {
        return Err(ProxyError::UnsupportedScheme);
    }
    let proxy_host = url
        .host_str()
        .filter(|h| !h.is_empty())
        .ok_or(ProxyError::Invalid)?
        .trim_matches(['[', ']'])
        .to_owned();
    let authorization = (!url.username().is_empty()).then(|| {
        let mut creds = percent_decode(url.username());
        creds.push(b':');
        creds.extend(percent_decode(url.password().unwrap_or("")));
        format!("Basic {}", STANDARD.encode(creds))
    });
    Ok(Some(Proxy {
        host: proxy_host,
        port: url.port_or_known_default().ok_or(ProxyError::Invalid)?,
        authorization,
    }))
}

/// Decodes `%XX` escapes of a URL userinfo part; malformed escapes stay literal.
fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |x: u8| (x as char).to_digit(16);
        match (
            b[i],
            b.get(i + 1).and_then(|&x| hex(x)),
            b.get(i + 2).and_then(|&x| hex(x)),
        ) {
            (b'%', Some(h), Some(l)) => {
                out.push((h << 4 | l) as u8);
                i += 3;
            }
            (c, ..) => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Splits a `NO_PROXY` entry into host and optional port (`[v6]:p`, `h:p`).
fn split_entry(entry: &str) -> (&str, Option<&str>) {
    if let Some(rest) = entry.strip_prefix('[') {
        return match rest.split_once(']') {
            Some((h, tail)) => (h, tail.strip_prefix(':')),
            None => (entry, None),
        };
    }
    match entry.split_once(':') {
        Some((h, p)) if !p.contains(':') => (h, Some(p)),
        _ => (entry, None),
    }
}

/// Whether `host:port` matches a `NO_PROXY` list.
fn bypassed(host: &str, port: u16, list: &str) -> bool {
    let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
    list.split(',')
        .map(|entry| entry.trim().to_ascii_lowercase())
        .filter(|entry| !entry.is_empty())
        .any(|entry| {
            if entry == "*" {
                return true;
            }
            let (name, entry_port) = split_entry(&entry);
            if entry_port.is_some_and(|p| p.parse::<u16>().ok() != Some(port)) {
                return false;
            }
            let suffix = name.trim_start_matches("*.").trim_start_matches('.');
            host == suffix
                || host
                    .strip_suffix(suffix)
                    .is_some_and(|rest| rest.ends_with('.'))
        })
}

/// Sends `CONNECT host:port` over `io` and requires a 2xx reply with no extra bytes.
pub(crate) async fn tunnel<S>(
    io: &mut S,
    host: &str,
    port: u16,
    authorization: Option<&str>,
) -> Result<(), TransportError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if host.bytes().any(|b| b <= b' ' || b == 0x7f) {
        return Err(TransportError::InvalidServerName);
    }
    if authorization.is_some_and(|a| a.bytes().any(|b| b == b'\r' || b == b'\n')) {
        return Err(TransportError::InvalidProxy);
    }
    let authority = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let mut req = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if let Some(a) = authorization {
        req.push_str("Proxy-Authorization: ");
        req.push_str(a);
        req.push_str("\r\n");
    }
    req.push_str("\r\n");
    io.write_all(req.as_bytes())
        .await
        .map_err(TransportError::Io)?;
    io.flush().await.map_err(TransportError::Io)?;

    let mut buf = BytesMut::with_capacity(1024);
    loop {
        let mut raw = [httparse::EMPTY_HEADER; 32];
        let mut res = httparse::Response::new(&mut raw);
        match res.parse(&buf) {
            Ok(httparse::Status::Complete(n)) => {
                let status = res.code.ok_or(TransportError::ProxyReply)?;
                if !(200..300).contains(&status) {
                    return Err(TransportError::ProxyRefused(status));
                }
                buf.advance(n);
                // Bytes before our TLS hello can only be a protocol violation.
                return if buf.is_empty() {
                    Ok(())
                } else {
                    Err(TransportError::ProxyReply)
                };
            }
            Ok(httparse::Status::Partial) if buf.len() < MAX_PROXY_REPLY => {}
            _ => return Err(TransportError::ProxyReply),
        }
        if io.read_buf(&mut buf).await.map_err(TransportError::Io)? == 0 {
            return Err(TransportError::ProxyReply);
        }
    }
}
