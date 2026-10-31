//! The transport error type. Messages are short and carry no header values,
//! bodies or credentials.

use std::io;

/// A failure sending a request or reading its response.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// The server name is not valid for TLS.
    #[error("the server name is not valid")]
    InvalidServerName,
    /// Connecting did not finish within the connect timeout.
    #[error("connecting to the server timed out")]
    ConnectTimeout,
    /// The TCP connection could not be opened.
    #[error("could not connect to the server")]
    Connect(#[source] io::Error),
    /// The TLS handshake failed.
    #[error("the TLS handshake failed")]
    Tls(#[source] io::Error),
    /// The TLS client could not be configured.
    #[error("the TLS client could not be configured")]
    TlsConfig(#[source] rustls::Error),
    /// The proxy settings contain a forbidden character.
    #[error("the proxy settings are not valid")]
    InvalidProxy,
    /// The proxy answered the tunnel request with a non-2xx status.
    #[error("the proxy refused the tunnel (status {0})")]
    ProxyRefused(u16),
    /// The proxy reply was not a valid HTTP response.
    #[error("the proxy sent an invalid reply")]
    ProxyReply,
    /// Reading or writing the connection failed.
    #[error("the connection failed")]
    Io(#[source] io::Error),
    /// The server closed the connection before replying.
    #[error("the server closed the connection before replying")]
    Closed,
    /// The response head exceeds the size limit.
    #[error("the response head is too large")]
    HeadTooLarge,
    /// The response head is not valid HTTP/1.
    #[error("the response head is malformed")]
    MalformedHead,
    /// The response declares conflicting or invalid body framing.
    #[error("the response body framing is invalid")]
    BadFraming,
    /// A chunk header or trailer is malformed or too large.
    #[error("the response chunk framing is malformed")]
    MalformedChunk,
    /// The body ended before its declared end.
    #[error("the response body ended early")]
    Truncated,
    /// The response uses a content encoding this client cannot decode.
    #[error("the response content encoding is not supported")]
    UnsupportedEncoding,
    /// The compressed body is corrupt.
    #[error("the response body could not be decoded")]
    Decode,
}

/// Whether an I/O error is how a peer-closed idle socket shows itself.
pub(crate) fn is_peer_gone(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::UnexpectedEof
    )
}
