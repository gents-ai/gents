//! A buffered connection and the response-head reader.

use std::io::{self, IoSlice};

use bytes::{Buf, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::error::TransportError;

/// Largest response head accepted (status line plus headers).
pub(crate) const MAX_HEAD_BYTES: usize = 64 * 1024;
/// Most headers accepted in a response head.
pub(crate) const MAX_HEADERS: usize = 128;
/// Minimum free space ensured before each socket read.
const READ_RESERVE: usize = 16 * 1024;

/// A byte stream the client can talk over (plain TCP or TLS).
pub(crate) trait Stream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T> Stream for T where T: AsyncRead + AsyncWrite + Send + Unpin {}

/// A connection with its read buffer, reused for every response on it.
pub(crate) struct Conn {
    io: Box<dyn Stream>,
    pub(crate) buf: BytesMut,
}

/// A parsed response head.
pub(crate) struct Head {
    pub(crate) status: u16,
    pub(crate) minor: u8,
    pub(crate) headers: Vec<(String, String)>,
}

impl Conn {
    pub(crate) fn new(io: Box<dyn Stream>) -> Self {
        Self {
            io,
            buf: BytesMut::with_capacity(READ_RESERVE),
        }
    }

    /// Reads once into the buffer; zero means the peer closed.
    pub(crate) async fn fill(&mut self) -> io::Result<usize> {
        self.buf.reserve(READ_RESERVE);
        self.io.read_buf(&mut self.buf).await
    }

    /// Writes head then body with vectored writes, then flushes.
    pub(crate) async fn write_all(&mut self, head: &[u8], body: &[u8]) -> io::Result<()> {
        let (mut h, mut b) = (head, body);
        while !h.is_empty() || !b.is_empty() {
            let n = if h.is_empty() {
                self.io.write(b).await?
            } else {
                self.io
                    .write_vectored(&[IoSlice::new(h), IoSlice::new(b)])
                    .await?
            };
            if n == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            let from_head = n.min(h.len());
            h = &h[from_head..];
            b = &b[n - from_head..];
        }
        self.io.flush().await
    }
}

/// Reads and parses one response head, leaving any body bytes in the buffer.
pub(crate) async fn read_head(conn: &mut Conn) -> Result<Head, TransportError> {
    loop {
        let mut raw = [httparse::EMPTY_HEADER; MAX_HEADERS];
        let mut res = httparse::Response::new(&mut raw);
        match res.parse(&conn.buf) {
            Ok(httparse::Status::Complete(n)) => {
                if n > MAX_HEAD_BYTES {
                    return Err(TransportError::HeadTooLarge);
                }
                let head = Head {
                    status: res.code.ok_or(TransportError::MalformedHead)?,
                    minor: res.version.ok_or(TransportError::MalformedHead)?,
                    headers: res
                        .headers
                        .iter()
                        .map(|h| {
                            (
                                h.name.to_owned(),
                                String::from_utf8_lossy(h.value).into_owned(),
                            )
                        })
                        .collect(),
                };
                conn.buf.advance(n);
                return Ok(head);
            }
            Ok(httparse::Status::Partial) if conn.buf.len() >= MAX_HEAD_BYTES => {
                return Err(TransportError::HeadTooLarge);
            }
            Ok(httparse::Status::Partial) => {}
            Err(httparse::Error::TooManyHeaders) => return Err(TransportError::HeadTooLarge),
            Err(_) => return Err(TransportError::MalformedHead),
        }
        match conn.fill().await.map_err(TransportError::Io)? {
            0 if conn.buf.is_empty() => return Err(TransportError::Closed),
            0 => return Err(TransportError::Truncated),
            _ => {}
        }
    }
}
