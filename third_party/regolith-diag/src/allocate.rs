//! Conflict-free allocation of ranges from a `u64` counter: [`Db::allocate`]
//! and [`Db::allocate_nowait`].

use std::ops::Range;

use crate::column_family::{DEFAULT_CF_ID, prefix_key};
use crate::{Db, Error, QueueId, Result};

impl Db {
    /// Reserves `n` consecutive values from the counter stored at `key` in
    /// the default column family, and returns them as a range.
    ///
    /// The key holds the last value reserved as an 8-byte big-endian `u64`,
    /// and absent means 0, so the first call on a new key returns `1..n + 1`.
    /// A later call returns a range that starts after the end of every range
    /// before it, so no value is returned twice and ranges only grow.
    /// `allocate(key, 0)` reserves nothing, writes nothing and returns an
    /// empty range at the next value.
    ///
    /// # Guarantees
    ///
    /// - **Never a conflict.** The reservation happens in the commit pipeline
    ///   and is not part of any transaction, so it cannot fail a transaction
    ///   or be failed by one, and concurrent callers all succeed.
    /// - **Unique across crashes.** The reservation takes its place in the
    ///   commit pipeline before this call returns, so its record goes to the
    ///   write-ahead log ahead of the record of any commit made after this
    ///   call, and a commit that uses one of its values is never durable
    ///   without it. A crash that keeps a commit using a value keeps the
    ///   reservation, and the counter recovers at or above every value in
    ///   use: after a crash no value that a surviving commit used is
    ///   returned again.
    /// - **Unique without a crash.** A write-ahead log failure that discards
    ///   the reservation after its range was returned writes its counter back
    ///   before any later commit is decided, so the range is not handed out
    ///   again; if that write fails too, the database stops taking writes.
    /// - **No I/O on this thread for its record.** The call writes and syncs
    ///   nothing: the record is written by the next call through the
    ///   pipeline (a commit, a flush, [`Db::close`]) and becomes durable with
    ///   the next sync, at the latest with the first commit that uses one of
    ///   its values. A value that was only handed to the outside world, and
    ///   not used by a committed write, can be returned again after a crash.
    /// - **Not stalled.** It does not wait while writes are stalled.
    /// - **Gaps.** Values reserved by code that never commits a use, such as
    ///   a transaction that aborts, are skipped for good.
    ///
    /// The key itself is not the authority: a [`Db::get`] of it right after
    /// this call may still show the counter before the reservation, until the
    /// reservation's record is written and published, which any later commit
    /// does. The range returned is what was reserved.
    ///
    /// # Where it waits
    ///
    /// It waits where a commit entering the pipeline waits: for room when
    /// the pipeline is full, and for an ordered piece of work ahead of it (a
    /// rotation of the memtable, an ingest's install). The first reservation
    /// of a key in a process reads the counter, which may read the device on
    /// this thread, as a blocking [`Db::get`] does; the database keeps the
    /// counter in memory from then on, so no later reservation of the key
    /// reads the device. It keeps up to 64 counters at once: a further one
    /// is read at each reservation, from memory while its last reservation
    /// is in the memtable. A write to the key that does not come from a
    /// reservation, or an ingest or [`Db::drop_all`], makes the next
    /// reservation read the key again. A caller that must never wait calls
    /// [`Db::allocate_nowait`].
    ///
    /// # Errors
    ///
    /// [`Error::InvalidArgument`], with nothing reserved, when `key` is
    /// longer than [`Options::max_key_size`](crate::Options::max_key_size),
    /// when the key holds anything but eight bytes, or when fewer than `n`
    /// values remain below `u64::MAX`. [`Error::ReadOnly`] and
    /// [`Error::Closed`] as for any write.
    ///
    /// # Preconditions
    ///
    /// Only this call writes the key. A write by anything else, a [`Db::put`]
    /// or a transaction included, can move the counter back or fill it with
    /// bytes that are no number, and regolith cannot tell.
    ///
    /// # Example
    ///
    /// ```
    /// use regolith::{Db, Error, Options};
    ///
    /// let db = Db::open("ids", Options::memory())?;
    ///
    /// // Ranges follow one another: no value comes back twice.
    /// assert_eq!(db.allocate(b"next-id", 3)?, 1..4);
    /// assert_eq!(db.allocate(b"next-id", 2)?, 4..6);
    /// // Asking for nothing reserves nothing.
    /// assert_eq!(db.allocate(b"next-id", 0)?, 6..6);
    ///
    /// // The key shows the last value reserved once a later write has passed
    /// // through (here a put), and each key is a counter of its own.
    /// db.put(b"marker", b"")?;
    /// assert_eq!(db.get(b"next-id")?, Some(5u64.to_be_bytes().to_vec()));
    /// assert_eq!(db.allocate(b"other", 1)?, 1..2);
    ///
    /// // A key that holds something else is refused, and left as it is.
    /// db.put(b"name", b"not a counter")?;
    /// assert!(matches!(db.allocate(b"name", 1), Err(Error::InvalidArgument(_))));
    /// assert_eq!(db.get(b"name")?.as_deref(), Some(&b"not a counter"[..]));
    /// # Ok::<(), regolith::Error>(())
    /// ```
    pub fn allocate(&self, key: &[u8], n: u64) -> Result<Range<u64>> {
        self.ensure_writable()?;
        self.validate_key_size(key)?;
        self.engine
            .allocate(prefix_key(DEFAULT_CF_ID, key), n)
            .map_err(Error::from)
    }

    /// [`Db::allocate`] for a caller that never waits and never reads the
    /// device: what would make it wait is handed back as
    /// [`Error::WouldBlock`] with a [`WouldBlock::Io`](crate::WouldBlock::Io)
    /// wait recorded on `queue`, which that queue's
    /// [`poll`](crate::IoQueue::poll) completes. Run the call again once the
    /// wait is ready.
    ///
    /// It never reads the device on this thread: a counter not in memory yet
    /// whose block is not cached (the first reservation of a key in a
    /// process) hands back a wait whose read the queue's poll does, and the
    /// call run again reserves. It also returns a wait when the pipeline is
    /// full, and when an ordered
    /// piece of work ahead of the reservation is under way on another thread
    /// or would read the device. A reservation it gave up for a wait is
    /// refused when its turn comes, or, when another thread had already made
    /// it, its values are skipped: a gap, never a value handed out twice.
    ///
    /// Every guarantee of [`Db::allocate`] holds for the range it returns.
    ///
    /// # Errors
    ///
    /// As [`Db::allocate`], and [`Error::InvalidArgument`] when `queue` is
    /// not open on this database.
    pub fn allocate_nowait(&self, queue: QueueId, key: &[u8], n: u64) -> Result<Range<u64>> {
        self.ensure_writable()?;
        self.validate_key_size(key)?;
        self.engine
            .allocate_nowait(queue, prefix_key(DEFAULT_CF_ID, key), n)
            .map_err(Error::from)
    }
}
