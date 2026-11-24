//! Write buffering over a [`WriteFile`].
//!
//! The engine used `std::io::BufWriter<File>` for the WAL, the
//! MANIFEST, and every SSTable. `BufWriter` needs `std::io::Write`,
//! which a positional [`WriteFile`] deliberately does not implement,
//! so this is the one replacement all three share. Buffering policy
//! matches `BufWriter`: an 8 KiB buffer, a write at least that large
//! bypasses it, and a drop flushes best-effort.

use std::io;

use super::WriteFile;

/// Default buffer size, matching `std::io::BufWriter`.
const DEFAULT_CAPACITY: usize = 8 * 1024;

/// A buffered writer over a [`WriteFile`].
pub(crate) struct BufferedWriter {
    inner: Box<dyn WriteFile>,
    buf: Vec<u8>,
    capacity: usize,
}

impl BufferedWriter {
    /// Wrap `inner` with the default 8 KiB buffer.
    pub(crate) fn new(inner: Box<dyn WriteFile>) -> Self {
        Self::with_capacity(DEFAULT_CAPACITY, inner)
    }

    /// Wrap `inner` with a buffer of `capacity` bytes. A capacity of
    /// zero writes straight through.
    pub(crate) fn with_capacity(capacity: usize, inner: Box<dyn WriteFile>) -> Self {
        Self {
            inner,
            buf: Vec::with_capacity(capacity),
            capacity,
        }
    }

    /// Buffer `data`, writing through when it does not fit.
    pub(crate) fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        if self.buf.len() + data.len() > self.capacity {
            self.drain()?;
        }
        if data.len() >= self.capacity {
            return self.inner.write_all(data);
        }
        self.buf.extend_from_slice(data);
        Ok(())
    }

    /// Drain the buffer to the file and flush the file.
    pub(crate) fn flush(&mut self) -> io::Result<()> {
        self.drain()?;
        self.inner.flush()
    }

    /// Drain the buffer and make every byte written so far durable.
    pub(crate) fn sync_all(&mut self) -> io::Result<()> {
        self.drain()?;
        self.inner.sync_all()
    }

    fn drain(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        // Reuse the allocation: `clear` after the write keeps the
        // buffer's capacity for the next batch, which is what makes
        // this cheap enough to sit on the per-record WAL path.
        let result = self.inner.write_all(&self.buf);
        self.buf.clear();
        result
    }
}

impl Drop for BufferedWriter {
    fn drop(&mut self) {
        // Matches `BufWriter`: a drop flushes what it can and cannot
        // report a failure. Every path that needs the bytes to have
        // landed calls `flush` or `sync_all` explicitly first.
        let _ = self.drain();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::{WriteMode, std_env};
    use tempfile::TempDir;

    fn open(dir: &TempDir, name: &str) -> BufferedWriter {
        let env = std_env();
        let file = env
            .open_write(&dir.path().join(name), WriteMode::Truncate)
            .unwrap();
        BufferedWriter::new(file)
    }

    #[test]
    fn small_writes_are_buffered_until_flush() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("buffered");
        let mut w = open(&dir, "buffered");
        w.write_all(b"hello").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        w.flush().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
    }

    #[test]
    fn a_write_at_least_the_buffer_size_goes_straight_through() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("big");
        let mut w = open(&dir, "big");
        let big = vec![7u8; DEFAULT_CAPACITY];
        w.write_all(&big).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            DEFAULT_CAPACITY as u64
        );
    }

    #[test]
    fn interleaved_small_and_large_writes_keep_byte_order() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("order");
        let mut w = open(&dir, "order");
        let big = vec![0xAB; DEFAULT_CAPACITY + 1];
        w.write_all(b"head").unwrap();
        w.write_all(&big).unwrap();
        w.write_all(b"tail").unwrap();
        w.flush().unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..4], b"head");
        assert!(bytes[4..4 + big.len()].iter().all(|&b| b == 0xAB));
        assert_eq!(&bytes[4 + big.len()..], b"tail");
    }

    #[test]
    fn drop_flushes_what_was_buffered() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("dropped");
        {
            let mut w = open(&dir, "dropped");
            w.write_all(b"landed").unwrap();
        }
        assert_eq!(std::fs::read(&path).unwrap(), b"landed");
    }
}
