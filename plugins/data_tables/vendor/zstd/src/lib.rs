//! A pure Rust stand-in for the `zstd` crate, covering the calls `parquet` and
//! `arrow-ipc` make (`bulk::Compressor`, `bulk::Decompressor` and a few
//! `zstd_safe` helpers). It is built on `ruzstd`, so the plugin compiles for
//! `wasm32-wasip1` without a C toolchain and still reads zstd-compressed
//! Parquet and Arrow files, whichever encoder wrote them.
//!
//! Decompression honours the capacity the caller gives, so a frame that
//! declares a small size and expands past it is an error, never an allocation.

use std::io::{self, Cursor, Read};
use std::ops::RangeInclusive;

/// The lowest and highest compression level a caller may ask for.
pub fn compression_level_range() -> RangeInclusive<i32> {
    1..=22
}

/// The helpers `parquet` reaches through `zstd::zstd_safe`.
pub mod zstd_safe {
    use super::Cursor;

    /// The frame header named no usable content size.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ContentSizeError;

    /// A destination `compress_to_buffer` appends to.
    pub trait WriteBuf {
        /// Appends `data` and returns how many bytes were written.
        fn append(&mut self, data: &[u8]) -> usize;
    }

    impl WriteBuf for Cursor<&mut Vec<u8>> {
        fn append(&mut self, data: &[u8]) -> usize {
            let at = usize::try_from(self.position()).unwrap_or(usize::MAX);
            let buf = self.get_mut();
            buf.truncate(at.min(buf.len()));
            buf.extend_from_slice(data);
            let end = buf.len() as u64;
            self.set_position(end);
            data.len()
        }
    }

    /// The most bytes compressing `len` bytes can produce (the reference bound).
    pub fn compress_bound(len: usize) -> usize {
        len + (len >> 8)
            + if len < (128 << 10) {
                ((128 << 10) - len) >> 11
            } else {
                0
            }
    }

    /// The content size a zstd frame header declares: `Ok(None)` when it
    /// declares none, an error when `src` does not start with a zstd frame.
    pub fn get_frame_content_size(src: &[u8]) -> Result<Option<u64>, ContentSizeError> {
        const MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];
        if src.len() < 5 || src[..4] != MAGIC {
            return Err(ContentSizeError);
        }
        let descriptor = src[4];
        let single_segment = descriptor & 0x20 != 0;
        let dictionary_id_len = [0usize, 1, 2, 4][usize::from(descriptor & 3)];
        let size_len = match descriptor >> 6 {
            0 => usize::from(single_segment),
            1 => 2,
            2 => 4,
            _ => 8,
        };
        if size_len == 0 {
            return Ok(None);
        }
        let at = 5 + usize::from(!single_segment) + dictionary_id_len;
        let field = src.get(at..at + size_len).ok_or(ContentSizeError)?;
        let mut bytes = [0u8; 8];
        bytes[..size_len].copy_from_slice(field);
        let value = u64::from_le_bytes(bytes);
        Ok(Some(if size_len == 2 { value + 256 } else { value }))
    }
}

/// One-shot compression and decompression of a whole buffer.
pub mod bulk {
    use super::zstd_safe::WriteBuf;
    use super::{io, Read};
    use ruzstd::decoding::StreamingDecoder;
    use ruzstd::encoding::{compress_to_vec, CompressionLevel};
    use std::marker::PhantomData;

    /// Compresses buffers into zstd frames.
    pub struct Compressor<'a> {
        level: CompressionLevel,
        _scope: PhantomData<&'a ()>,
    }

    impl Compressor<'_> {
        /// A compressor; every level is accepted and runs the encoder's only
        /// implemented effort, `Fastest`.
        pub fn new(_level: i32) -> io::Result<Self> {
            Ok(Self {
                level: CompressionLevel::Fastest,
                _scope: PhantomData,
            })
        }

        /// Compresses `source` into one frame.
        pub fn compress(&mut self, source: &[u8]) -> io::Result<Vec<u8>> {
            Ok(compress_to_vec(source, self.level))
        }

        /// Compresses `source` and appends the frame to `destination`.
        pub fn compress_to_buffer<C: WriteBuf + ?Sized>(
            &mut self,
            source: &[u8],
            destination: &mut C,
        ) -> io::Result<usize> {
            let frame = self.compress(source)?;
            Ok(destination.append(&frame))
        }
    }

    /// Decompresses zstd frames.
    pub struct Decompressor<'a> {
        _scope: PhantomData<&'a ()>,
    }

    impl Decompressor<'_> {
        /// A decompressor.
        pub fn new() -> io::Result<Self> {
            Ok(Self {
                _scope: PhantomData,
            })
        }

        /// Decompresses `source`; it is an error when the frame holds more than `capacity` bytes.
        pub fn decompress(&mut self, source: &[u8], capacity: usize) -> io::Result<Vec<u8>> {
            let mut decoder = StreamingDecoder::new(source)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
            let mut out = Vec::with_capacity(capacity.min(1 << 26));
            let limit = capacity as u64;
            decoder
                .by_ref()
                .take(limit.saturating_add(1))
                .read_to_end(&mut out)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
            if out.len() > capacity {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "the zstd frame holds more than its declared size",
                ));
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::bulk::{Compressor, Decompressor};
    use super::zstd_safe::{compress_bound, get_frame_content_size};

    /// `printf 'hello zstd hello zstd hello zstd' | zstd -3 -c` (no declared size) from the reference encoder.
    const STREAMED: [u8; 30] = [
        0x28, 0xb5, 0x2f, 0xfd, 0x04, 0x58, 0x8d, 0x00, 0x00, 0x58, 0x68, 0x65, 0x6c, 0x6c, 0x6f,
        0x20, 0x7a, 0x73, 0x74, 0x64, 0x20, 0x01, 0x00, 0xfe, 0x8a, 0x17, 0x0a, 0x03, 0x22, 0x8e,
    ];

    /// 300 times `a` through `zstd -3 -c file` (declares its size) from the reference encoder.
    const SIZED: [u8; 23] = [
        0x28, 0xb5, 0x2f, 0xfd, 0x64, 0x2c, 0x00, 0x4d, 0x00, 0x00, 0x10, 0x61, 0x61, 0x01, 0x00,
        0x27, 0x2a, 0xc0, 0x02, 0xc7, 0xcf, 0xcf, 0xb9,
    ];

    #[test]
    fn rejects_a_non_frame() {
        assert!(get_frame_content_size(b"not zstd").is_err());
        assert!(get_frame_content_size(&[0x28, 0xb5, 0x2f, 0xfd]).is_err());
    }

    #[test]
    fn round_trips_within_the_capacity_given() {
        let data: Vec<u8> = (0..100_000u32)
            .flat_map(|i| (i % 251).to_le_bytes())
            .collect();
        let frame = Compressor::new(3).unwrap().compress(&data).unwrap();
        assert!(frame.len() < data.len());
        assert!(compress_bound(data.len()) >= frame.len());
        let back = Decompressor::new()
            .unwrap()
            .decompress(&frame, data.len())
            .unwrap();
        assert_eq!(back, data);
    }

    #[test]
    fn a_frame_larger_than_its_capacity_is_an_error() {
        let data = vec![7u8; 10_000];
        let frame = Compressor::new(1).unwrap().compress(&data).unwrap();
        assert!(Decompressor::new()
            .unwrap()
            .decompress(&frame, 100)
            .is_err());
        assert!(Decompressor::new()
            .unwrap()
            .decompress(&frame, 10_000)
            .is_ok());
    }

    #[test]
    fn corrupt_input_is_an_error_not_a_panic() {
        let mut frame = Compressor::new(1)
            .unwrap()
            .compress(&vec![1u8; 5000])
            .unwrap();
        for i in 0..frame.len() {
            let saved = frame[i];
            frame[i] ^= 0xff;
            let _ = Decompressor::new().unwrap().decompress(&frame, 5000);
            frame[i] = saved;
        }
        assert!(Decompressor::new()
            .unwrap()
            .decompress(&frame[..frame.len() / 2], 5000)
            .is_err());
    }

    #[test]
    fn reads_frames_written_by_the_reference_encoder() {
        assert_eq!(get_frame_content_size(&STREAMED), Ok(None));
        assert_eq!(get_frame_content_size(&SIZED), Ok(Some(300)));
        let mut d = Decompressor::new().unwrap();
        assert_eq!(
            d.decompress(&STREAMED, 64).unwrap(),
            b"hello zstd hello zstd hello zstd"
        );
        assert_eq!(d.decompress(&SIZED, 300).unwrap(), vec![b'a'; 300]);
        assert!(d.decompress(&SIZED, 299).is_err());
    }
}
