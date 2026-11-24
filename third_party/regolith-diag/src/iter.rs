//! Public streaming iterator over a consistent snapshot of the database.
//!
//! An [`Iter`] is obtained from [`Db::iter`](crate::Db::iter) or
//! [`Snapshot::iter`](crate::Snapshot::iter) and yields user keys in
//! ascending order. It honors MVCC snapshot visibility - keys written
//! after the iterator was created are invisible - and hides tombstoned
//! keys from the stream. The iterator is safe against concurrent
//! background compaction: it holds pinned references to every SSTable
//! file that existed at creation time, so compaction can unlink files
//! without invalidating in-flight reads.

use std::marker::PhantomData;
use std::sync::Arc;

use crate::engine::RegolithEngine;
use crate::engine::iterator::RegolithIterator;
use crate::statistics::{Histogram, Statistics, Ticker, TimeScope};
use crate::{DbSlice, ReadMode, Result};

/// Streaming iterator over a consistent view of the database.
///
/// Unlike [`Db::scan`](crate::Db::scan), which materializes an entire
/// result set at once, `Iter` yields entries on demand and bounds memory
/// by the size of a single block. Use [`Db::scan_page`](crate::Db::scan_page)
/// when a Vec-backed page is more convenient than manually driving the
/// iterator.
///
/// # Lifecycle
///
/// A fresh iterator is not positioned; call one of [`seek`](Self::seek),
/// [`seek_for_prev`](Self::seek_for_prev), or
/// [`seek_to_first`](Self::seek_to_first) before reading.
///
/// # Example
///
/// ```
/// use regolith::{Db, Options};
///
/// let db = Db::open("fruit", Options::memory()).unwrap();
/// db.put(b"apple", b"red").unwrap();
/// db.put(b"banana", b"yellow").unwrap();
/// db.put(b"cherry", b"red").unwrap();
///
/// // From the first key at or after `b`, in key order.
/// let mut seen = Vec::new();
/// let mut it = db.iter();
/// it.seek(b"b");
/// while it.valid() {
///     let (key, value) = (it.key().unwrap(), it.value().unwrap());
///     seen.push(format!("{} is {}", String::from_utf8_lossy(key), String::from_utf8_lossy(value)));
///     it.next();
/// }
/// // The walk ended at the last key, not on an error.
/// it.status().unwrap();
/// assert_eq!(seen, ["banana is yellow", "cherry is red"]);
/// ```
///
/// # Non-blocking walks
///
/// An iterator made from a snapshot read in [`ReadMode::CacheOnly`] never
/// touches the device. A seek or a step that needs a block the cache does not
/// hold stops, [`valid`](Self::valid) turns `false`, and
/// [`status`](Self::status) reports [`crate::Error::WouldBlock`]. Poll the
/// queue it names, then call [`resume`](Self::resume): the interrupted seek
/// or step runs again and the walk carries on exactly where it would have,
/// skipping nothing and repeating nothing.
pub struct Iter<'a> {
    inner: RegolithIterator,
    /// Optional statistics sink captured from the parent `Db`'s
    /// options at construction time. Seek / next / prev all fire
    /// ticker increments and timing histograms through this
    /// handle, guarded by a single `Option` branch.
    stats: Option<Arc<Statistics>>,
    /// Where this iterator's block-cache misses go.
    mode: ReadMode,
    // Ties the iterator's lifetime to its parent `Db` / `Snapshot` so the
    // borrow checker prevents the iterator from outliving the engine it
    // was built from.
    _marker: PhantomData<&'a RegolithEngine>,
}

impl<'a> Iter<'a> {
    pub(crate) fn from_internal(inner: RegolithIterator) -> Self {
        Self {
            inner,
            stats: None,
            mode: ReadMode::Blocking,
            _marker: PhantomData,
        }
    }

    pub(crate) fn with_stats(mut self, stats: Option<Arc<Statistics>>) -> Self {
        self.stats = stats;
        self
    }

    /// Read in `mode` from now on.
    pub(crate) fn with_mode(mut self, mode: ReadMode) -> Self {
        self.mode = mode;
        self.resumable(mode.is_cache_only())
    }

    /// Keep what [`Iter::resume`] needs, for an iterator whose caller sets
    /// the read mode around each call itself.
    pub(crate) fn resumable(mut self, on: bool) -> Self {
        self.inner.track_resume(on);
        self
    }

    /// Run the seek or step a [`crate::WouldBlock`] interrupted again, once
    /// the queue it names was polled.
    ///
    /// The walk continues exactly where the interrupted call would have left
    /// it: after a step, on the entry that step would have reached, nothing
    /// skipped and nothing repeated. It may stop on `WouldBlock` again, for
    /// another block. Does nothing when the last call did not stop on one.
    pub fn resume(&mut self) {
        let _scope = self.mode.scope();
        self.inner.resume();
    }

    fn tick_seek(&self) {
        if let Some(s) = self.stats.as_deref() {
            s.add(Ticker::IterSeekCount, 1);
        }
    }

    fn tick_next(&self) {
        if let Some(s) = self.stats.as_deref() {
            s.add(Ticker::IterNextCount, 1);
        }
    }

    /// Position the iterator at the first user key `>= target`. Sets the
    /// scan direction to forward, so subsequent [`next`](Self::next)
    /// calls advance alphabetically.
    pub fn seek(&mut self, target: &[u8]) {
        self.tick_seek();
        let _t = TimeScope::new(self.stats.as_deref(), Histogram::DbIterSeek);
        let _scope = self.mode.scope();
        self.inner.seek(target);
    }

    /// Position the iterator at the first user key `>= target`, and end
    /// forward iteration before the first user key `>= upper_bound`.
    ///
    /// Prefer this to [`seek`](Self::seek) plus a bound check in the caller
    /// whenever the range has an end: the iterator skips deleted and
    /// not-yet-visible entries internally, and only a bound it knows about
    /// stops that skipping at the end of the range.
    pub fn seek_bounded(&mut self, target: &[u8], upper_bound: &[u8]) {
        self.tick_seek();
        let _t = TimeScope::new(self.stats.as_deref(), Histogram::DbIterSeek);
        let _scope = self.mode.scope();
        self.inner.seek_bounded(target, upper_bound);
    }

    /// Position the iterator at the largest user key `<= target`. Sets
    /// the scan direction to reverse, so subsequent [`prev`](Self::prev)
    /// calls walk backward. Calling [`next`](Self::next) after
    /// `seek_for_prev` flips direction and moves alphabetically forward.
    pub fn seek_for_prev(&mut self, target: &[u8]) {
        self.tick_seek();
        let _t = TimeScope::new(self.stats.as_deref(), Histogram::DbIterSeek);
        let _scope = self.mode.scope();
        self.inner.seek_for_prev(target);
    }

    /// Position the iterator at the first user key starting with
    /// `prefix` and confine forward iteration to keys that share that
    /// prefix. The scan ends (iterator becomes invalid) as soon as the
    /// next candidate key falls outside the prefix range.
    ///
    /// When the database was built with a [`PrefixExtractor`] matching
    /// the prefix width, SSTables that demonstrably cannot contain the
    /// prefix are skipped via the prefix bloom filter. Files without a
    /// prefix bloom are consulted normally (conservative superset).
    /// Point lookups are unaffected.
    ///
    /// [`PrefixExtractor`]: crate::PrefixExtractor
    pub fn seek_prefix(&mut self, prefix: &[u8]) {
        self.tick_seek();
        let _t = TimeScope::new(self.stats.as_deref(), Histogram::DbIterSeek);
        let _scope = self.mode.scope();
        self.inner.seek_prefix(prefix);
    }

    /// Position the iterator at the smallest user key in the database.
    /// Sets the scan direction to forward.
    pub fn seek_to_first(&mut self) {
        self.tick_seek();
        let _t = TimeScope::new(self.stats.as_deref(), Histogram::DbIterSeek);
        let _scope = self.mode.scope();
        self.inner.seek_to_first();
    }

    /// Position the iterator at the largest user key in the database.
    /// Sets the scan direction to reverse.
    pub fn seek_to_last(&mut self) {
        self.tick_seek();
        let _t = TimeScope::new(self.stats.as_deref(), Histogram::DbIterSeek);
        let _scope = self.mode.scope();
        self.inner.seek_to_last();
    }

    /// Position the iterator at the last user key strictly below
    /// `exclusive_upper`, and set the scan direction to reverse. Used
    /// by the column-family iterator, whose upper bound is exclusive.
    pub(crate) fn seek_to_last_before(&mut self, exclusive_upper: &[u8]) {
        self.tick_seek();
        let _t = TimeScope::new(self.stats.as_deref(), Histogram::DbIterSeek);
        let _scope = self.mode.scope();
        self.inner.seek_to_last_before(exclusive_upper);
    }

    /// Advance to the next user key alphabetically. If the iterator was
    /// walking backward, direction flips before the advance. A no-op if
    /// the iterator is not valid.
    pub fn next(&mut self) {
        let _t = TimeScope::new(self.stats.as_deref(), Histogram::DbIterNext);
        let _scope = self.mode.scope();
        self.inner.next();
        // Only count the step when it produced a key - an
        // end-of-stream `next` that invalidates the iterator
        // should not inflate the counter.
        if self.inner.valid() {
            self.tick_next();
        }
    }

    /// Step back to the previous user key alphabetically. If the
    /// iterator was walking forward, direction flips before the step.
    /// A no-op if the iterator is not valid.
    pub fn prev(&mut self) {
        let _t = TimeScope::new(self.stats.as_deref(), Histogram::DbIterNext);
        let _scope = self.mode.scope();
        self.inner.prev();
        if self.inner.valid() {
            self.tick_next();
        }
    }

    /// Whether the caller has positioned this cursor at all.
    ///
    /// A cursor that was seeked past the end of the range is not valid, but
    /// it is positioned, and re-seeking it would undo what the caller asked
    /// for. Anything that seeks on the caller's behalf has to check this
    /// rather than [`Iter::valid`].
    ///
    /// Only the seek family sets it. Stepping with [`Iter::next`] or
    /// [`Iter::prev`] cannot be what first positions a cursor, because on one
    /// nobody placed the step is a no-op, so the flag stays off the per-entry
    /// path where it would cost something.
    pub fn positioned(&self) -> bool {
        self.inner.positioned()
    }

    /// Whether the iterator currently points at a valid `(key, value)`
    /// pair. Becomes `false` once the end of the stream is reached or an
    /// error is encountered.
    pub fn valid(&self) -> bool {
        self.inner.valid()
    }

    /// Returns the current user key, or `None` if the iterator isn't
    /// positioned on a live entry.
    pub fn key(&self) -> Option<&[u8]> {
        self.inner.key()
    }

    /// Returns the current value, or `None` if the iterator isn't
    /// positioned on a live entry.
    pub fn value(&self) -> Option<&[u8]> {
        self.inner.value()
    }

    /// Returns the current value as a [`DbSlice`], or `None` if the
    /// iterator isn't positioned on a live entry.
    ///
    /// Unlike [`Iter::value`], the returned slice does not borrow the
    /// iterator, so it stays valid after [`Iter::next`] moves on. While
    /// scanning SSTables forward this costs one reference count and no
    /// copy; a memtable-resident entry, a merge result and the reverse
    /// path all own their bytes separately and are copied once.
    ///
    /// Holding one pins its owner: see [`DbSlice`].
    pub fn value_slice(&self) -> Option<DbSlice> {
        self.inner.value_slice()
    }

    /// Returns `Ok(())` if the iterator has not encountered an I/O error,
    /// or the most recent error otherwise. An iterator that reports an
    /// error stops yielding entries (`valid()` returns `false`).
    pub fn status(&self) -> Result<()> {
        self.inner.status().map_err(crate::Error::from)
    }
}
