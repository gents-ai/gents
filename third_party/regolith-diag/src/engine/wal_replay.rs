//! Streaming reader over one write-ahead log file.
//!
//! Recovery reads the log one record at a time and hands each entry to
//! the memtable before touching the next, so replay holds one record's
//! payload rather than the whole log. The stamp says which framing the
//! records use: format 2, which this build writes, or format 1, which
//! 0.1.x wrote and which is still read by the rules it always was.
//!
//! The rule for format 2 is the one `proofs/tla/WalRecovery.tla` checks
//! and `proofs/lean/Regolith/WalRecovery.lean` proves: at the first
//! unusable record of the newest log, at O, refuse when a usable record
//! anywhere in the file proves a byte past O synced (O < P), and
//! otherwise end the log at O. Every unusable record is alike: torn,
//! zeroed and garbled tails are one case, not three.

use std::collections::VecDeque;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};

use std::sync::Arc;

use crate::env::{Env, ReadFile, ReadFileCursor};

/// Where a WAL file sits in the recovery order.
///
/// The torn-tail rule is only sound for the newest file. An earlier
/// file was synced whole before the rotation that created its successor
/// (`proofs/tla/WalRotation.tla`), or truncated and synced by the recovery
/// that created it, so no crash can leave a record in it half-written: a
/// damaged record there is media rot, and discarding it as a tail would
/// drop acknowledged writes while still serving the records of every
/// later file, leaving recovery on a state matching no prefix of the
/// write history.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum WalPosition {
    /// The file the database was writing to when it stopped.
    Newest,
    /// A file a rotation already closed.
    Earlier,
}

use super::checksum;
use super::seal::{Keyring, Sealer};
use super::wal::{
    RECORD_BATCH, RECORD_DELETE, RECORD_DELETE_RANGE, RECORD_MERGE, RECORD_PUT, TailVerdict,
    WAL_STAMP_LEN, WalEntry, parse_delete_range_record, parse_delete_record, parse_merge_record,
    parse_put_record, read_exact_or_truncated,
};
use super::wal_frame;
use super::wal_seal;
use super::wal_v1::{
    classify_incomplete_record, classify_unusable_record, parse_batch_record, read_wal_header,
};

/// How the records after the stamp are framed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Framing {
    /// No stamp: a log a crash caught before anything in it was synced.
    Unstamped,
    /// 0.1.x logs: one checksummed record per write.
    V1,
    /// One record per commit, each bound to this log's nonce.
    V2 { nonce: u64 },
}

/// Reads a WAL file record by record.
///
/// Peak live bytes are bounded by the largest single record in the
/// file plus the entries decoded from it. A format 2 group is decoded
/// one operation at a time from its payload; a format 1 batch record
/// yields its whole op list at once. A commit never writes a record
/// larger than one group, so the bound is "one group", not "one log".
pub(crate) struct WalReplayIter {
    reader: BufReader<ReadFileCursor<Box<dyn ReadFile>>>,
    path: PathBuf,
    /// Total file length, used to reject a length header that claims
    /// more bytes than the file holds *before* allocating for it. A
    /// corrupt header must not be able to ask for a 4 GiB buffer.
    file_len: u64,
    /// Bytes consumed so far, including record framing.
    consumed: u64,
    /// Reused record payload. Grows to the largest record seen and is
    /// not reallocated after that.
    payload: Vec<u8>,
    /// Where the next operation of the current format 2 group starts in
    /// `payload`; at `group_end` the group is done.
    group_at: usize,
    /// Where the current group's operations end in `payload`: its length,
    /// or, in a sealed log, the start of the tag.
    group_end: usize,
    /// Set when the log is sealed: every record's payload opens under this
    /// key, the one the sealed stamp names.
    seal: Option<Sealer>,
    /// Entries decoded from the current format 1 batch record, drained in
    /// order.
    pending: VecDeque<WalEntry>,
    /// Set when the replay stopped short of the last byte and discarded
    /// the rest as a crash artifact. Recovery truncates the log there and
    /// reports it.
    tail: Option<TailVerdict>,
    /// Whether the torn-tail rule applies to this file at all.
    position: WalPosition,
    framing: Framing,
    /// Whether a usable CLOSE was read. CLOSE proves the whole file
    /// durable, so nothing may follow it.
    closed: bool,
    /// Kept for the whole life of the iterator: classifying a format 1
    /// tail re-reads the log, and that read has to reach the same
    /// filesystem the records came from.
    env: Arc<dyn Env>,
}

/// Read up to `buf.len()` bytes, returning how many were available.
/// A short read is not an error here: a crash can leave a log shorter
/// than its own stamp.
fn read_full(reader: &mut impl io::Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut read = 0;
    while read < buf.len() {
        match reader.read(&mut buf[read..]) {
            Ok(0) => break,
            Ok(n) => read += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(read)
}

impl WalReplayIter {
    /// Open a WAL file for streaming replay, through `env`.
    ///
    /// Through `Env` rather than `std::fs`: recovery has to read the
    /// same filesystem the database was written to, and for an OPFS
    /// database in a browser `std::fs` is not merely the wrong file,
    /// it reports `Unsupported` and no reopen can ever replay.
    ///
    /// A log with no stamp yields nothing. The newest such log is reported
    /// as discarded whole when it holds any byte; an earlier one holding
    /// anything but zeros is damage and refuses here.
    ///
    /// A sealed log (`super::wal_seal`) opens only through `keyring`,
    /// under the key its stamp names, and refuses, naming the file, when
    /// the stamp's tag fails: a wrong key never reads as a torn tail.
    pub(crate) fn open(
        env: &Arc<dyn Env>,
        path: &Path,
        position: WalPosition,
        keyring: Option<&Keyring>,
    ) -> io::Result<Self> {
        let cursor = ReadFileCursor::new(env.open_read(path)?)?;
        let file_len = cursor.len();
        let mut reader = BufReader::new(cursor);

        let mut stamp = [0u8; wal_seal::SEALED_STAMP_LEN];
        let head = read_full(&mut reader, &mut stamp[..WAL_STAMP_LEN])?;
        let named = super::wal::validate_wal_stamp(&stamp[..head])
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
        let flags = u16::from_le_bytes([stamp[6], stamp[7]]);
        let mut seal = None;
        let (framing, consumed) = match named {
            None => (Framing::Unstamped, 0),
            Some(2) if flags == wal_seal::FLAG_SEALED => {
                let rest = read_full(&mut reader, &mut stamp[WAL_STAMP_LEN..])?;
                if WAL_STAMP_LEN + rest < wal_seal::SEALED_STAMP_LEN {
                    // A sealed log appears under its name only once its
                    // whole stamp is durable, so this is not a crash.
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("{}: the write-ahead log header is damaged", path.display()),
                    ));
                }
                let (nonce, sealer) = wal_seal::open_stamp(&stamp, keyring, path)?;
                seal = Some(sealer);
                (Framing::V2 { nonce }, wal_seal::SEALED_STAMP_LEN as u64)
            }
            Some(2) if flags == 0 => {
                let rest = read_full(&mut reader, &mut stamp[WAL_STAMP_LEN..wal_frame::STAMP_LEN])?;
                if WAL_STAMP_LEN + rest < wal_frame::STAMP_LEN {
                    // A stamp torn by a crash while the log was created.
                    (Framing::Unstamped, 0)
                } else {
                    let mut plain = [0u8; wal_frame::STAMP_LEN];
                    plain.copy_from_slice(&stamp[..wal_frame::STAMP_LEN]);
                    let nonce = wal_frame::stamp_nonce(&plain).map_err(|e| {
                        io::Error::new(e.kind(), format!("{}: {e}", path.display()))
                    })?;
                    (Framing::V2 { nonce }, wal_frame::STAMP_LEN as u64)
                }
            }
            Some(2) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "{}: the write-ahead log header carries flags {flags:#x}, which this \
                         version of regolith cannot read; open it with the version that wrote it",
                        path.display()
                    ),
                ));
            }
            Some(_) => (Framing::V1, WAL_STAMP_LEN as u64),
        };

        let mut iter = Self {
            reader,
            path: path.to_path_buf(),
            file_len,
            consumed,
            payload: Vec::new(),
            group_at: 0,
            group_end: 0,
            seal,
            pending: VecDeque::new(),
            tail: None,
            position,
            framing,
            closed: false,
            env: Arc::clone(env),
        };
        if framing == Framing::Unstamped {
            iter.unstamped()?;
        }
        Ok(iter)
    }

    /// Settle a log with no stamp: nothing in it was ever synced, unless
    /// what is left of the stamp shows it was written whole and rotted
    /// later, which refuses. The newest such log reports every byte it
    /// held as discarded. An earlier one is the leftover of a crash at its
    /// creation that a later recovery already replaced, and yields
    /// nothing.
    fn unstamped(&mut self) -> io::Result<()> {
        if self.file_len == 0 {
            return Ok(());
        }
        // A crash during creation leaves zeros or garbage, never a stamp
        // whose checks pass with only its magic wrong, nor records that
        // verify under its nonce.
        let mut head = [0u8; wal_frame::STAMP_LEN + wal_frame::HEADER_LEN];
        let n = (self.file_len as usize).min(head.len());
        self.reader
            .get_ref()
            .file()
            .read_exact_at(0, &mut head[..n])?;
        if super::wal::stamp_was_written(&head[..n]) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{}: the write-ahead log header is damaged",
                    self.path.display()
                ),
            ));
        }
        if self.position == WalPosition::Newest {
            self.tail = Some(TailVerdict {
                offset: 0,
                discarded_bytes: self.file_len,
            });
        }
        // Nothing is read past the missing stamp.
        self.file_len = 0;
        Ok(())
    }

    /// The next entry, or `None` at the end of the log.
    ///
    /// The end of the log is the end of the file, or the first unusable
    /// record of the newest log when nothing proves it synced; the
    /// discard is then recorded for [`Self::discarded_tail`]. Damage that
    /// held synced writes, and any damage in an earlier log, is an error.
    pub(crate) fn next_entry(&mut self) -> io::Result<Option<WalEntry>> {
        match self.framing {
            Framing::Unstamped => Ok(None),
            Framing::V1 => self.next_v1_entry(),
            Framing::V2 { nonce } => self.next_v2_entry(nonce),
        }
    }

    fn next_v1_entry(&mut self) -> io::Result<Option<WalEntry>> {
        let record_start = self.consumed;
        match self.next_v1_entry_inner() {
            // A record the file ends inside is the ordinary shape of a
            // crash. Whether the tail is torn or is damage with whole
            // records behind it cannot be told from a streaming read, so
            // the decision is delegated to the same discriminator the
            // whole-file replay uses.
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                if self.position == WalPosition::Earlier {
                    return Err(self.damage_in_a_closed_file(record_start));
                }
                self.tail = Some(classify_incomplete_record(
                    &*self.env,
                    &self.path,
                    record_start,
                )?);
                Ok(None)
            }
            // A record that framed cleanly but carries an unusable type
            // or a failing checksum is corruption unless everything from
            // it on is zeros, which is how an unwritten or power-zeroed
            // tail reads back.
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                if self.position == WalPosition::Earlier {
                    return Err(self.damage_in_a_closed_file(record_start));
                }
                self.tail = Some(classify_unusable_record(
                    &*self.env,
                    &self.path,
                    record_start,
                )?);
                Ok(None)
            }
            other => other,
        }
    }

    fn damage_in_a_closed_file(&self, offset: u64) -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "write-ahead log {} is damaged at offset {offset}, and a newer log \
                 follows it; refusing to open rather than lose the writes it held",
                self.path.display()
            ),
        )
    }

    /// Where this replay stopped short, if it did.
    pub(crate) fn discarded_tail(&self) -> Option<TailVerdict> {
        self.tail
    }

    fn next_v2_entry(&mut self, nonce: u64) -> io::Result<Option<WalEntry>> {
        loop {
            if self.group_at < self.group_end {
                let (entry, len) =
                    wal_frame::decode_entry(&self.payload[self.group_at..self.group_end])?;
                self.group_at += len;
                return Ok(Some(entry));
            }
            if self.consumed == self.file_len {
                return Ok(None);
            }
            let record_start = self.consumed;
            if self.closed || !self.read_v2_record(nonce)? {
                return self.unusable_v2_record(nonce, record_start);
            }
        }
    }

    /// Read the format 2 record at `consumed`. `Ok(false)` when it is not
    /// usable: the file ends inside it, a check or its tag fails, or its
    /// operations do not parse. A record's operations are all checked here,
    /// before any is yielded, so a commit is replayed whole or not at all.
    fn read_v2_record(&mut self, nonce: u64) -> io::Result<bool> {
        let offset = self.consumed;
        let remaining = self.file_len - offset;
        if remaining < wal_frame::HEADER_LEN as u64 {
            return Ok(false);
        }
        let mut bytes = [0u8; wal_frame::HEADER_LEN];
        read_exact_or_truncated(&mut self.reader, &mut bytes, "truncated WAL record header")?;
        let Some(header) = wal_frame::decode_header(&bytes, nonce, offset, self.seal.is_some())
        else {
            return Ok(false);
        };
        // Checked before anything is sized from the length, so a header
        // that verified but runs past the file cannot ask for a buffer
        // the file could not fill.
        if header.record_len() > remaining {
            return Ok(false);
        }
        self.payload.clear();
        self.payload.resize(header.len as usize, 0);
        read_exact_or_truncated(&mut self.reader, &mut self.payload, "truncated WAL record")?;
        let operations = match &self.seal {
            Some(sealer) => {
                wal_seal::open_record(sealer, &header.fields(), nonce, offset, &mut self.payload)
            }
            None => header
                .payload_matches(&self.payload)
                .then_some(0..self.payload.len()),
        };
        let Some(operations) =
            operations.filter(|ops| wal_frame::entries_are_whole(&self.payload[ops.clone()]))
        else {
            self.payload.clear();
            return Ok(false);
        };
        self.consumed += header.record_len();
        self.group_at = operations.start;
        self.group_end = operations.end;
        self.closed = header.kind == wal_frame::KIND_CLOSE;
        Ok(true)
    }

    /// The format 2 record at `offset` is not usable. In an earlier log
    /// that is damage. In the newest it ends the log, unless a usable
    /// record proves a byte past `offset` synced.
    fn unusable_v2_record(&mut self, nonce: u64, offset: u64) -> io::Result<Option<WalEntry>> {
        self.payload.clear();
        self.group_at = 0;
        self.group_end = 0;
        if self.position == WalPosition::Earlier {
            return Err(self.damage_in_a_closed_file(offset));
        }
        // Every record read before `offset` claims no more than its own
        // offset, so only a CLOSE among them, or a record after `offset`,
        // can prove the damage synced.
        let proof = if self.closed {
            Some(self.file_len)
        } else {
            wal_frame::proof_past(
                self.reader.get_ref().file(),
                nonce,
                offset + 1,
                self.file_len,
                offset,
                self.seal.as_ref(),
            )?
        };
        if let Some(synced) = proof {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "write-ahead log {} is damaged at offset {offset}, but offsets up to \
                     {synced} were already made durable; refusing to open rather than \
                     lose acknowledged writes",
                    self.path.display()
                ),
            ));
        }
        self.tail = Some(TailVerdict {
            offset,
            discarded_bytes: self.file_len - offset,
        });
        self.consumed = self.file_len;
        Ok(None)
    }

    fn next_v1_entry_inner(&mut self) -> io::Result<Option<WalEntry>> {
        loop {
            if let Some(entry) = self.pending.pop_front() {
                return Ok(Some(entry));
            }

            let Some(header) = read_wal_header(&mut self.reader)? else {
                return Ok(None);
            };
            self.consumed += header.len() as u64;
            let len = u32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
            let record_type = header[4];

            // A record claiming more bytes than the file can still hold
            // is truncated by definition. Deciding that from the header
            // keeps a corrupt length from sizing an allocation.
            // A length past the format's maximum is a mangled length
            // field, which is the same signal as one claiming more bytes
            // than the file holds: truncated. Reporting it as corruption
            // instead would refuse to open after an ordinary torn write.
            // The check exists so the number never sizes an allocation.
            if len as u64 > super::wal::MAX_RECORD_LEN as u64 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "WAL record length exceeds the format's maximum",
                ));
            }
            let remaining = self.file_len.saturating_sub(self.consumed);
            if len as u64 + 4 > remaining {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated WAL record data",
                ));
            }

            self.payload.clear();
            self.payload.resize(len, 0);
            read_exact_or_truncated(
                &mut self.reader,
                &mut self.payload,
                "truncated WAL record data",
            )?;
            self.consumed += len as u64;

            let mut checksum_bytes = [0u8; 4];
            read_exact_or_truncated(
                &mut self.reader,
                &mut checksum_bytes,
                "truncated WAL record checksum",
            )?;
            self.consumed += 4;

            let stored_checksum = u32::from_le_bytes(checksum_bytes);
            let computed_checksum = checksum::wal_record(len as u32, record_type, &self.payload);
            if stored_checksum != computed_checksum {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("WAL checksum mismatch in {}", self.path.display()),
                ));
            }

            match record_type {
                RECORD_PUT => return Ok(Some(parse_put_record(&self.payload)?)),
                RECORD_DELETE => return Ok(Some(parse_delete_record(&self.payload)?)),
                RECORD_DELETE_RANGE => {
                    return Ok(Some(parse_delete_range_record(&self.payload)?));
                }
                RECORD_MERGE => return Ok(Some(parse_merge_record(&self.payload)?)),
                RECORD_BATCH => {
                    self.pending.extend(parse_batch_record(&self.payload)?);
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("unknown WAL record type {record_type}"),
                    ));
                }
            }
        }
    }

    /// Largest record payload this iterator has had to buffer. The
    /// replay-memory bound is stated in terms of this number.
    #[cfg(test)]
    pub(crate) fn high_water_bytes(&self) -> usize {
        self.payload.capacity()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WriteBatchOp;
    use crate::engine::wal::Wal;
    use tempfile::TempDir;

    fn drain(path: &Path) -> io::Result<Vec<WalEntry>> {
        let mut iter =
            WalReplayIter::open(&crate::env::std_env(), path, WalPosition::Newest, None)?;
        let mut out = Vec::new();
        while let Some(entry) = iter.next_entry()? {
            out.push(entry);
        }
        Ok(out)
    }

    #[test]
    fn streams_every_record_type_in_order() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("mixed.wal");
        {
            let mut wal = Wal::create(&path).unwrap();
            wal.append_put(b"a", b"1", 1).unwrap();
            wal.append_delete(b"b", 2).unwrap();
            wal.append_merge(b"c", b"op", 3).unwrap();
            wal.append_delete_range(b"d", b"e", 4).unwrap();
            let mut group = Vec::new();
            crate::engine::wal::encode_ops_record(
                &mut group,
                &[
                    WriteBatchOp::Put {
                        key: b"f".to_vec(),
                        value: b"2".to_vec(),
                    },
                    WriteBatchOp::Delete { key: b"g".to_vec() },
                ],
                5,
            );
            wal.append_group(&group).unwrap();
            wal.sync_data().unwrap();
        }

        let entries = drain(&path).unwrap();
        assert_eq!(
            entries,
            vec![
                WalEntry::Put {
                    key: b"a".to_vec(),
                    value: b"1".to_vec(),
                    seq: 1
                },
                WalEntry::Delete {
                    key: b"b".to_vec(),
                    seq: 2
                },
                WalEntry::Merge {
                    key: b"c".to_vec(),
                    operand: b"op".to_vec(),
                    seq: 3
                },
                WalEntry::DeleteRange {
                    start: b"d".to_vec(),
                    end: b"e".to_vec(),
                    seq: 4
                },
                WalEntry::Put {
                    key: b"f".to_vec(),
                    value: b"2".to_vec(),
                    seq: 5
                },
                WalEntry::Delete {
                    key: b"g".to_vec(),
                    seq: 6
                },
            ]
        );
    }

    #[test]
    fn empty_log_yields_nothing() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("empty.wal");
        Wal::create(&path).unwrap().sync_data().unwrap();
        assert!(drain(&path).unwrap().is_empty());
    }

    #[test]
    fn streamed_entries_match_the_batch_reader() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("parity.wal");
        {
            let mut wal = Wal::create(&path).unwrap();
            for i in 0..256u64 {
                wal.append_put(format!("key{i:04}").as_bytes(), &[b'v'; 37], i + 1)
                    .unwrap();
            }
            wal.sync_data().unwrap();
        }
        assert_eq!(drain(&path).unwrap(), Wal::replay(&path).unwrap());
    }

    #[test]
    fn truncated_tail_ends_the_log_after_the_good_prefix() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("truncated.wal");
        {
            let mut wal = Wal::create(&path).unwrap();
            wal.append_put(b"a", b"1", 1).unwrap();
            wal.append_put(b"b", b"2", 2).unwrap();
            wal.sync_data().unwrap();
        }
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.truncate(bytes.len() - 3);
        std::fs::write(&path, &bytes).unwrap();

        let mut iter =
            WalReplayIter::open(&crate::env::std_env(), &path, WalPosition::Newest, None).unwrap();
        assert!(
            iter.next_entry().unwrap().is_some(),
            "first record is whole"
        );
        // A record the file ends inside is the ordinary shape of a
        // crash, so the whole records before it stand and the tail is
        // discarded. Erroring here would refuse to open a database
        // after an ordinary `kill -9`.
        assert!(
            iter.next_entry().unwrap().is_none(),
            "a torn trailing record ends the log rather than failing it"
        );
    }

    /// A checksum mismatch in a record a later record proves synced is an
    /// error; the same damage in the last record, which nothing vouches
    /// for, is a tail.
    #[test]
    fn checksum_mismatch_below_a_later_proof_is_an_error() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("corrupt.wal");
        {
            let mut wal = Wal::create(&path).unwrap();
            wal.append_put(b"a", b"1", 1).unwrap();
            wal.sync_data().unwrap();
            wal.append_put(b"b", b"2", 2).unwrap();
            wal.sync_data().unwrap();
        }
        let clean = std::fs::read(&path).unwrap();
        let mut bytes = clean.clone();
        // The first record's last payload byte.
        let first_end = crate::engine::wal_frame::STAMP_LEN
            + crate::engine::wal_frame::HEADER_LEN
            + crate::engine::wal::put_record_len(b"a", b"1");
        bytes[first_end - 1] ^= 0xFF;
        std::fs::write(&path, &bytes).unwrap();
        let mut iter =
            WalReplayIter::open(&crate::env::std_env(), &path, WalPosition::Newest, None).unwrap();
        let err = iter.next_entry().expect_err("checksum must not pass");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);

        let mut bytes = clean;
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        std::fs::write(&path, &bytes).unwrap();
        let mut iter =
            WalReplayIter::open(&crate::env::std_env(), &path, WalPosition::Newest, None).unwrap();
        assert!(iter.next_entry().unwrap().is_some());
        assert!(iter.next_entry().unwrap().is_none());
        assert_eq!(iter.discarded_tail().unwrap().offset, first_end as u64);
    }

    #[test]
    fn oversized_length_header_is_rejected_without_allocating() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bad-len.wal");
        // A format 1 record header claiming 3 GiB, then nothing.
        let mut bytes = crate::engine::wal_v1::stamp().to_vec();
        bytes.extend_from_slice(&(3u32 * 1024 * 1024 * 1024).to_le_bytes());
        bytes.push(RECORD_PUT);
        std::fs::write(&path, &bytes).unwrap();

        let mut iter =
            WalReplayIter::open(&crate::env::std_env(), &path, WalPosition::Newest, None).unwrap();
        // Nothing follows the bogus length, so it reads as a torn tail.
        // The point of the test is the allocation, not the verdict.
        assert!(iter.next_entry().unwrap().is_none());
        assert_eq!(
            iter.high_water_bytes(),
            0,
            "a bogus length must not size an allocation"
        );
    }

    #[test]
    fn payload_buffer_is_reused_across_records() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("reuse.wal");
        {
            let mut wal = Wal::create(&path).unwrap();
            for i in 0..64u64 {
                wal.append_put(b"k", &vec![b'v'; 512], i + 1).unwrap();
            }
            wal.sync_data().unwrap();
        }
        let mut iter =
            WalReplayIter::open(&crate::env::std_env(), &path, WalPosition::Newest, None).unwrap();
        let mut count = 0;
        while iter.next_entry().unwrap().is_some() {
            count += 1;
        }
        assert_eq!(count, 64);
        // One record is ~530 bytes; the buffer must be sized for one
        // record, not for the whole 34 KiB log.
        assert!(
            iter.high_water_bytes() < 4096,
            "payload buffer grew to {} bytes",
            iter.high_water_bytes()
        );
    }
}
