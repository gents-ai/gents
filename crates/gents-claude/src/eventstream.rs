//! The AWS event-stream binary framing that Amazon Bedrock answers with
//! (`application/vnd.amazon.eventstream`), decoded as the vendored smithy codec
//! in Claude Code 2.1.311 does.
//!
//! A frame is a 12-byte prelude (total length, headers length, CRC32 of those
//! eight bytes), the typed headers, the payload and a CRC32 of everything before
//! it. [`Decoder::push`] takes the response bytes as they arrive, in any split,
//! and returns the complete frames. Memory is bounded by one frame: the prelude
//! is checked before a frame is buffered, and a frame above [`MAX_FRAME_BYTES`]
//! (the AWS event-stream maximum message size) or with headers above
//! [`MAX_HEADER_BYTES`] fails with a typed error instead of growing the buffer.
//! A corrupt prelude or frame poisons the decoder: every later call returns the
//! same error, so a damaged stream never yields a frame after the damage.
//! [`encode`] writes a frame, for fixtures and round-trip tests.

use crc32fast::Hasher;

/// Largest frame accepted: the AWS event-stream maximum message size, 16 MiB.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
/// Largest header block accepted: the AWS event-stream maximum, 128 KiB.
pub const MAX_HEADER_BYTES: usize = 128 * 1024;

const PRELUDE_BYTES: usize = 12;
/// The prelude and the trailing message CRC.
const FRAMING_BYTES: usize = PRELUDE_BYTES + 4;

/// A typed header value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeaderValue {
    /// Type 0 and 1.
    Bool(bool),
    /// Type 2.
    Byte(i8),
    /// Type 3.
    Short(i16),
    /// Type 4.
    Int(i32),
    /// Type 5.
    Long(i64),
    /// Type 6.
    Bytes(Vec<u8>),
    /// Type 7.
    String(String),
    /// Type 8, milliseconds since the Unix epoch.
    Timestamp(i64),
    /// Type 9.
    Uuid([u8; 16]),
}

/// One decoded message.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Frame {
    /// The headers in wire order.
    pub headers: Vec<(String, HeaderValue)>,
    /// The payload.
    pub payload: Vec<u8>,
}

impl Frame {
    /// The string header `name`, such as `:message-type`.
    pub fn string_header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find_map(|(have, value)| match value {
            HeaderValue::String(text) if have == name => Some(text.as_str()),
            _ => None,
        })
    }
}

/// Why a stream could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EventStreamError {
    /// A frame announces a length below the framing or above the cap.
    #[error("an event-stream frame announces {announced} bytes, outside 16..={MAX_FRAME_BYTES}")]
    FrameLength {
        /// The announced total length.
        announced: u32,
    },
    /// The announced header block does not fit the frame or exceeds the cap.
    #[error("an event-stream frame announces {announced} header bytes in a {total} byte frame")]
    HeadersLength {
        /// The announced header length.
        announced: u32,
        /// The announced total length.
        total: u32,
    },
    /// The prelude checksum does not match.
    #[error("an event-stream prelude fails its checksum")]
    PreludeChecksum,
    /// The message checksum does not match.
    #[error("an event-stream frame fails its checksum")]
    MessageChecksum,
    /// A header is cut short or has an unknown type.
    #[error("an event-stream frame has a malformed header")]
    MalformedHeader,
    /// The stream ended inside a frame.
    #[error("the event stream ended inside a frame")]
    Truncated,
}

fn crc(bytes: &[u8]) -> u32 {
    let mut hasher = Hasher::new();
    hasher.update(bytes);
    hasher.finalize()
}

fn be32(bytes: &[u8]) -> u32 {
    let mut word = [0u8; 4];
    word.copy_from_slice(&bytes[..4]);
    u32::from_be_bytes(word)
}

/// Incremental frame decoder.
#[derive(Debug, Default)]
pub struct Decoder {
    buffer: Vec<u8>,
    failed: Option<EventStreamError>,
}

impl Decoder {
    /// An empty decoder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `bytes` and returns every frame they complete.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Frame>, EventStreamError> {
        if let Some(error) = &self.failed {
            return Err(error.clone());
        }
        self.buffer.extend_from_slice(bytes);
        let mut frames = Vec::new();
        let mut consumed = 0;
        let outcome = loop {
            match decode_one(&self.buffer[consumed..]) {
                Ok(Some((frame, used))) => {
                    frames.push(frame);
                    consumed += used;
                }
                Ok(None) => break Ok(()),
                Err(error) => break Err(error),
            }
        };
        self.buffer.drain(..consumed);
        match outcome {
            Ok(()) => Ok(frames),
            Err(error) => {
                self.buffer = Vec::new();
                self.failed = Some(error.clone());
                Err(error)
            }
        }
    }

    /// Ends the stream: an error when it stopped inside a frame.
    pub fn finish(&self) -> Result<(), EventStreamError> {
        match (&self.failed, self.buffer.is_empty()) {
            (Some(error), _) => Err(error.clone()),
            (None, true) => Ok(()),
            (None, false) => Err(EventStreamError::Truncated),
        }
    }
}

/// One frame from the front of `bytes` and the bytes it used, or `None` when
/// more input is needed.
fn decode_one(bytes: &[u8]) -> Result<Option<(Frame, usize)>, EventStreamError> {
    if bytes.len() < PRELUDE_BYTES {
        return Ok(None);
    }
    let total = be32(bytes);
    let headers_len = be32(&bytes[4..]);
    if crc(&bytes[..8]) != be32(&bytes[8..]) {
        return Err(EventStreamError::PreludeChecksum);
    }
    let total_len = usize::try_from(total).unwrap_or(usize::MAX);
    if !(FRAMING_BYTES..=MAX_FRAME_BYTES).contains(&total_len) {
        return Err(EventStreamError::FrameLength { announced: total });
    }
    let headers_len_usize = usize::try_from(headers_len).unwrap_or(usize::MAX);
    if headers_len_usize > MAX_HEADER_BYTES || headers_len_usize > total_len - FRAMING_BYTES {
        return Err(EventStreamError::HeadersLength {
            announced: headers_len,
            total,
        });
    }
    if bytes.len() < total_len {
        return Ok(None);
    }
    let body_end = total_len - 4;
    if crc(&bytes[..body_end]) != be32(&bytes[body_end..]) {
        return Err(EventStreamError::MessageChecksum);
    }
    let headers_end = PRELUDE_BYTES + headers_len_usize;
    let headers = parse_headers(&bytes[PRELUDE_BYTES..headers_end])?;
    Ok(Some((
        Frame {
            headers,
            payload: bytes[headers_end..body_end].to_vec(),
        },
        total_len,
    )))
}

/// A cursor over the header block that fails with [`EventStreamError::MalformedHeader`].
struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], EventStreamError> {
        if self.0.len() < count {
            return Err(EventStreamError::MalformedHeader);
        }
        let (head, rest) = self.0.split_at(count);
        self.0 = rest;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], EventStreamError> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    fn sized(&mut self) -> Result<&'a [u8], EventStreamError> {
        let len = usize::from(u16::from_be_bytes(self.array()?));
        self.take(len)
    }
}

fn text(bytes: &[u8]) -> Result<String, EventStreamError> {
    String::from_utf8(bytes.to_vec()).map_err(|_| EventStreamError::MalformedHeader)
}

fn parse_headers(block: &[u8]) -> Result<Vec<(String, HeaderValue)>, EventStreamError> {
    let mut cursor = Cursor(block);
    let mut headers = Vec::new();
    while !cursor.0.is_empty() {
        let name_len = usize::from(cursor.array::<1>()?[0]);
        let name = text(cursor.take(name_len)?)?;
        let value = match cursor.array::<1>()?[0] {
            0 => HeaderValue::Bool(true),
            1 => HeaderValue::Bool(false),
            2 => HeaderValue::Byte(i8::from_be_bytes(cursor.array()?)),
            3 => HeaderValue::Short(i16::from_be_bytes(cursor.array()?)),
            4 => HeaderValue::Int(i32::from_be_bytes(cursor.array()?)),
            5 => HeaderValue::Long(i64::from_be_bytes(cursor.array()?)),
            6 => HeaderValue::Bytes(cursor.sized()?.to_vec()),
            7 => HeaderValue::String(text(cursor.sized()?)?),
            8 => HeaderValue::Timestamp(i64::from_be_bytes(cursor.array()?)),
            9 => HeaderValue::Uuid(cursor.array()?),
            _ => return Err(EventStreamError::MalformedHeader),
        };
        headers.push((name, value));
    }
    Ok(headers)
}

/// Writes `frame` as wire bytes. A header name or value above its length field
/// (255 bytes for a name, 65535 for a sized value) is cut to fit.
pub fn encode(frame: &Frame) -> Vec<u8> {
    let mut headers = Vec::new();
    for (name, value) in &frame.headers {
        let name = &name.as_bytes()[..name.len().min(usize::from(u8::MAX))];
        headers.push(u8::try_from(name.len()).unwrap_or(u8::MAX));
        headers.extend_from_slice(name);
        let sized = |headers: &mut Vec<u8>, tag: u8, bytes: &[u8]| {
            let bytes = &bytes[..bytes.len().min(usize::from(u16::MAX))];
            headers.push(tag);
            headers
                .extend_from_slice(&u16::try_from(bytes.len()).unwrap_or(u16::MAX).to_be_bytes());
            headers.extend_from_slice(bytes);
        };
        match value {
            HeaderValue::Bool(true) => headers.push(0),
            HeaderValue::Bool(false) => headers.push(1),
            HeaderValue::Byte(v) => headers.extend_from_slice(&[2, v.to_be_bytes()[0]]),
            HeaderValue::Short(v) => {
                headers.push(3);
                headers.extend_from_slice(&v.to_be_bytes());
            }
            HeaderValue::Int(v) => {
                headers.push(4);
                headers.extend_from_slice(&v.to_be_bytes());
            }
            HeaderValue::Long(v) => {
                headers.push(5);
                headers.extend_from_slice(&v.to_be_bytes());
            }
            HeaderValue::Bytes(v) => sized(&mut headers, 6, v),
            HeaderValue::String(v) => sized(&mut headers, 7, v.as_bytes()),
            HeaderValue::Timestamp(v) => {
                headers.push(8);
                headers.extend_from_slice(&v.to_be_bytes());
            }
            HeaderValue::Uuid(v) => {
                headers.push(9);
                headers.extend_from_slice(v);
            }
        }
    }
    let total =
        u32::try_from(FRAMING_BYTES + headers.len() + frame.payload.len()).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(usize::try_from(total).unwrap_or(0));
    out.extend_from_slice(&total.to_be_bytes());
    out.extend_from_slice(
        &u32::try_from(headers.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    out.extend_from_slice(&crc(&out[..8]).to_be_bytes());
    out.extend_from_slice(&headers);
    out.extend_from_slice(&frame.payload);
    out.extend_from_slice(&crc(&out).to_be_bytes());
    out
}

#[cfg(test)]
#[path = "eventstream_tests.rs"]
mod tests;
