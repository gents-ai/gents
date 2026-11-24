//! A [`LogFile`] over an append-only [`WriteFile`]: what [`Env::open_log`]
//! hands out for an environment outside this crate that has not said how
//! it writes at an offset. No environment of this crate uses it: each has
//! a log no write of which waits for another.
//!
//! The file only ever grows at its end, so the writes are put in offset
//! order: one that starts at the end is appended, one that starts above it
//! is held until the writes below it arrive, and one wholly below it was
//! already written and is dropped (the contract makes its bytes the same).
//!
//! # Invariants
//!
//! - The file holds exactly the contiguous written prefix: every byte below
//!   `end`, and nothing above it, unless an append or a cut failed. That
//!   failure is latched: the file may then hold part of the append past
//!   `end`, with its write position past it too, so no append and no sync
//!   is taken until a cut ([`LogFile::set_len`]) succeeds and puts the file
//!   back at a known length.
//! - A held write starts above `end`. Filling the gap below it appends it in
//!   the same call, so no write that could be appended stays held.
//! - Bytes held are bounded by what the commit pipeline has in flight: a
//!   write is held only while an earlier commit's write has not arrived.
//!
//! One lock covers the file and the held writes: the file is appended to
//! through `&mut`, so one caller at a time has it. It is a
//! [`crate::sync::Mutex`], which no thread sleeps on in the kernel: free, it
//! is one compare-and-swap to take, and a caller that finds another thread's
//! device call in the way parks until that call returns. No device sync
//! runs under it except the caller's own [`LogFile::sync_data`].

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use super::{Env, LogFile, WriteFile, WriteMode};
use crate::sync::internal::wait_here;
use crate::sync::{Mutex, MutexGuard};

pub(super) struct SerialLog {
    inner: Mutex<Inner>,
}

struct Inner {
    file: Box<dyn WriteFile>,
    /// Bytes the file holds: where the next append lands.
    end: u64,
    /// Writes that arrived ahead of a gap, by the offset they start at.
    held: BTreeMap<u64, Vec<u8>>,
    /// The kind of the append or cut that failed, until a cut succeeds: a
    /// later append would land past the bytes it left (see the module docs).
    failed: Option<io::ErrorKind>,
}

#[cfg(test)]
thread_local! {
    /// Serial logs this thread opened: what shows that no environment of
    /// this crate hands one out.
    static OPENED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Test-only: how many serial logs this thread has opened.
#[cfg(test)]
pub(super) fn opened_here() -> usize {
    OPENED.with(std::cell::Cell::get)
}

impl SerialLog {
    pub(super) fn open<E: Env + ?Sized>(env: &E, path: &Path, mode: WriteMode) -> io::Result<Self> {
        #[cfg(test)]
        OPENED.with(|opened| opened.set(opened.get() + 1));
        let (file, end) = match mode {
            WriteMode::Truncate => (env.open_write(path, WriteMode::Truncate)?, 0),
            WriteMode::Append | WriteMode::Update => {
                // `Update`, not `Append`: a rollback shortens the file, which
                // an append handle cannot do on every host. `set_len` to the
                // length it already has moves the write position to the end.
                let mut file = env.open_write(path, WriteMode::Update)?;
                let end = file.len()?;
                file.set_len(end)?;
                (file, end)
            }
        };
        Ok(Self::over(file, end))
    }

    /// A log over `file`, which holds `end` bytes and writes next at `end`.
    fn over(file: Box<dyn WriteFile>, end: u64) -> Self {
        Self {
            // Built without spin (E48): it is held across the device calls of
            // an append and the caller's own sync, so a waiter queues at once.
            inner: Mutex::new_without_spin(Inner {
                file,
                end,
                held: BTreeMap::new(),
                failed: None,
            }),
        }
    }
}

impl SerialLog {
    /// The file and the held writes, to this caller alone. Waits on this
    /// thread for the caller that has them, which is inside one device call.
    fn inner(&self) -> io::Result<MutexGuard<'_, Inner>> {
        wait_here(self.inner.lock())
    }
}

impl Inner {
    /// Refuse an append or a sync while a failure is latched.
    fn usable(&self) -> io::Result<()> {
        match self.failed {
            None => Ok(()),
            Some(kind) => Err(io::Error::new(
                kind,
                "an earlier write to the log failed; it takes no more writes until it is truncated",
            )),
        }
    }

    /// Append `buf` at the end. A failure is latched: the file may hold a
    /// prefix of `buf` past `end` now.
    fn append(&mut self, buf: &[u8]) -> io::Result<()> {
        if let Err(err) = self.file.write_all(buf) {
            self.failed = Some(err.kind());
            return Err(err);
        }
        self.end += buf.len() as u64;
        Ok(())
    }

    /// Append every held write that now starts at or below the end.
    fn append_held(&mut self) -> io::Result<()> {
        while let Some(entry) = self.held.first_entry() {
            let at = *entry.key();
            if at > self.end {
                break;
            }
            let buf = entry.remove();
            let covered = (self.end - at).min(buf.len() as u64) as usize;
            // Not kept when it fails: the latch refuses every sync from
            // here, so nothing vouches for the commit that handed it over,
            // and the cut that lifts the latch drops every held write.
            self.append(&buf[covered..])?;
        }
        Ok(())
    }
}

impl LogFile for SerialLog {
    fn write_all_at(&self, offset: u64, buf: &[u8]) -> io::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let mut inner = self.inner()?;
        let Some(end) = offset.checked_add(buf.len() as u64) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the write would make the log too large to address",
            ));
        };
        if end <= inner.end {
            return Ok(());
        }
        inner.usable()?;
        if offset > inner.end {
            inner.held.insert(offset, buf.to_vec());
            return Ok(());
        }
        let covered = (inner.end - offset) as usize;
        inner.append(&buf[covered..])?;
        inner.append_held()
    }

    fn sync_data(&self) -> io::Result<()> {
        let mut inner = self.inner()?;
        // Every held write starts above the end unless an append failed,
        // and that failure is latched: a sync must not vouch for bytes the
        // file never took, or took at the wrong offset.
        inner.usable()?;
        inner.file.sync_data()
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        let mut inner = self.inner()?;
        if let Err(err) = inner.file.set_len(len) {
            // The length and the write position are unknown now too.
            inner.failed = Some(err.kind());
            return Err(err);
        }
        inner.end = len;
        // Everything held starts above the old end, so above the new one
        // too: the writes of commits the truncation discards.
        inner.held.clear();
        inner.failed = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::MemEnv;
    use proptest::prelude::*;

    fn open(env: &MemEnv, mode: WriteMode) -> SerialLog {
        SerialLog::open(env, Path::new("/db/log"), mode).unwrap()
    }

    fn env() -> MemEnv {
        let env = MemEnv::new();
        env.create_dir_all(Path::new("/db")).unwrap();
        env
    }

    #[test]
    fn a_write_ahead_of_a_gap_reaches_the_file_once_the_gap_is_filled() {
        let env = env();
        let log = open(&env, WriteMode::Truncate);
        log.write_all_at(4, b"5678").unwrap();
        assert_eq!(
            env.read(Path::new("/db/log")).unwrap(),
            b"",
            "a held write is not in the file"
        );
        log.write_all_at(0, b"1234").unwrap();
        assert_eq!(env.read(Path::new("/db/log")).unwrap(), b"12345678");
    }

    #[test]
    fn a_repeated_write_changes_nothing() {
        let env = env();
        let log = open(&env, WriteMode::Truncate);
        log.write_all_at(0, b"abcd").unwrap();
        log.write_all_at(0, b"abcd").unwrap();
        log.write_all_at(4, b"ef").unwrap();
        log.write_all_at(4, b"ef").unwrap();
        assert_eq!(env.read(Path::new("/db/log")).unwrap(), b"abcdef");
    }

    #[test]
    fn truncation_drops_the_tail_and_what_was_held_above_it() {
        let env = env();
        let log = open(&env, WriteMode::Truncate);
        log.write_all_at(0, b"keep").unwrap();
        log.write_all_at(4, b"drop").unwrap();
        log.write_all_at(12, b"held").unwrap();
        log.set_len(4).unwrap();
        // The offsets are written again, with other bytes: a new commit.
        log.write_all_at(4, b"next").unwrap();
        log.write_all_at(8, b"more").unwrap();
        assert_eq!(env.read(Path::new("/db/log")).unwrap(), b"keepnextmore");
    }

    #[test]
    fn reopening_for_update_continues_above_what_the_file_holds() {
        let env = env();
        open(&env, WriteMode::Truncate)
            .write_all_at(0, b"stamp")
            .unwrap();
        let log = open(&env, WriteMode::Update);
        log.write_all_at(5, b"record").unwrap();
        assert_eq!(env.read(Path::new("/db/log")).unwrap(), b"stamprecord");
    }

    /// Several threads each writing every record of a log at once, half of
    /// them front to back and half back to front: the file ends up holding
    /// every record once, at its offset, as if one thread had written them
    /// in order. A writer that found the end of the file, let another writer
    /// in and then appended would write a record a second time.
    #[test]
    fn records_written_by_several_threads_at_once_land_in_offset_order() {
        const THREADS: usize = 4;
        let rounds = if cfg!(miri) { 2 } else { 40 };
        for round in 0..rounds {
            let mut records = Vec::new();
            let mut at = 0u64;
            for i in 0..96usize {
                let len = 1 + (i * 7 + round) % 23;
                records.push((at, vec![i as u8 + 1; len]));
                at += len as u64;
            }
            let env = env();
            let log = open(&env, WriteMode::Truncate);
            let start = std::sync::Barrier::new(THREADS);
            std::thread::scope(|scope| {
                for thread in 0..THREADS {
                    let (log, records, start) = (&log, &records, &start);
                    scope.spawn(move || {
                        start.wait();
                        // Every record, so two threads meet at the record
                        // the file ends at; back to front for every other
                        // thread, so writes arrive ahead of gaps too.
                        let mut order: Vec<usize> = (0..records.len()).collect();
                        if thread % 2 == 1 {
                            order.reverse();
                        }
                        for i in order {
                            let (offset, bytes) = &records[i];
                            log.write_all_at(*offset, bytes).unwrap();
                        }
                    });
                }
            });
            log.sync_data().unwrap();
            let want: Vec<u8> = records.iter().flat_map(|(_, b)| b.clone()).collect();
            assert_eq!(
                env.read(Path::new("/db/log")).unwrap(),
                want,
                "round {round}"
            );
        }
    }

    /// A file whose append number `fail_in` (from 0) puts the first
    /// `prefix` bytes of its buffer in the file and then fails, as a short
    /// write followed by an error does. Every other call passes through.
    struct ShortOnce {
        file: Box<dyn WriteFile>,
        fail_in: Option<usize>,
        prefix: usize,
    }

    impl WriteFile for ShortOnce {
        fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
            match self.fail_in {
                Some(0) => {
                    self.fail_in = None;
                    self.file.write_all(&buf[..self.prefix.min(buf.len())])?;
                    Err(io::Error::other("injected short append"))
                }
                Some(n) => {
                    self.fail_in = Some(n - 1);
                    self.file.write_all(buf)
                }
                None => self.file.write_all(buf),
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            self.file.flush()
        }

        fn sync_all(&mut self) -> io::Result<()> {
            self.file.sync_all()
        }

        fn set_len(&mut self, len: u64) -> io::Result<()> {
            self.file.set_len(len)
        }

        fn len(&self) -> io::Result<u64> {
            self.file.len()
        }
    }

    fn short_once(env: &MemEnv, fail_in: usize, prefix: usize) -> SerialLog {
        let file = env
            .open_write(Path::new("/db/log"), WriteMode::Truncate)
            .unwrap();
        SerialLog::over(
            Box::new(ShortOnce {
                file,
                fail_in: Some(fail_in),
                prefix,
            }),
            0,
        )
    }

    fn held(env: &MemEnv) -> String {
        String::from_utf8_lossy(&env.read(Path::new("/db/log")).unwrap()).into_owned()
    }

    /// An append that fails part way leaves bytes past the end the log
    /// counts. Until a cut puts the file back at a known length, a retry of
    /// the same write must not land after them and report success, and no
    /// sync may vouch for the file.
    #[test]
    fn a_retry_after_a_short_failed_append_is_refused_until_the_log_is_cut() {
        let env = env();
        let log = short_once(&env, 1, 3);
        log.write_all_at(0, b"head").unwrap();
        assert!(
            log.write_all_at(4, b"record").is_err(),
            "setup: the append fails"
        );
        assert_eq!(held(&env), "headrec", "setup: part of it reached the file");
        let retried = log.write_all_at(4, b"record");
        assert!(
            retried.is_err(),
            "a retry was acknowledged with the file holding {:?}",
            held(&env)
        );
        assert!(
            log.sync_data().is_err(),
            "a sync vouched for a file holding {:?}",
            held(&env)
        );
        // A rollback cuts back to where the failed write began, and the
        // log takes the write again.
        log.set_len(4).unwrap();
        log.write_all_at(4, b"record").unwrap();
        log.sync_data().unwrap();
        assert_eq!(held(&env), "headrecord");
    }

    /// The same for a held write whose append fails part way once the gap
    /// below it is filled: a later sync must not append it again after the
    /// bytes that reached the file and vouch for them.
    #[test]
    fn a_held_write_whose_append_fails_part_way_is_not_appended_again() {
        let env = env();
        let log = short_once(&env, 1, 2);
        log.write_all_at(4, b"5678").unwrap();
        assert!(
            log.write_all_at(0, b"1234").is_err(),
            "setup: the held append fails"
        );
        assert_eq!(held(&env), "123456", "setup: part of it reached the file");
        assert!(
            log.sync_data().is_err(),
            "a sync vouched for a file holding {:?}",
            held(&env)
        );
        assert!(
            log.write_all_at(4, b"5678").is_err(),
            "a retry was acknowledged with the file holding {:?}",
            held(&env)
        );
        log.set_len(4).unwrap();
        log.write_all_at(4, b"5678").unwrap();
        log.sync_data().unwrap();
        assert_eq!(held(&env), "12345678");
    }

    proptest! {
        /// Whatever order the records of a log are written in, and however
        /// often each is written again, the file ends up holding them in
        /// offset order.
        #[test]
        fn any_write_order_yields_the_records_in_offset_order(
            lens in proptest::collection::vec(1usize..40, 1..24),
            order in proptest::collection::vec(any::<prop::sample::Index>(), 0..64),
        ) {
            let mut records = Vec::new();
            let mut at = 0u64;
            for (i, len) in lens.iter().enumerate() {
                records.push((at, vec![i as u8 + 1; *len]));
                at += *len as u64;
            }
            let env = env();
            let log = open(&env, WriteMode::Truncate);
            for pick in &order {
                let (offset, bytes) = &records[pick.index(records.len())];
                log.write_all_at(*offset, bytes).unwrap();
            }
            for (offset, bytes) in records.iter().rev() {
                log.write_all_at(*offset, bytes).unwrap();
            }
            log.sync_data().unwrap();
            let want: Vec<u8> = records.iter().flat_map(|(_, b)| b.clone()).collect();
            prop_assert_eq!(env.read(Path::new("/db/log")).unwrap(), want);
        }
    }
}

#[cfg(test)]
mod in_tree {
    use std::sync::Arc;

    use super::*;
    use crate::env::open_file_limit::OpenFileLimit;
    use crate::env::walk_fault::WalkFault;
    use crate::env::{MemEnv, std_env};

    /// The serial log is for an environment outside this crate. Each one in
    /// it that a database runs on, on this target, hands out a log of its
    /// own, and a wrapper hands out the log of what it wraps.
    #[test]
    fn no_environment_of_this_crate_hands_out_the_serial_log() {
        let dir = tempfile::TempDir::new().unwrap();
        let on_disk = dir.path().join("log");
        let in_memory = Path::new("/db/log");
        let mem = MemEnv::new();
        mem.create_dir_all(Path::new("/db")).unwrap();
        let faulted = WalkFault::default();
        faulted.create_dir_all(Path::new("/db")).unwrap();

        let before = opened_here();
        std_env().open_log(&on_disk, WriteMode::Truncate).unwrap();
        mem.open_log(in_memory, WriteMode::Truncate).unwrap();
        OpenFileLimit::new(std_env(), 4)
            .open_log(&on_disk, WriteMode::Update)
            .unwrap();
        OpenFileLimit::new(Arc::new(mem.clone()), 4)
            .open_log(in_memory, WriteMode::Update)
            .unwrap();
        faulted.open_log(in_memory, WriteMode::Truncate).unwrap();
        assert_eq!(
            opened_here(),
            before,
            "an environment fell back to the lock"
        );

        // The count is live: the default does open one.
        SerialLog::open(&mem, in_memory, WriteMode::Update).unwrap();
        assert_eq!(opened_here(), before + 1);
    }
}
