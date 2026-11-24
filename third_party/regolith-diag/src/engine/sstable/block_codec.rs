//! The codec each data block is written with.
//!
//! A table is written with one [`CompressionType`], but each data block
//! names its own codec in its first byte, and the reader decodes each block
//! by its own. An LZ4 or Snappy frame records the block's length in four
//! bytes, so a block too long for that, which only a value of about 4 GiB
//! makes (a merged value can grow that large), is written uncompressed
//! rather than with a length that does not fit.

use crate::options::CompressionType;

/// The codec a data block of `raw_len` bytes is written with, in a table
/// written with `compression`.
pub(super) fn block_codec(compression: CompressionType, raw_len: usize) -> CompressionType {
    #[cfg(test)]
    if FRAME_LIMIT
        .with(std::cell::Cell::get)
        .is_some_and(|limit| raw_len > limit)
    {
        return CompressionType::None;
    }
    let frames = match compression {
        CompressionType::None => true,
        // The frame starts with the block's length in four bytes.
        CompressionType::Lz4 => u32::try_from(raw_len).is_ok(),
        // Snappy bounds its output in four bytes as well, and refuses an
        // input whose bound does not fit.
        CompressionType::Snappy => snap::raw::max_compress_len(raw_len) != 0,
    };
    if frames {
        compression
    } else {
        CompressionType::None
    }
}

#[cfg(test)]
thread_local! {
    /// Test seam: while set, a block longer than this is one no compressed
    /// frame can hold.
    static FRAME_LIMIT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Test-only: run `f` with every block this thread writes that is longer
/// than `limit` treated as too long for a compressed frame, so a test
/// crosses the bound without a value of 4 GiB.
#[cfg(test)]
pub(super) fn frames_up_to<T>(limit: usize, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<usize>);
    impl Drop for Restore {
        fn drop(&mut self) {
            FRAME_LIMIT.with(|frame| frame.set(self.0));
        }
    }
    let _restore = Restore(FRAME_LIMIT.with(|frame| frame.replace(Some(limit))));
    f()
}

#[cfg(test)]
mod tests;
