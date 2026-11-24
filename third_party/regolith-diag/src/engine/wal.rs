//! The write-ahead log: the durable record of every write between one
//! memtable flush and the next.
//!
//! # Formats
//!
//! Every log opens with a self-checksummed stamp whose `format` field says
//! how the records after it are framed.
//!
//! - **Format 2** is what this build writes: one record per commit,
//!   each carrying `synced_through`, and a CLOSE record at the end of a
//!   cleanly closed log. Its framing, and why it is shaped that way, is in
//!   [`super::wal_frame`].
//! - **Format 1** is what 0.1.x wrote, and it is still read exactly as
//!   before ([`super::wal_v1`]): one record per write, `[len u32][type
//!   u8][payload][checksum u32]` after a 12-byte stamp. Nothing writes it
//!   any more, so a database upgraded to format 2 cannot be opened by
//!   0.1.x again.
//!
//! ```text
//! format 1 stamp  ["REGO"][format u16 = 1][reserved u16 = 0][stamp checksum u32]
//! format 2 stamp  ["REGO"][format u16 = 2][reserved u16 = 0][nonce u64][stamp checksum u32]
//! ```
//!
//! # A log with no stamp
//!
//! The stamp is written when the log is created, before any record, and a
//! sync of any record makes it durable too. A file whose first four bytes
//! are not `REGO` is one a crash caught before anything in it was synced:
//! no record in it was ever made durable. The newest log in that state is
//! discarded whole, and the discard is reported like any dropped tail; an
//! earlier one is the leftover of such a crash that a later recovery
//! replaced, and yields nothing. The exception is a stamp whose checks
//! still pass with the magic put back, or whose nonce still verifies the
//! first record: that stamp was written whole and rotted afterwards, and
//! the open is refused ([`stamp_was_written`]).
//!
//! A stamp can never be read as a record header. `REGO` as a
//! little-endian `len` is 0x4F47_4552, about 1.3 GiB, and
//! [`MAX_RECORD_LEN`] caps a record far below that.
//!
//! # How format 2 replay tells a crash from corruption
//!
//! Replay of the newest log reads P, the largest `synced_through` of any
//! usable record, or the end of the file when a usable CLOSE is present,
//! and stops at O, the first record that is not usable: torn, failing a
//! check, or past the end of the file. When O < P a surviving record
//! proves the damaged bytes were synced, so they held acknowledged writes,
//! and the open is refused naming the file, O and P. Otherwise nothing
//! proves the bytes from O to the end of the file were synced: they are
//! what a crash leaves of an unsynced tail, in any state, or rot in the
//! last synced group that nothing after it vouches for, which the format
//! cannot tell apart from such a tail. They are
//! dropped, truncated away durably before the next log is created, and
//! reported: a warn line, the `WalTailDiscarded` and
//! `WalTailDiscardedBytes` tickers and
//! [`crate::EventListener::on_wal_tail_discarded`]. Earlier logs were
//! synced whole when they were sealed, so any damage in them refuses.
//! `proofs/tla/WalRecovery.tla` and `proofs/lean/Regolith/WalRecovery.lean`
//! state and prove the rule.
//!
//! # How format 1 replay tells a crash from corruption
//!
//! Format 1 carries no `synced_through`, so its newest log is judged by
//! its shape. A record the file ends inside is a torn tail and ends the
//! log. A whole record that fails its checksum, carries an unknown type or
//! does not parse refuses the open, unless nothing but zero bytes follows
//! it, which is how an unwritten or power-zeroed tail reads back. An
//! incomplete record followed by whole records that tile the rest of the
//! file is a mangled length rather than a torn write, and refuses. That
//! rule refuses some states a crash does produce (a whole final record
//! with garbage after it), which is why format 2 replaced it.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::checksum;
use super::seal::{Keyring, Sealer};
use super::wal_frame;
use super::wal_seal;
use crate::WriteBatchOp;
use crate::env::{Env, WriteFile, WriteMode};

/// Operation types: a format 1 record's type, and the type byte of each
/// operation inside a format 2 group.
pub(super) const RECORD_PUT: u8 = 0x01;
pub(super) const RECORD_DELETE: u8 = 0x02;
pub(super) const RECORD_DELETE_RANGE: u8 = 0x03;
pub(super) const RECORD_MERGE: u8 = 0x04;
/// A format 1 record holding a whole write batch. Format 2 has none: a
/// group is replayed whole or not at all.
pub(super) const RECORD_BATCH: u8 = 0x05;

/// `REGO`, the four bytes every WAL begins with.
pub(crate) const WAL_MAGIC: [u8; 4] = *b"REGO";

/// Format 1 stamp layout: magic, format, reserved, checksum.
pub(crate) const WAL_STAMP_LEN: usize = 12;

/// Largest group payload the writer will emit, and so the largest payload
/// a reader will believe. A write whose operations would be larger is
/// refused before it is admitted (see [`check_record_len`]), and a commit
/// group never stages more than this in one append. Well under `REGO` read
/// as a little-endian length (0x4F47_4552), so the stamp can never be
/// parsed as a record header.
pub(crate) const MAX_RECORD_LEN: u32 = 1 << 30;

/// A write-ahead log being written, in format 2.
///
/// Every append is one record carrying `synced_through`, the offset the
/// last completed [`Wal::sync_data`] covered, so replay can tell bytes a
/// sync made durable from bytes a crash was free to lose
/// (`proofs/tla/WalRecovery.tla`, `AppendRecord` and `Sync`).
pub(crate) struct Wal {
    /// Through the host environment, so a log is written the same way on
    /// a filesystem, under wasi, and against OPFS in a browser.
    ///
    /// Deliberately unbuffered. A record is handed to the host in one
    /// write, so a buffer in front of it saves no syscall, and it would
    /// change what a crash costs: a process
    /// killed with bytes still in a userspace buffer loses them, where
    /// bytes handed to the host survive in its page cache. That
    /// distinction is the whole basis of `DurabilityMode::Eventual`.
    file: Box<dyn WriteFile>,
    /// Bytes appended so far, tracked in memory rather than queried so
    /// [`Wal::rollback_to`] can discard a failed group without a metadata
    /// syscall on the write path.
    offset: u64,
    path: PathBuf,
    parent_synced: bool,
    env: Arc<dyn Env>,
    /// Drawn when the log is created and written into its stamp. Every
    /// record's header check covers it, so a record of another log never
    /// verifies as one of this log's.
    nonce: u64,
    /// The offset the last completed sync covered: every byte before it is
    /// durable. Stamped into every record appended after it.
    synced_through: u64,
    /// Set once CLOSE is appended. Nothing may follow CLOSE, since replay
    /// takes a usable CLOSE to prove the whole file durable.
    closed: bool,
    /// A small record's frame and payload, joined so they leave in one
    /// plain write; reused, and never larger than [`COALESCE_LEN`] plus
    /// a frame. In a sealed log every record is joined, to be encrypted in
    /// place, and the buffer is kept only up to [`SEALED_KEEP_LEN`].
    joined: Vec<u8>,
    /// Set when the log is sealed (`super::wal_seal`): every record's
    /// payload is sealed under this key, the one its stamp names.
    seal: Option<Sealer>,
    /// Bytes of the stamp, which a rollback never truncates into.
    stamp_len: u64,
}

/// Largest payload copied next to its frame for one plain write. Past it a
/// vectored write costs less than the copy.
const COALESCE_LEN: usize = 4096;

/// Largest buffer a sealed log keeps between records. A larger record's
/// buffer is released once written, so one huge record does not pin its
/// size.
const SEALED_KEEP_LEN: usize = 1 << 20;

/// A replayed WAL entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WalEntry {
    Put {
        key: Vec<u8>,
        value: Vec<u8>,
        seq: u64,
    },
    Delete {
        key: Vec<u8>,
        seq: u64,
    },
    DeleteRange {
        start: Vec<u8>,
        end: Vec<u8>,
        seq: u64,
    },
    Merge {
        key: Vec<u8>,
        operand: Vec<u8>,
        seq: u64,
    },
}

impl Wal {
    /// Create a new WAL file at the given path, sealed under the current
    /// key of `keyring` when one is given.
    ///
    /// A sealed log's stamp is durable before this returns, and the log
    /// appears under its name only then (`super::wal_seal`).
    pub(crate) fn create_in(
        env: &Arc<dyn Env>,
        path: &Path,
        keyring: Option<&Keyring>,
    ) -> io::Result<Self> {
        let nonce = fresh_nonce(env.as_ref(), path);
        let named = |e: io::Error| {
            if crate::Error::is_typed(&e) {
                return e;
            }
            io::Error::new(e.kind(), format!("creating wal {}: {e}", path.display()))
        };
        let (file, seal, stamp_len, synced) = match keyring {
            Some(keyring) => {
                let sealer = keyring.current().map_err(named)?;
                let stamp = wal_seal::encode_stamp(nonce, &sealer)?;
                let file = wal_seal::create_durably(env, path, &stamp).map_err(named)?;
                (file, Some(sealer), stamp.len() as u64, true)
            }
            None => {
                // Named on failure: creating a log at a path a previous log
                // was unlinked from is the interesting case, because on
                // Windows an unlinked file whose handle is still open keeps
                // its name until that handle closes, and creating over it
                // is refused.
                let mut file = env.open_write(path, WriteMode::Truncate).map_err(named)?;
                file.write_all(&wal_frame::encode_stamp(nonce))?;
                (file, None, wal_frame::STAMP_LEN as u64, false)
            }
        };
        Ok(Self {
            file,
            offset: stamp_len,
            path: path.to_path_buf(),
            parent_synced: synced,
            env: Arc::clone(env),
            nonce,
            synced_through: if synced { stamp_len } else { 0 },
            closed: false,
            joined: Vec::new(),
            seal,
            stamp_len,
        })
    }

    /// Create a WAL through the standard environment.
    #[cfg(test)]
    pub(crate) fn create(path: &Path) -> io::Result<Self> {
        Self::create_in(&crate::env::std_env(), path, None)
    }

    /// Append one commit, whose operations `entries` holds, as one record,
    /// and advance the tracked offset.
    ///
    /// `entries` must already be encoded by [`encode_op_record`],
    /// [`encode_ops_record`] or [`encode_put_record`]; this adds the
    /// record's frame, in the same host write: a small record is copied next
    /// to its frame, a large one leaves in one vectored write. On failure
    /// the tracked offset is left at the pre-call value so
    /// [`Wal::rollback_to`] can discard whatever prefix reached the file.
    /// An empty group appends nothing. After [`Wal::close`] every append
    /// is refused.
    ///
    /// The commit path never stages more than [`MAX_RECORD_LEN`] in one
    /// call.
    pub(crate) fn append_group(&mut self, entries: &[u8]) -> io::Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        if self.closed {
            return Err(io::Error::other("the write-ahead log is already closed"));
        }
        debug_assert!(
            entries.len() as u64 <= MAX_RECORD_LEN as u64,
            "a commit over MAX_RECORD_LEN must be refused before it is staged"
        );
        self.write_record(wal_frame::KIND_GROUP, entries)
    }

    fn write_record(&mut self, kind: u8, payload: &[u8]) -> io::Result<()> {
        if let Some(sealer) = &self.seal {
            wal_seal::frame_record(
                sealer,
                kind,
                payload,
                self.synced_through,
                self.nonce,
                self.offset,
                &mut self.joined,
            )?;
            let written = self.file.write_all(&self.joined);
            let len = self.joined.len();
            if self.joined.capacity() > SEALED_KEEP_LEN {
                self.joined = Vec::new();
            }
            written?;
            self.offset += len as u64;
            return Ok(());
        }
        let header =
            wal_frame::encode_header(kind, payload, self.synced_through, self.nonce, self.offset);
        if payload.len() <= COALESCE_LEN {
            self.joined.clear();
            self.joined.extend_from_slice(&header);
            self.joined.extend_from_slice(payload);
            self.file.write_all(&self.joined)?;
        } else {
            self.file.write_all_vectored(&[&header, payload])?;
        }
        self.offset += (wal_frame::HEADER_LEN + payload.len()) as u64;
        Ok(())
    }

    /// Close the log cleanly: sync every record, append CLOSE, and sync
    /// again, so replay finds CLOSE and knows the whole file is durable.
    ///
    /// The first sync is what makes CLOSE honest
    /// (`proofs/tla/WalRecovery.tla`, RED `CloseWithoutSync`): appended
    /// before it, a crash could keep CLOSE and lose a record before it,
    /// and replay would refuse a crash it should survive. Once CLOSE may
    /// be in the file nothing else is appended, even when the final sync
    /// fails. Closing a closed log does nothing.
    #[cfg(test)]
    pub(crate) fn close(&mut self) -> io::Result<()> {
        if self.closed {
            return Ok(());
        }
        self.sync_data()?;
        self.closed = true;
        self.write_record(wal_frame::KIND_CLOSE, &[])?;
        self.sync_data()
    }

    /// Byte offset one past the last successfully appended record.
    #[cfg(test)]
    pub(crate) fn offset(&self) -> u64 {
        self.offset
    }

    /// Hand this log to the commit pipeline: the same file, written by
    /// position from here on (`super::wal_log`). Returns it with the offset
    /// its records end at. The appending handle is closed first, so the
    /// file is opened once at a time.
    pub(crate) fn into_log(self) -> io::Result<(super::wal_log::Log, u64)> {
        let Wal {
            file,
            offset,
            path,
            parent_synced,
            env,
            nonce,
            synced_through,
            seal,
            stamp_len,
            ..
        } = self;
        drop(file);
        let log = super::wal_log::Log::adopt(
            &env,
            &path,
            nonce,
            seal,
            stamp_len,
            synced_through,
            parent_synced,
        )?;
        Ok((log, offset))
    }

    /// The key this log is sealed under, or `None` when it is not sealed.
    #[cfg(test)]
    pub(crate) fn seal_key(&self) -> Option<crate::KeyId> {
        self.seal.as_ref().map(Sealer::id)
    }

    /// Truncate back to `offset` and reposition the write cursor there.
    ///
    /// Called when an append or a sync failed, so a partially written
    /// record never survives as a torn one that replay would have to
    /// reason about.
    #[cfg(test)]
    pub(crate) fn rollback_to(&mut self, offset: u64) -> io::Result<()> {
        // The stamp is not a record and is never rolled back over: doing
        // so would leave an unidentifiable file behind.
        debug_assert!(
            offset >= self.stamp_len,
            "rollback must not truncate into the WAL stamp"
        );
        // The handle appends, so truncating is enough to move the write
        // position back: there is no cursor to seek.
        self.file.set_len(offset)?;
        self.offset = offset;
        Ok(())
    }

    /// Append a put as a group of one.
    #[cfg(test)]
    pub(crate) fn append_put(&mut self, key: &[u8], value: &[u8], seq: u64) -> io::Result<()> {
        let mut group = Vec::with_capacity(put_record_len(key, value));
        encode_put_record(&mut group, key, value, seq);
        self.append_group(&group)
    }

    /// Append a delete as a group of one.
    #[cfg(test)]
    pub(crate) fn append_delete(&mut self, key: &[u8], seq: u64) -> io::Result<()> {
        self.append_op(&WriteBatchOp::Delete { key: key.to_vec() }, seq)
    }

    /// Append a merge operand as a group of one.
    #[cfg(test)]
    pub(crate) fn append_merge(&mut self, key: &[u8], operand: &[u8], seq: u64) -> io::Result<()> {
        self.append_op(
            &WriteBatchOp::Merge {
                key: key.to_vec(),
                operand: operand.to_vec(),
            },
            seq,
        )
    }

    /// Append a range delete covering `[start, end)` as a group of one.
    #[cfg(test)]
    pub(crate) fn append_delete_range(
        &mut self,
        start: &[u8],
        end: &[u8],
        seq: u64,
    ) -> io::Result<()> {
        self.append_op(
            &WriteBatchOp::DeleteRange {
                start: start.to_vec(),
                end: end.to_vec(),
            },
            seq,
        )
    }

    #[cfg(test)]
    fn append_op(&mut self, op: &WriteBatchOp, seq: u64) -> io::Result<()> {
        let mut group = Vec::new();
        encode_op_record(&mut group, op, seq);
        self.append_group(&group)
    }

    /// Flush the appended bytes to stable storage.
    ///
    /// `sync_data` (`fdatasync`), not `sync_all` (`fsync`): the WAL is
    /// append-only, and `fdatasync` already flushes the metadata a later
    /// read needs, which includes the file size. Inode timestamps are not
    /// needed to replay the log, so flushing them is work no reader will
    /// ever benefit from. How much latency that saves is filesystem
    /// dependent and can be nil.
    ///
    /// The directory entry naming the file is a separate durability
    /// concern that no `fdatasync` on the file itself can cover, so the
    /// parent directory is fsynced once per WAL file on first sync.
    ///
    /// Only a sync that completes advances `synced_through`, and only to
    /// the offset it began at, so no record ever claims bytes a sync did
    /// not cover.
    pub(crate) fn sync_data(&mut self) -> io::Result<()> {
        let env = Arc::clone(&self.env);
        self.sync_with_parent_sync(move |p| crate::env::sync_parent_dir(&*env, p))
    }

    fn sync_with_parent_sync(
        &mut self,
        mut sync_parent: impl FnMut(&Path) -> io::Result<()>,
    ) -> io::Result<()> {
        #[cfg(test)]
        if fault::should_fail_sync(&self.path) {
            return Err(io::Error::other("injected WAL sync failure"));
        }
        let through = self.offset;
        self.file.sync_data()?;
        if !self.parent_synced {
            sync_parent(&self.path)?;
            self.parent_synced = true;
        }
        self.synced_through = through;
        Ok(())
    }

    /// Get the path to this WAL file.
    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Replay a WAL file and collect every entry.
    ///
    /// Test-only reference implementation: recovery streams the log
    /// through [`WalReplayIter`] instead, so it never holds more than
    /// one record. This wrapper drains that same iterator, which is
    /// what makes the WAL tests below a check on the streaming reader
    /// rather than on a second, divergent parser.
    ///
    /// [`WalReplayIter`]: super::wal_replay::WalReplayIter
    #[cfg(test)]
    pub(crate) fn replay(path: &Path) -> io::Result<Vec<WalEntry>> {
        let mut iter = super::wal_replay::WalReplayIter::open(
            &crate::env::std_env(),
            path,
            super::wal_replay::WalPosition::Newest,
            None,
        )?;
        let mut entries = Vec::new();
        while let Some(entry) = iter.next_entry()? {
            entries.push(entry);
        }
        Ok(entries)
    }

    /// Delete a WAL file from `env`.
    pub(crate) fn remove_in(env: &dyn Env, path: &Path) -> io::Result<()> {
        crate::env::remove_file_and_sync_parent(env, path)
    }

    /// Delete a WAL file through the standard environment.
    #[cfg(test)]
    pub(crate) fn remove(path: &Path) -> io::Result<()> {
        Self::remove_in(&*crate::env::std_env(), path)
    }
}

/// A nonce for a new log: distinct from every other log's, which is all
/// the header check needs from it, not secret.
///
/// `RandomState` draws its keys from the operating system once per thread
/// and moves them on for every new state, so two logs created one after
/// the other differ. The clock and the path are mixed in for a platform
/// whose `RandomState` has no randomness to draw on, where they still
/// separate one database's logs and one session from the next.
pub(super) fn fresh_nonce(env: &dyn Env, path: &Path) -> u64 {
    use std::hash::BuildHasher;
    std::collections::hash_map::RandomState::new().hash_one((env.now_micros(), path))
}

/// Make the first `len` bytes of the log at `path` its whole content,
/// durably. Recovery calls it on the newest log after dropping a tail,
/// before any newer log exists, so the log is complete when it becomes an
/// earlier one (`proofs/tla/WalRecovery.tla`, RED `NoTruncate`).
pub(crate) fn truncate_durably(env: &dyn Env, path: &Path, len: u64) -> io::Result<()> {
    let mut file = env.open_write(path, WriteMode::Update)?;
    file.set_len(len)?;
    file.sync_data()
}

/// Test-only fault injection for the commit path.
///
/// Scoped to a directory rather than armed globally so two tests running
/// in parallel in one process cannot trip each other's injection.
#[cfg(test)]
pub(crate) mod fault {
    use std::path::{Path, PathBuf};

    use crate::sync::internal::Mutex;

    /// Every directory currently armed. A list rather than a single
    /// slot because tests run in parallel in one process: with one slot,
    /// arming for a second directory silently disarms the first and
    /// disarming from either clears both, which makes both tests flaky.
    static ARMED: Mutex<Vec<Arm>> = Mutex::new(Vec::new());

    /// One armed directory and how its syncs fail.
    struct Arm {
        dir: PathBuf,
        /// `0` fails every sync. `n > 0` fails every sync except each
        /// `n`th one, so `2` alternates fail, succeed, fail, succeed.
        period: u64,
        /// Syncs matched under `dir` so far, which is what `period`
        /// counts against.
        seen: u64,
    }

    /// Make every `sync_data` on a WAL under `dir` fail until disarmed.
    ///
    /// Both the path as given and its resolved form are armed. On macOS
    /// the temporary directory sits under `/var/folders`, which is a
    /// symlink to `/private/var/folders`, so a prefix test against only
    /// one of the two never matches and the fault silently never fires:
    /// the test then sees every write acknowledged and fails on its own
    /// "the fault must produce a mix" assertion rather than on anything
    /// the engine did.
    pub(crate) fn arm_sync_failure(dir: &Path) {
        arm(dir, 0);
    }

    /// Make every sync under `dir` fail except each `period`th one.
    ///
    /// A test that needs both outcomes cannot get them from a fault that
    /// only ever fails: it has to rely on something else to supply the
    /// successes, and the only thing available is which commits happen
    /// to be covered by the same sync. That is decided by thread timing,
    /// so on a slower machine every sync can cover a writer that asked
    /// for one, every one fails, and the test that wanted a mix gets
    /// none. Alternating here makes the mix a property
    /// of the fault rather than of the scheduler.
    pub(crate) fn arm_flapping_sync_failure(dir: &Path, period: u64) {
        assert!(period > 1, "a flapping fault needs a period above one");
        arm(dir, period);
    }

    fn arm(dir: &Path, period: u64) {
        let mut armed = ARMED.lock();
        armed.push(Arm {
            dir: dir.to_path_buf(),
            period,
            seen: 0,
        });
        if let Some(real) = resolved(dir)
            && real != dir
        {
            armed.push(Arm {
                dir: real,
                period,
                seen: 0,
            });
        }
    }

    /// `dir` with symlinks resolved, and without Windows' verbatim
    /// prefix.
    ///
    /// Two platforms need this and neither is optional. On macOS the
    /// temporary directory sits under `/var/folders`, a symlink to
    /// `/private/var/folders`. On Windows `canonicalize` hands back a
    /// `\\?\C:\...` verbatim path, which no path built by joining
    /// ever matches. Either way a prefix test against one form alone
    /// silently never matches, the fault never fires, and the test fails
    /// on its own "the fault must produce a mix" assertion rather than
    /// on anything the engine did.
    fn resolved(dir: &Path) -> Option<PathBuf> {
        let real = dir.canonicalize().ok()?;
        let text = real.to_str()?;
        Some(match text.strip_prefix(r"\\?\") {
            Some(plain) => PathBuf::from(plain),
            None => real,
        })
    }

    /// Stop failing syncs under `dir`, leaving any other test's arming
    /// in place.
    pub(crate) fn disarm_sync_failure(dir: &Path) {
        let mut armed = ARMED.lock();
        let real = resolved(dir);
        armed.retain(|a| a.dir != dir && Some(&a.dir) != real.as_ref());
    }

    /// Every directory under which cutting a log back fails.
    static CUTS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

    /// Make every cut of a positional log under `dir` back to an earlier
    /// length fail until disarmed: what a rollback meets when the device
    /// refuses the truncation, after which the log's tail is unknown. The
    /// path is armed as given and resolved, as for a sync.
    pub(crate) fn arm_cut_failure(dir: &Path) {
        let mut cuts = CUTS.lock();
        cuts.push(dir.to_path_buf());
        if let Some(real) = resolved(dir)
            && real != dir
        {
            cuts.push(real);
        }
    }

    /// Stop failing cuts under `dir`.
    pub(crate) fn disarm_cut_failure(dir: &Path) {
        let real = resolved(dir);
        CUTS.lock()
            .retain(|armed| armed != dir && Some(armed) != real.as_ref());
    }

    pub(crate) fn should_fail_cut(path: &Path) -> bool {
        let real = path.parent().and_then(resolved);
        CUTS.lock().iter().any(|dir| {
            path.starts_with(dir) || real.as_ref().is_some_and(|real| real.starts_with(dir))
        })
    }

    pub(crate) fn should_fail_sync(path: &Path) -> bool {
        let mut armed = ARMED.lock();
        // The log itself may not exist yet, so the resolved form is
        // taken from its directory.
        let real = path.parent().and_then(resolved);
        for arm in armed.iter_mut() {
            let matched = path.starts_with(&arm.dir)
                || real.as_ref().is_some_and(|r| r.starts_with(&arm.dir));
            if !matched {
                continue;
            }
            if arm.period == 0 {
                return true;
            }
            arm.seen += 1;
            return arm.seen % arm.period != 0;
        }
        false
    }
}

/// Format a WAL filename from a numeric ID.
pub(crate) fn wal_filename(id: u64) -> String {
    format!("wal_{:06}.log", id)
}

/// Bytes one operation occupies inside a group: its type byte and its
/// payload.
///
/// Saturating: on a 32-bit target a length near `usize::MAX` would
/// otherwise wrap past the limit check instead of failing it.
fn entry_len(payload_len: usize) -> usize {
    payload_len.saturating_add(1)
}

/// Bytes [`encode_put_record`] adds to a group.
pub(crate) fn put_record_len(key: &[u8], value: &[u8]) -> usize {
    entry_len(put_payload_len(key, value))
}

/// Encode a put as one operation of a group.
pub(crate) fn encode_put_record(out: &mut Vec<u8>, key: &[u8], value: &[u8], seq: u64) {
    out.push(RECORD_PUT);
    encode_put_payload(out, key, value, seq);
}

/// Bytes [`encode_delete_record`] adds to a group.
pub(crate) fn delete_record_len(key: &[u8]) -> usize {
    entry_len(delete_payload_len(key))
}

/// Encode a delete as one operation of a group.
pub(crate) fn encode_delete_record(out: &mut Vec<u8>, key: &[u8], seq: u64) {
    out.push(RECORD_DELETE);
    encode_delete_payload(out, key, seq);
}

/// Bytes [`encode_merge_record`] adds to a group.
pub(crate) fn merge_record_len(key: &[u8], operand: &[u8]) -> usize {
    entry_len(merge_payload_len(key, operand))
}

/// Encode a merge operand as one operation of a group.
pub(crate) fn encode_merge_record(out: &mut Vec<u8>, key: &[u8], operand: &[u8], seq: u64) {
    out.push(RECORD_MERGE);
    encode_merge_payload(out, key, operand, seq);
}

/// Bytes [`encode_delete_range_record`] adds to a group.
pub(crate) fn delete_range_record_len(start: &[u8], end: &[u8]) -> usize {
    entry_len(delete_range_payload_len(start, end))
}

/// Encode a range delete as one operation of a group.
pub(crate) fn encode_delete_range_record(out: &mut Vec<u8>, start: &[u8], end: &[u8], seq: u64) {
    out.push(RECORD_DELETE_RANGE);
    encode_delete_range_payload(out, start, end, seq);
}

/// Encode one write-batch operation as one operation of a group, at `seq`.
pub(crate) fn encode_op_record(out: &mut Vec<u8>, op: &WriteBatchOp, seq: u64) {
    match op {
        WriteBatchOp::Put { key, value } => encode_put_record(out, key, value, seq),
        WriteBatchOp::Delete { key } => encode_delete_record(out, key, seq),
        WriteBatchOp::DeleteRange { start, end } => {
            encode_delete_range_record(out, start, end, seq)
        }
        WriteBatchOp::Merge { key, operand } => encode_merge_record(out, key, operand, seq),
    }
}

/// Bytes [`encode_ops_record`] adds to a group for `ops`.
pub(crate) fn ops_record_len(ops: &[WriteBatchOp]) -> usize {
    let mut len = RecordLen::default();
    ops.iter().for_each(|op| len.op(op));
    len.framed()
}

/// Bytes the operations of one write add to a group, accumulated one
/// operation at a time.
///
/// The single implementation of [`encode_ops_record`]'s sizing rule.
/// Validation folds it into the pass it already makes over a write, so the
/// length costs no extra walk.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RecordLen {
    /// Sum of the operations' lengths so far.
    entries: usize,
}

impl RecordLen {
    pub(crate) fn put(&mut self, key: &[u8], value: &[u8]) {
        self.push(put_payload_len(key, value))
    }
    /// A put whose key and value have the given lengths, for a bound taken
    /// before the bytes exist.
    pub(crate) fn put_sized(&mut self, key_len: usize, value_len: usize) {
        self.push(put_payload_size(key_len, value_len))
    }
    pub(crate) fn delete(&mut self, key: &[u8]) {
        self.push(delete_payload_len(key))
    }
    pub(crate) fn delete_range(&mut self, start: &[u8], end: &[u8]) {
        self.push(delete_range_payload_len(start, end))
    }
    pub(crate) fn merge(&mut self, key: &[u8], operand: &[u8]) {
        self.push(merge_payload_len(key, operand))
    }
    pub(crate) fn op(&mut self, op: &WriteBatchOp) {
        self.push(batch_op_payload_len(op))
    }

    fn push(&mut self, payload: usize) {
        // Saturating: on a 32-bit target a sum of lengths can wrap, and a
        // wrapped length would pass the limit check.
        self.entries = self.entries.saturating_add(entry_len(payload));
    }

    /// Bytes [`encode_ops_record`] emits for the operations seen so far.
    pub(crate) fn framed(&self) -> usize {
        self.entries
    }
}

/// Refuse a write whose operations would log more than `limit` bytes.
///
/// Production passes [`MAX_RECORD_LEN`] through [`check_write_len`]; the
/// limit is a parameter only so the boundary can be tested without a
/// gigabyte of data.
pub(crate) fn check_record_len(framed_len: usize, limit: usize) -> io::Result<()> {
    if framed_len <= limit {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "write is too large: it would log {framed_len} bytes and one write can \
             log at most {limit}; split it into smaller writes"
        ),
    ))
}

/// [`check_record_len`] at the format's limit, the bound every write path
/// enforces.
pub(crate) fn check_write_len(framed_len: usize) -> io::Result<()> {
    check_record_len(framed_len, MAX_RECORD_LEN as usize)
}

/// Encode `ops` into `out` as operations of a group, at consecutive
/// sequence numbers from `base_seq`. The group is replayed whole or not at
/// all, so a multi-op write needs no record of its own to stay atomic.
pub(crate) fn encode_ops_record(out: &mut Vec<u8>, ops: &[WriteBatchOp], base_seq: u64) {
    for (i, op) in ops.iter().enumerate() {
        encode_op_record(out, op, base_seq + i as u64);
    }
}

pub(super) fn encode_put_payload(out: &mut Vec<u8>, key: &[u8], value: &[u8], seq: u64) {
    out.extend_from_slice(&(key.len() as u32).to_le_bytes());
    out.extend_from_slice(key);
    out.extend_from_slice(&(value.len() as u32).to_le_bytes());
    out.extend_from_slice(value);
    out.extend_from_slice(&seq.to_le_bytes());
}

pub(super) fn encode_delete_payload(out: &mut Vec<u8>, key: &[u8], seq: u64) {
    out.extend_from_slice(&(key.len() as u32).to_le_bytes());
    out.extend_from_slice(key);
    out.extend_from_slice(&seq.to_le_bytes());
}

fn encode_delete_range_payload(out: &mut Vec<u8>, start: &[u8], end: &[u8], seq: u64) {
    out.extend_from_slice(&(start.len() as u32).to_le_bytes());
    out.extend_from_slice(start);
    out.extend_from_slice(&(end.len() as u32).to_le_bytes());
    out.extend_from_slice(end);
    out.extend_from_slice(&seq.to_le_bytes());
}

fn encode_merge_payload(out: &mut Vec<u8>, key: &[u8], operand: &[u8], seq: u64) {
    out.extend_from_slice(&(key.len() as u32).to_le_bytes());
    out.extend_from_slice(key);
    out.extend_from_slice(&(operand.len() as u32).to_le_bytes());
    out.extend_from_slice(operand);
    out.extend_from_slice(&seq.to_le_bytes());
}

fn put_payload_len(key: &[u8], value: &[u8]) -> usize {
    put_payload_size(key.len(), value.len())
}

fn put_payload_size(key_len: usize, value_len: usize) -> usize {
    4 + key_len + 4 + value_len + 8
}

fn delete_payload_len(key: &[u8]) -> usize {
    4 + key.len() + 8
}

fn delete_range_payload_len(start: &[u8], end: &[u8]) -> usize {
    4 + start.len() + 4 + end.len() + 8
}

fn merge_payload_len(key: &[u8], operand: &[u8]) -> usize {
    4 + key.len() + 4 + operand.len() + 8
}

fn batch_op_payload_len(op: &WriteBatchOp) -> usize {
    match op {
        WriteBatchOp::Put { key, value } => put_payload_len(key, value),
        WriteBatchOp::Delete { key } => delete_payload_len(key),
        WriteBatchOp::DeleteRange { start, end } => delete_range_payload_len(start, end),
        WriteBatchOp::Merge { key, operand } => merge_payload_len(key, operand),
    }
}

/// Where a replay of the newest log stopped short of the last byte, and
/// how much it threw away. Recovery truncates the log there and reports
/// it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TailVerdict {
    pub(crate) offset: u64,
    pub(crate) discarded_bytes: u64,
}

impl TailVerdict {
    pub(super) fn discarded(bytes: &[u8], pos: usize) -> Self {
        Self {
            offset: pos as u64,
            discarded_bytes: (bytes.len() - pos) as u64,
        }
    }
}

/// Validate the first [`WAL_STAMP_LEN`] bytes of a log, which every
/// format shares, and return the format they name; `None` for a log with
/// no stamp.
///
/// The stamp is mandatory: every log carries one. The exception is a log
/// a crash caught before the stamp reached the disk, which reads back as
/// nothing, as zeros or as garbage, and holds nothing a sync made durable,
/// since any sync would have made the stamp durable too. A format 2 stamp
/// is longer; its remaining bytes are checked by
/// [`wal_frame::stamp_nonce`].
pub(super) fn validate_wal_stamp(bytes: &[u8]) -> io::Result<Option<u16>> {
    if bytes.len() < WAL_STAMP_LEN || bytes[0..4] != WAL_MAGIC {
        return Ok(None);
    }
    let format = u16::from_le_bytes([bytes[4], bytes[5]]);
    let reserved = u16::from_le_bytes([bytes[6], bytes[7]]);
    let stored = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
    if stored != checksum::wal_stamp(&WAL_MAGIC, format, reserved) {
        return Err(invalid_wal("the write-ahead log header is damaged"));
    }
    // A newer format is refused rather than guessed at. This is the whole
    // point of the field: an older build must fail loudly on a log a
    // newer one wrote, instead of misreading its framing. 0.1.x says the
    // same of format 2, because format 2 keeps this checksum where 0.1.x
    // looks for it.
    if format == 0 || format > wal_frame::FORMAT_V2 {
        return Err(invalid_wal(format!(
            "the write-ahead log is in format {format}, which this version of regolith \
             cannot read; open it with the version that wrote it"
        )));
    }
    Ok(Some(format))
}

/// Whether `head`, the first bytes of a log whose magic is wrong, still
/// holds a stamp that was written whole: a stamp check passes with the
/// magic put back, or the first record verifies under the nonce the stamp
/// holds. A crash while the log was created leaves zeros or garbage, which
/// pass a 32-bit check about once in four billion; bit rot in a written
/// stamp leaves these, and then the log is damage, not an empty log.
pub(super) fn stamp_was_written(head: &[u8]) -> bool {
    // A stamp with its magic intact that is still unreadable was cut short
    // while the log was created.
    if head.len() < WAL_STAMP_LEN || head[0..4] == WAL_MAGIC {
        return false;
    }
    let mut fixed = [0u8; wal_frame::STAMP_LEN];
    let n = head.len().min(wal_frame::STAMP_LEN);
    fixed[..n].copy_from_slice(&head[..n]);
    fixed[0..4].copy_from_slice(&WAL_MAGIC);
    let format = u16::from_le_bytes([fixed[4], fixed[5]]);
    let reserved = u16::from_le_bytes([fixed[6], fixed[7]]);
    let head_check = u32::from_le_bytes([fixed[8], fixed[9], fixed[10], fixed[11]]);
    if head_check == checksum::wal_stamp(&WAL_MAGIC, format, reserved) {
        return true;
    }
    let mut restored = head.to_vec();
    restored[0..4].copy_from_slice(&WAL_MAGIC);
    if wal_seal::stamp_check_holds(&restored) {
        return true;
    }
    if head.len() < wal_frame::STAMP_LEN {
        return false;
    }
    let mut nonce = [0u8; 8];
    nonce.copy_from_slice(&fixed[12..20]);
    wal_frame::stamp_nonce(&fixed).is_ok()
        || wal_frame::decode_header(
            &head[wal_frame::STAMP_LEN..],
            u64::from_le_bytes(nonce),
            wal_frame::STAMP_LEN as u64,
            false,
        )
        .is_some()
}

fn invalid_wal(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub(super) fn read_exact_or_truncated(
    reader: &mut impl Read,
    buf: &mut [u8],
    message: &'static str,
) -> io::Result<()> {
    reader.read_exact(buf).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            io::Error::new(io::ErrorKind::UnexpectedEof, message)
        } else {
            e
        }
    })
}

pub(super) fn parse_put_record(data: &[u8]) -> io::Result<WalEntry> {
    if data.len() < 16 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "put record too short",
        ));
    }

    let mut pos = 0;
    let key_len = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
    pos += 4;

    if pos + key_len + 4 > data.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "put record key overflow",
        ));
    }

    let key = data[pos..pos + key_len].to_vec();
    pos += key_len;

    let value_len = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
    pos += 4;

    if pos + value_len + 8 > data.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "put record value overflow",
        ));
    }

    let value = data[pos..pos + value_len].to_vec();
    pos += value_len;

    let seq = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());

    Ok(WalEntry::Put { key, value, seq })
}

pub(super) fn parse_delete_record(data: &[u8]) -> io::Result<WalEntry> {
    if data.len() < 12 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "delete record too short",
        ));
    }

    let key_len = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
    if 4 + key_len + 8 > data.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "delete record key overflow",
        ));
    }

    let key = data[4..4 + key_len].to_vec();
    let seq = u64::from_le_bytes(data[4 + key_len..4 + key_len + 8].try_into().unwrap());

    Ok(WalEntry::Delete { key, seq })
}

pub(super) fn parse_delete_range_record(data: &[u8]) -> io::Result<WalEntry> {
    if data.len() < 16 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "delete_range record too short",
        ));
    }

    let mut pos = 0;
    let start_len = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
    pos += 4;

    if pos + start_len + 4 > data.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "delete_range record start overflow",
        ));
    }
    let start = data[pos..pos + start_len].to_vec();
    pos += start_len;

    let end_len = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
    pos += 4;

    if pos + end_len + 8 > data.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "delete_range record end overflow",
        ));
    }
    let end = data[pos..pos + end_len].to_vec();
    pos += end_len;

    let seq = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());

    Ok(WalEntry::DeleteRange { start, end, seq })
}

pub(super) fn parse_merge_record(data: &[u8]) -> io::Result<WalEntry> {
    if data.len() < 16 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "merge record too short",
        ));
    }

    let mut pos = 0;
    let key_len = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
    pos += 4;

    if pos + key_len + 4 > data.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "merge record key overflow",
        ));
    }

    let key = data[pos..pos + key_len].to_vec();
    pos += key_len;

    let operand_len = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
    pos += 4;

    if pos + operand_len + 8 > data.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "merge record operand overflow",
        ));
    }

    let operand = data[pos..pos + operand_len].to_vec();
    pos += operand_len;

    let seq = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());

    Ok(WalEntry::Merge { key, operand, seq })
}

#[cfg(test)]
#[path = "wal_tests.rs"]
mod tests;
