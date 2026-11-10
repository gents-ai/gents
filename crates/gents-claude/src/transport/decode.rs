//! Push-style content decoders (gzip, deflate, brotli, zstd). Each call takes
//! as much input as it can and fills a caller buffer, so a streamed body is
//! decoded as it arrives and memory stays bounded by the output buffer.

use brotli_decompressor::{BrotliDecompressStream, BrotliResult, BrotliState, StandardAlloc};
use flate2::{Crc, Decompress, FlushDecompress, Status};
use zstd::stream::raw::{Decoder as ZstdDecoder, Operation};

use super::error::TransportError;

type BrotliStd = BrotliState<StandardAlloc, StandardAlloc, StandardAlloc>;

/// A response content decoder.
pub(crate) struct Decoder {
    kind: Kind,
    fed: bool,
}

enum Kind {
    Identity,
    Gzip(Box<Gzip>),
    Zlib(Inflate),
    Brotli(Box<BrotliStd>, bool),
    Zstd(Box<ZstdDecoder<'static>>, bool),
}

impl Decoder {
    /// The decoder for a `Content-Encoding` value; unknown encodings are an error.
    pub(crate) fn for_encoding(value: Option<&str>) -> Result<Self, TransportError> {
        let kind = match value.map(str::trim) {
            None | Some("") => Kind::Identity,
            Some(v) if v.eq_ignore_ascii_case("identity") => Kind::Identity,
            Some(v) if v.eq_ignore_ascii_case("gzip") || v.eq_ignore_ascii_case("x-gzip") => {
                Kind::Gzip(Box::new(Gzip::new()))
            }
            Some(v) if v.eq_ignore_ascii_case("deflate") => Kind::Zlib(Inflate::new(true)),
            Some(v) if v.eq_ignore_ascii_case("br") => Kind::Brotli(
                Box::new(BrotliState::new(
                    StandardAlloc::default(),
                    StandardAlloc::default(),
                    StandardAlloc::default(),
                )),
                false,
            ),
            Some(v) if v.eq_ignore_ascii_case("zstd") => Kind::Zstd(
                Box::new(ZstdDecoder::new().map_err(|_| TransportError::Decode)?),
                false,
            ),
            Some(_) => return Err(TransportError::UnsupportedEncoding),
        };
        Ok(Self { kind, fed: false })
    }

    pub(crate) fn is_identity(&self) -> bool {
        matches!(self.kind, Kind::Identity)
    }

    /// Whether the encoded stream ended cleanly (or never carried a byte).
    pub(crate) fn complete(&self) -> bool {
        if !self.fed {
            return true;
        }
        match &self.kind {
            Kind::Identity => true,
            Kind::Gzip(g) => g.done,
            Kind::Zlib(z) => z.done,
            Kind::Brotli(_, done) => *done,
            Kind::Zstd(_, ended) => *ended,
        }
    }

    /// Consumes input (advancing the slice) and writes decoded bytes to `out`.
    pub(crate) fn decode(
        &mut self,
        input: &mut &[u8],
        out: &mut [u8],
    ) -> Result<usize, TransportError> {
        self.fed |= !input.is_empty();
        match &mut self.kind {
            Kind::Identity => Err(TransportError::Decode),
            Kind::Gzip(g) => g.run(input, out),
            Kind::Zlib(z) => z.run(input, out),
            Kind::Brotli(state, done) => {
                if *done {
                    return trailing(input);
                }
                let (mut avail_in, mut in_off) = (input.len(), 0);
                let (mut avail_out, mut out_off, mut total) = (out.len(), 0, 0);
                let r = BrotliDecompressStream(
                    &mut avail_in,
                    &mut in_off,
                    input,
                    &mut avail_out,
                    &mut out_off,
                    out,
                    &mut total,
                    state,
                );
                *input = &input[in_off..];
                match r {
                    BrotliResult::ResultSuccess => *done = true,
                    BrotliResult::ResultFailure => return Err(TransportError::Decode),
                    BrotliResult::NeedsMoreInput | BrotliResult::NeedsMoreOutput => {}
                }
                Ok(out_off)
            }
            Kind::Zstd(dec, ended) => {
                let st = dec
                    .run_on_buffers(input, out)
                    .map_err(|_| TransportError::Decode)?;
                *input = &input[st.bytes_read..];
                // An empty-input call after a frame reports the next frame's
                // header hint, so only a call that read input can reopen it.
                if st.remaining == 0 {
                    *ended = true;
                } else if st.bytes_read > 0 {
                    *ended = false;
                }
                Ok(st.bytes_written)
            }
        }
    }
}

/// A flate2 inflater that stops at the end of its stream.
struct Inflate {
    d: Decompress,
    done: bool,
}

impl Inflate {
    fn new(zlib_header: bool) -> Self {
        Self {
            d: Decompress::new(zlib_header),
            done: false,
        }
    }

    fn run(&mut self, input: &mut &[u8], out: &mut [u8]) -> Result<usize, TransportError> {
        if self.done {
            return trailing(input);
        }
        let (bi, bo) = (self.d.total_in(), self.d.total_out());
        let st = self
            .d
            .decompress(input, out, FlushDecompress::None)
            .map_err(|_| TransportError::Decode)?;
        *input = &input[(self.d.total_in() - bi) as usize..];
        self.done = st == Status::StreamEnd;
        Ok((self.d.total_out() - bo) as usize)
    }
}

const FHCRC: u8 = 2;
const FEXTRA: u8 = 4;
const FNAME: u8 = 8;
const FCOMMENT: u8 = 16;

#[derive(Clone, Copy)]
enum Hdr {
    Fixed,
    ExtraLen,
    Skip(usize),
    UntilNul(u8),
    Crc16,
    Done,
}

/// A gzip decoder (concatenated members allowed): header state machine, raw inflate, CRC32/ISIZE trailer.
struct Gzip {
    hdr: Hdr,
    flags: u8,
    acc: [u8; 10],
    have: usize,
    inf: Inflate,
    crc: Crc,
    size: u32,
    in_trailer: bool,
    done: bool,
}

/// Bytes after the end of a single-stream encoding are corrupt data.
fn trailing(input: &mut &[u8]) -> Result<usize, TransportError> {
    if input.is_empty() {
        Ok(0)
    } else {
        Err(TransportError::Decode)
    }
}

/// Copies input into `acc[have..want]`; true once `want` bytes are held.
fn take(input: &mut &[u8], acc: &mut [u8], have: &mut usize, want: usize) -> bool {
    let n = (want - *have).min(input.len());
    acc[*have..*have + n].copy_from_slice(&input[..n]);
    *have += n;
    *input = &input[n..];
    *have == want
}

impl Gzip {
    fn new() -> Self {
        Self {
            hdr: Hdr::Fixed,
            flags: 0,
            acc: [0; 10],
            have: 0,
            inf: Inflate::new(false),
            crc: Crc::new(),
            size: 0,
            in_trailer: false,
            done: false,
        }
    }

    /// Moves to the first optional header field after `after` that `flags` sets.
    fn next_field(&mut self, after: u8) {
        self.have = 0;
        self.hdr = if after < FEXTRA && self.flags & FEXTRA != 0 {
            Hdr::ExtraLen
        } else if after < FNAME && self.flags & FNAME != 0 {
            Hdr::UntilNul(FNAME)
        } else if after < FCOMMENT && self.flags & FCOMMENT != 0 {
            Hdr::UntilNul(FCOMMENT)
        } else if after < 32 && self.flags & FHCRC != 0 {
            Hdr::Crc16
        } else {
            Hdr::Done
        };
    }

    /// Consumes header bytes; true once the header is complete.
    fn header(&mut self, input: &mut &[u8]) -> Result<bool, TransportError> {
        loop {
            match self.hdr {
                Hdr::Fixed => {
                    if !take(input, &mut self.acc, &mut self.have, 10) {
                        return Ok(false);
                    }
                    if self.acc[..3] != [0x1f, 0x8b, 8] {
                        return Err(TransportError::Decode);
                    }
                    self.flags = self.acc[3];
                    self.next_field(0);
                }
                Hdr::ExtraLen => {
                    if !take(input, &mut self.acc, &mut self.have, 2) {
                        return Ok(false);
                    }
                    self.hdr =
                        Hdr::Skip(usize::from(u16::from_le_bytes([self.acc[0], self.acc[1]])));
                }
                Hdr::Skip(n) => {
                    let k = n.min(input.len());
                    *input = &input[k..];
                    if k < n {
                        self.hdr = Hdr::Skip(n - k);
                        return Ok(false);
                    }
                    self.next_field(FEXTRA);
                }
                Hdr::UntilNul(field) => match input.iter().position(|&b| b == 0) {
                    Some(i) => {
                        *input = &input[i + 1..];
                        self.next_field(field);
                    }
                    None => {
                        *input = &[];
                        return Ok(false);
                    }
                },
                Hdr::Crc16 => {
                    if !take(input, &mut self.acc, &mut self.have, 2) {
                        return Ok(false);
                    }
                    self.next_field(32);
                }
                Hdr::Done => return Ok(true),
            }
        }
    }

    fn run(&mut self, input: &mut &[u8], out: &mut [u8]) -> Result<usize, TransportError> {
        if self.done {
            if input.is_empty() {
                return Ok(0);
            }
            // RFC 1952: another member may follow; anything else fails its header check.
            *self = Self::new();
        }
        if !self.header(input)? {
            return Ok(0);
        }
        let mut n = 0;
        if !self.inf.done {
            n = self.inf.run(input, out)?;
            self.crc.update(&out[..n]);
            self.size = self.size.wrapping_add(n as u32);
        }
        if self.inf.done {
            if !self.in_trailer {
                self.in_trailer = true;
                self.have = 0;
            }
            self.trailer(input)?;
        }
        Ok(n)
    }

    fn trailer(&mut self, input: &mut &[u8]) -> Result<(), TransportError> {
        if !take(input, &mut self.acc, &mut self.have, 8) {
            return Ok(());
        }
        let crc = u32::from_le_bytes([self.acc[0], self.acc[1], self.acc[2], self.acc[3]]);
        let size = u32::from_le_bytes([self.acc[4], self.acc[5], self.acc[6], self.acc[7]]);
        if crc != self.crc.sum() || size != self.size {
            return Err(TransportError::Decode);
        }
        self.done = true;
        Ok(())
    }
}
