//! The response body: `Content-Length`, chunked or read-to-close framing,
//! content decoding, and hand-back of the connection to the idle pool once the
//! body ended cleanly. Dropping a `Body` mid-way closes its connection.

use std::sync::Arc;

use bytes::{Buf, Bytes, BytesMut};

use super::conn::Conn;
use super::decode::Decoder;
use super::error::TransportError;
use super::idle::{IdlePool, Key};

/// Longest chunk-size line (size plus extensions) accepted.
const MAX_CHUNK_LINE: usize = 4096;
/// Most trailer bytes accepted after the last chunk.
const MAX_TRAILER_BYTES: usize = 64 * 1024;
/// Decoded bytes produced per call.
const DECODE_BUF: usize = 32 * 1024;

/// How the body length is signalled.
pub(crate) enum Framing {
    Length(u64),
    Chunked(ChunkState),
    UntilClose,
    Done,
}

/// Position inside a chunked body.
pub(crate) enum ChunkState {
    Size,
    Data(u64),
    DataEnd,
    Trailers(usize),
}

enum Step {
    Data(Bytes),
    More,
    Done,
}

/// Where a connection returns to when its body ends cleanly.
pub(crate) struct Release {
    pub(crate) pool: Arc<IdlePool>,
    pub(crate) key: Key,
}

/// A streamed, decoded response body.
pub struct Body {
    conn: Option<Conn>,
    framing: Framing,
    decoder: Decoder,
    release: Option<Release>,
    pending: Bytes,
    out: Vec<u8>,
}

impl Body {
    pub(crate) fn new(
        conn: Conn,
        framing: Framing,
        decoder: Decoder,
        release: Option<Release>,
    ) -> Self {
        let out = if decoder.is_identity() {
            Vec::new()
        } else {
            vec![0; DECODE_BUF]
        };
        Self {
            conn: Some(conn),
            framing,
            decoder,
            release,
            pending: Bytes::new(),
            out,
        }
    }

    /// Returns the next decoded bytes as they arrive, or `None` at the end.
    pub async fn chunk(&mut self) -> Result<Option<Bytes>, TransportError> {
        let r = self.step().await;
        if r.is_err() {
            self.conn = None;
            self.release = None;
            self.framing = Framing::Done;
        }
        r
    }

    /// Returns the connection to the pool if the body is over and the wire is clean.
    pub(crate) fn settle(&mut self) {
        if !matches!(self.framing, Framing::Done) {
            return;
        }
        if let (Some(conn), Some(rel)) = (self.conn.take(), self.release.take()) {
            if conn.buf.is_empty() {
                rel.pool.put(rel.key, conn);
            }
        }
    }

    async fn step(&mut self) -> Result<Option<Bytes>, TransportError> {
        if self.decoder.is_identity() {
            return self.next_raw().await;
        }
        loop {
            let mut input: &[u8] = &self.pending;
            let before = input.len();
            let n = self.decoder.decode(&mut input, &mut self.out)?;
            let consumed = before - input.len();
            self.pending.advance(consumed);
            if n > 0 {
                return Ok(Some(Bytes::copy_from_slice(&self.out[..n])));
            }
            if !self.pending.is_empty() {
                if consumed == 0 {
                    return Err(TransportError::Decode);
                }
                continue;
            }
            match self.next_raw().await? {
                Some(raw) => self.pending = raw,
                None if self.decoder.complete() => return Ok(None),
                None => return Err(TransportError::Truncated),
            }
        }
    }

    /// Next framed (still encoded) bytes.
    async fn next_raw(&mut self) -> Result<Option<Bytes>, TransportError> {
        loop {
            let Some(conn) = self.conn.as_mut() else {
                return Ok(None);
            };
            let out = match &mut self.framing {
                Framing::Done => {
                    self.settle();
                    return Ok(None);
                }
                Framing::Length(rem) => {
                    if conn.buf.is_empty() {
                        fill(conn).await?;
                    }
                    let take = (*rem).min(conn.buf.len() as u64) as usize;
                    *rem -= take as u64;
                    let b = conn.buf.split_to(take).freeze();
                    if *rem == 0 {
                        self.framing = Framing::Done;
                    }
                    b
                }
                Framing::Chunked(st) => match chunk_step(st, &mut conn.buf)? {
                    Step::Data(b) => b,
                    Step::More => {
                        fill(conn).await?;
                        continue;
                    }
                    Step::Done => {
                        self.framing = Framing::Done;
                        continue;
                    }
                },
                Framing::UntilClose => {
                    if conn.buf.is_empty() && conn.fill().await.map_err(TransportError::Io)? == 0 {
                        self.framing = Framing::Done;
                        self.release = None;
                        self.conn = None;
                        return Ok(None);
                    }
                    conn.buf.split().freeze()
                }
            };
            self.settle();
            return Ok(Some(out));
        }
    }
}

/// Reads more body bytes; a close here means the body ended early.
async fn fill(conn: &mut Conn) -> Result<(), TransportError> {
    match conn.fill().await.map_err(TransportError::Io)? {
        0 => Err(TransportError::Truncated),
        _ => Ok(()),
    }
}

fn find_lf(buf: &[u8]) -> Option<usize> {
    buf.iter().position(|&b| b == b'\n')
}

/// Parses one chunk-size line (without its line ending) into a size.
fn parse_chunk_size(line: &[u8]) -> Result<u64, TransportError> {
    let digits = line
        .iter()
        .position(|b| !b.is_ascii_hexdigit())
        .unwrap_or(line.len());
    let (hex, rest) = line.split_at(digits);
    // Only whitespace or a `;extension` may follow the digits.
    let rest = rest.trim_ascii_start();
    if hex.is_empty() || hex.len() > 16 || !(rest.is_empty() || rest[0] == b';') {
        return Err(TransportError::MalformedChunk);
    }
    hex.iter().try_fold(0u64, |acc, &b| {
        let d = (b as char)
            .to_digit(16)
            .ok_or(TransportError::MalformedChunk)?;
        Ok(acc << 4 | u64::from(d))
    })
}

/// Advances the chunked decoder over the buffered bytes.
fn chunk_step(st: &mut ChunkState, buf: &mut BytesMut) -> Result<Step, TransportError> {
    loop {
        match st {
            ChunkState::Size => {
                let Some(lf) = find_lf(buf) else {
                    return if buf.len() > MAX_CHUNK_LINE {
                        Err(TransportError::MalformedChunk)
                    } else {
                        Ok(Step::More)
                    };
                };
                if lf + 1 > MAX_CHUNK_LINE {
                    return Err(TransportError::MalformedChunk);
                }
                let line = buf[..lf].strip_suffix(b"\r").unwrap_or(&buf[..lf]);
                let size = parse_chunk_size(line)?;
                buf.advance(lf + 1);
                *st = if size == 0 {
                    ChunkState::Trailers(0)
                } else {
                    ChunkState::Data(size)
                };
            }
            ChunkState::Data(rem) => {
                if buf.is_empty() {
                    return Ok(Step::More);
                }
                let take = (*rem).min(buf.len() as u64) as usize;
                *rem -= take as u64;
                if *rem == 0 {
                    *st = ChunkState::DataEnd;
                }
                return Ok(Step::Data(buf.split_to(take).freeze()));
            }
            ChunkState::DataEnd => {
                if buf.len() < 2 {
                    return Ok(Step::More);
                }
                if &buf[..2] != b"\r\n" {
                    return Err(TransportError::MalformedChunk);
                }
                buf.advance(2);
                *st = ChunkState::Size;
            }
            ChunkState::Trailers(seen) => {
                let Some(lf) = find_lf(buf) else {
                    return if *seen + buf.len() > MAX_TRAILER_BYTES {
                        Err(TransportError::MalformedChunk)
                    } else {
                        Ok(Step::More)
                    };
                };
                *seen += lf + 1;
                if *seen > MAX_TRAILER_BYTES {
                    return Err(TransportError::MalformedChunk);
                }
                let blank = lf == 0 || (lf == 1 && buf[0] == b'\r');
                buf.advance(lf + 1);
                if blank {
                    return Ok(Step::Done);
                }
            }
        }
    }
}
