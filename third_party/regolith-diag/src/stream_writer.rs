//! Bounded-memory streaming writes.
//!
//! [`crate::WriteBatch`] holds every operation until it is applied, so
//! the memory a batch costs is the size of the input. That is the right
//! shape when the input is small and the caller wants all of it to land
//! atomically, and the wrong shape when the input is a stream whose size
//! the caller does not control. On an [`crate::Options::embedded`]
//! budget of a few MiB it is the difference between working and not.
//!
//! [`StreamingWriter`] bounds the memory instead of the input: writes
//! accumulate until they reach a byte budget, then flush. Peak footprint
//! is the budget plus one operation, whatever the stream's length.

use crate::Result;
use crate::options::{DurabilityMode, WriteOptions};
use crate::{Db, WriteBatch};

/// How a [`StreamingWriter`] buffers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamOptions {
    /// Flush once buffered keys and values reach this many bytes.
    ///
    /// Peak footprint is this plus the one operation that crosses the
    /// line, so a budget below the largest single value bounds nothing:
    /// each write then flushes on its own.
    pub max_buffered_bytes: usize,
    /// Durability for each flush. `None` uses the database's default.
    ///
    /// [`DurabilityMode::Immediate`] fsyncs every flush, which for a
    /// stream means one fsync per budget's worth rather than one for the
    /// whole stream.
    pub durability: Option<DurabilityMode>,
}

impl Default for StreamOptions {
    fn default() -> Self {
        Self {
            max_buffered_bytes: 1 << 20,
            durability: None,
        }
    }
}

/// A write stream that bounds its own memory.
///
/// # Atomicity
///
/// This is the tradeoff the type exists to make, and it is not the
/// [`WriteBatch`] guarantee. Each flush is atomic; the stream as a whole
/// is not. A crash partway through leaves every completed flush applied
/// and the rest absent, so recovery reaches a **valid prefix of the
/// stream**, never a half-applied flush and never an interleaving that
/// was not written. A caller that needs all-or-nothing across the whole
/// input wants a single `WriteBatch`, and has to pay the memory for it.
///
/// Dropping without calling [`StreamingWriter::finish`] discards
/// whatever is still buffered. Everything already flushed stays applied,
/// because it is already durable.
pub struct StreamingWriter<'db> {
    db: &'db Db,
    batch: WriteBatch,
    buffered: usize,
    opts: StreamOptions,
    /// Sequence of the most recent flush, so `finish` can report where
    /// the stream landed without the caller tracking it.
    last_sequence: u64,
}

impl<'db> StreamingWriter<'db> {
    pub(crate) fn new(db: &'db Db, opts: StreamOptions) -> Self {
        Self {
            db,
            batch: WriteBatch::new(),
            buffered: 0,
            opts,
            last_sequence: 0,
        }
    }

    /// Buffer a put, flushing first if it would exceed the budget.
    pub fn put(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        self.put_owned(key, value.to_vec())
    }

    /// [`StreamingWriter::put`] for a value the caller already owns.
    ///
    /// Takes the buffer rather than copying it, so a producer that just
    /// built the bytes hands them straight through to the batch.
    pub fn put_owned(&mut self, key: &[u8], value: Vec<u8>) -> Result<()> {
        self.buffered += key.len() + value.len();
        self.batch.put_owned(key, value);
        self.flush_if_full()
    }

    /// Buffer a delete, flushing first if it would exceed the budget.
    pub fn delete(&mut self, key: &[u8]) -> Result<()> {
        self.buffered += key.len();
        self.batch.delete(key);
        self.flush_if_full()
    }

    /// Bytes currently buffered and not yet applied.
    pub fn buffered_bytes(&self) -> usize {
        self.buffered
    }

    /// Apply everything buffered so far.
    ///
    /// A no-op on an empty buffer, so calling it on a cadence of the
    /// caller's own is cheap.
    pub fn flush(&mut self) -> Result<()> {
        if self.batch.is_empty() {
            return Ok(());
        }
        let batch = std::mem::take(&mut self.batch);
        self.buffered = 0;
        self.last_sequence = match self.opts.durability {
            // `sync` is the per-call form of `DurabilityMode::Immediate`,
            // so an explicit `Eventual` here means "do not fsync this
            // flush" rather than "fall back to the database default".
            Some(durability) => {
                let opts = WriteOptions {
                    sync: matches!(durability, DurabilityMode::Immediate),
                    ..WriteOptions::default()
                };
                self.db.write_sequenced_opt(&opts, batch)?
            }
            None => self.db.write_sequenced(batch)?,
        };
        Ok(())
    }

    /// Flush anything still buffered and report the sequence the stream
    /// ended at, so an upper layer can order its own versions against the
    /// store.
    ///
    /// Zero when the stream wrote nothing.
    pub fn finish(mut self) -> Result<u64> {
        self.flush()?;
        Ok(self.last_sequence)
    }

    fn flush_if_full(&mut self) -> Result<()> {
        if self.buffered >= self.opts.max_buffered_bytes {
            self.flush()?;
        }
        Ok(())
    }
}

impl std::fmt::Debug for StreamingWriter<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamingWriter")
            .field("buffered", &self.buffered)
            .field("max_buffered_bytes", &self.opts.max_buffered_bytes)
            .finish_non_exhaustive()
    }
}
