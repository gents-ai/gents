//! Request-body gzip exactly as Bun 1.4.4 (`src/http/compress_body.rs`):
//! libdeflate level 6 while the worst-case output fits Bun's 512 KiB shared
//! buffer, zlib-ng's own gzip wrapper (level 6, memLevel 8) above it. The two
//! encoders emit different bytes, so the size switch is part of the wire
//! contract.

use std::mem::MaybeUninit;
use std::os::raw::c_int;
use std::ptr;

use libdeflater::{CompressionLvl, Compressor};
use libz_ng_sys as z;

/// Bun's `LIBDEFLATE_SHARED_BUFFER_LEN`: the largest bound libdeflate handles.
const LIBDEFLATE_SHARED_BUFFER_LEN: usize = 512 * 1024;
/// The compression level Bun uses on both paths.
const LEVEL: i32 = 6;
/// zlib `windowBits` 31: a gzip wrapper around a 32 KiB window.
const GZIP_WINDOW_BITS: c_int = 31;
/// zlib `memLevel` default.
const MEM_LEVEL: c_int = 8;

/// A gzip failure.
#[derive(Debug, thiserror::Error)]
pub enum CompressError {
    /// The body is larger than the encoder can take.
    #[error("the request body is too large to compress")]
    TooLarge,
    /// The encoder failed.
    #[error("the request body could not be compressed")]
    Encoder,
}

/// Gzips `body` the way Bun 1.4.4 does.
///
/// Synchronous by design: it is CPU-bound, so callers run it under
/// `tokio::task::spawn_blocking` for large bodies.
pub fn gzip(body: &[u8]) -> Result<Vec<u8>, CompressError> {
    let lvl = CompressionLvl::new(LEVEL).map_err(|_| CompressError::Encoder)?;
    let mut compressor = Compressor::new(lvl);
    let bound = compressor.gzip_compress_bound(body.len());
    if bound <= LIBDEFLATE_SHARED_BUFFER_LEN {
        let mut out = vec![0u8; bound];
        let n = compressor
            .gzip_compress(body, &mut out)
            .map_err(|_| CompressError::Encoder)?;
        out.truncate(n);
        return Ok(out);
    }
    zlib_ng_gzip(body)
}

/// Owns an initialised zlib-ng deflate stream and ends it on drop.
struct Deflate {
    strm: Box<MaybeUninit<z::z_stream>>,
}

impl Deflate {
    fn new() -> Result<Self, CompressError> {
        // Zeroed is the documented "use the default allocator" state; the fn
        // pointer fields are never read from Rust before init fills them.
        let mut strm = Box::new(MaybeUninit::<z::z_stream>::zeroed());
        // SAFETY: `strm` points to a zeroed z_stream, which is what
        // deflateInit2 requires of an uninitialised stream.
        let rc = unsafe {
            z::zng_deflateInit2(
                strm.as_mut_ptr(),
                LEVEL,
                z::Z_DEFLATED,
                GZIP_WINDOW_BITS,
                MEM_LEVEL,
                z::Z_DEFAULT_STRATEGY,
            )
        };
        if rc != z::Z_OK {
            return Err(CompressError::Encoder);
        }
        Ok(Self { strm })
    }

    fn compress(&mut self, body: &[u8]) -> Result<Vec<u8>, CompressError> {
        let len = u32::try_from(body.len()).map_err(|_| CompressError::TooLarge)?;
        let p = self.strm.as_mut_ptr();
        // SAFETY: `p` is an initialised stream (Deflate::new succeeded).
        let bound = unsafe { z::deflateBound(p, body.len() as _) } as usize;
        let cap = u32::try_from(bound).map_err(|_| CompressError::TooLarge)?;
        let mut out = vec![0u8; bound];
        // SAFETY: the stream is initialised; `body` and `out` outlive the
        // deflate call, zlib only reads next_in and writes next_out within
        // avail_in/avail_out, and the pointers are cleared afterwards.
        let rc = unsafe {
            (*p).next_in = body.as_ptr().cast_mut();
            (*p).avail_in = len;
            (*p).next_out = out.as_mut_ptr();
            (*p).avail_out = cap;
            let rc = z::deflate(p, z::Z_FINISH);
            let written = (*p).total_out;
            (*p).next_in = ptr::null_mut();
            (*p).next_out = ptr::null_mut();
            (rc, written)
        };
        if rc.0 != z::Z_STREAM_END {
            return Err(CompressError::Encoder);
        }
        out.truncate(rc.1);
        Ok(out)
    }
}

impl Drop for Deflate {
    fn drop(&mut self) {
        // SAFETY: the stream was initialised by Deflate::new and is ended once.
        unsafe { z::deflateEnd(self.strm.as_mut_ptr()) };
    }
}

fn zlib_ng_gzip(body: &[u8]) -> Result<Vec<u8>, CompressError> {
    Deflate::new()?.compress(body)
}
