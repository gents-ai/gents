//! The write-ahead log of [`super::MemEnv`]: a [`LogFile`] over a
//! [`MemFile`], with no lock.
//!
//! A `MemFile` grows at its end without a copy and nowhere else, so the log
//! keeps the file equal to the contiguous written prefix, as the serial log
//! does for an append-only host (`serial_log.rs`). What differs is that no
//! write waits for another:
//!
//! - **A write that starts at the file's end is appended there**, by a
//!   compare-and-swap that lands only while the file is still that long
//!   ([`MemFile::append_at`]). Two threads writing one record at once race
//!   for that append. The loser finds the file past its offset, and is
//!   done; or finds the winner still copying, takes the append over on a
//!   fresh extent, and the winner finds the file past its offset.
//! - **A write that starts above the end is held**: its bytes go into a
//!   lock-free map under their offset, and the call returns. The thread
//!   whose append reaches that offset appends them too.
//! - **A write that starts below the end** was written already (the
//!   contract makes its bytes the same) and is dropped.
//!
//! # Invariants
//!
//! - **ML1, the file holds exactly the contiguous written prefix**: every
//!   byte below its length, and nothing above it. A reader opened on the
//!   path sees a record once every record below it is written.
//! - **ML2, no held write that could be appended stays held.** A holder
//!   puts its bytes in the map and then looks at the file's end; an
//!   appender moves the end and then looks in the map. A fence sits between
//!   the two steps on each side, so one of them sees the other: the
//!   appender takes the bytes, or the holder finds the gap closed and
//!   appends them itself.
//! - **ML3, an entry of the map is of the record that starts at its key
//!   now.** A cut drops every entry at or above it, so no entry outlives a
//!   discarded commit to be appended in place of the one that takes its
//!   offset.
//! - **ML4, a write never waits for another thread** (see above).
//! - **ML5, bytes held are bounded by what the commit pipeline has in
//!   flight**: a write is held only while an earlier commit's write has
//!   not arrived. A holder that finds the file already past its offset (a
//!   thread writing the same record got there first) takes its own entry
//!   back out, so nothing that is written stays held.

use std::io;
use std::sync::Arc;

use kovan_map::HashMap;

use super::LogFile;
use super::mem_file::{AppendAt, Free, MemFile};
use crate::sync::internal::{Ordering, fence};

/// Buckets the map of held writes starts with; it grows on demand.
const HELD_BUCKETS: usize = 16;

/// See the module docs.
pub(super) struct MemLog {
    /// The file, which holds the contiguous written prefix (ML1).
    data: Arc<MemFile>,
    /// Writes that arrived ahead of a gap, by the offset they start at
    /// (ML2, ML3).
    held: HashMap<u64, Arc<[u8]>>,
}

fn cut_under_a_write() -> io::Error {
    io::Error::other("a log was cut back under a write that reaches the cut")
}

impl MemLog {
    /// A log over `data`, whose writes continue above what it holds.
    pub(super) fn new(data: Arc<MemFile>) -> Self {
        Self {
            data,
            held: HashMap::with_capacity(HELD_BUCKETS),
        }
    }

    /// Test-only: how many writes are held.
    #[cfg(test)]
    fn held_writes(&self) -> usize {
        self.held.len()
    }

    /// Keep `buf`, which starts above the file's end, until the writes
    /// below it arrive (ML2).
    fn hold(&self, offset: u64, buf: &[u8]) -> io::Result<()> {
        #[cfg(test)]
        if let Some(hook) = BEFORE_HOLD.with(|slot| slot.borrow_mut().take()) {
            hook();
        }
        let bytes: Arc<[u8]> = Arc::from(buf);
        self.held.insert(offset, Arc::clone(&bytes));
        // With the fence in `append_held`.
        fence(Ordering::SeqCst);
        if self.data.len() < offset {
            // Still above a gap: the append that reaches `offset` finds
            // the bytes.
            return Ok(());
        }
        // The gap closed while the bytes were on their way into the map.
        // If they are still there, the thread that closed it looked too
        // early and this one appends them, or finds them written by a
        // thread writing the same record (ML5). If they are gone, the
        // thread that took them appends them.
        if self
            .held
            .remove_if(&offset, |held| Arc::ptr_eq(held, &bytes))
            .is_none()
        {
            return Ok(());
        }
        match self.data.append_at(offset, &[&bytes], &Free)? {
            AppendAt::Appended(end) => self.append_held(end),
            AppendAt::Past => Ok(()),
            AppendAt::Short => Err(cut_under_a_write()),
        }
    }

    /// Append every held write that starts where the file ends, `end` being
    /// where an append of this thread's just ended (ML2).
    fn append_held(&self, mut end: u64) -> io::Result<()> {
        loop {
            // With the fence in `hold`.
            fence(Ordering::SeqCst);
            let Some(bytes) = self.held.remove(&end) else {
                return Ok(());
            };
            match self.data.append_at(end, &[&bytes], &Free)? {
                AppendAt::Appended(next) => end = next,
                // A thread writing the same record appended it: go on from
                // where the file ends now.
                AppendAt::Past => end = self.data.len(),
                AppendAt::Short => return Err(cut_under_a_write()),
            }
        }
    }
}

impl LogFile for MemLog {
    fn write_all_at(&self, offset: u64, buf: &[u8]) -> io::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        match self.data.append_at(offset, &[buf], &Free)? {
            AppendAt::Appended(end) => self.append_held(end),
            AppendAt::Past => Ok(()),
            AppendAt::Short => self.hold(offset, buf),
        }
    }

    /// Nothing in a `MemEnv` survives the process
    /// (`Capabilities::durable_sync`).
    fn sync_data(&self) -> io::Result<()> {
        Ok(())
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        if len < self.data.len() {
            self.data.set_len(len, &Free)?;
        }
        // What was held at or above the cut is of the commits the cut
        // discards (ML3). The contract keeps a write that reaches the cut
        // out while this runs, so nothing is put there meanwhile.
        let discarded: Vec<u64> = self.held.keys().filter(|at| *at >= len).collect();
        for at in discarded {
            self.held.remove(&at);
        }
        Ok(())
    }
}

#[cfg(test)]
thread_local! {
    /// Runs once, on this thread, inside its next write that found a gap
    /// below it, before the write's bytes go into the map: the window in
    /// which another thread may close the gap or write the same record.
    static BEFORE_HOLD: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `hook` inside this thread's next held write, before its bytes are
/// held.
#[cfg(test)]
fn before_hold(hook: impl FnOnce() + 'static) {
    BEFORE_HOLD.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}

#[cfg(test)]
#[path = "mem_log_tests.rs"]
mod tests;
