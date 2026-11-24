//! The MANIFEST: the log of which tables the database holds at each level,
//! and the version it replays into.
//!
//! # Format
//!
//! A stamp (`REGOMAN`, a format byte, a checksum; sealed on an encrypted
//! database), then batches. A batch is one [`VersionSet::apply`]: every
//! record of it under one length and one checksum, so recovery installs all
//! of an edit or none of it (`tail.rs` says how the end of the log is
//! judged). A record is a tag byte and its fields:
//!
//! ```text
//! 1  a table added to a level: level, file id, key range, size, entries
//! 2  a table removed from a level
//! 3  the last sequence held in tables
//! 4  the next file id
//! 5  the oldest write-ahead log still needed
//! 6  every table dropped at once
//! 7  tag 1 for an ingested table, with the sequence its entries read at
//! 8  the deletion counts of a table this manifest lists: file id, point
//!    deletions, hidden entries, newest point deletion
//! ```
//!
//! # Tag 8 and older databases
//!
//! A table written by this build is recorded as its tag 1 or tag 7 followed
//! by a tag 8 in the same batch, so a power cut leaves both or neither. A
//! table recorded without a tag 8 (by a build that predates it, or restored
//! from a backup) reads back with its counts unknown and is counted in the
//! background by the census (`compaction::census`), which then appends a tag
//! 8 on its own. That batch is not synced: it adds knowledge about a table
//! and changes no read, so a crash that loses it leaves the table unknown
//! and it is counted again.
//!
//! A tag 8 naming a table the manifest no longer lists is ignored: the
//! table was compacted away, and nothing is left to know about it. A tag 8
//! whose counts cannot be those of its table is ignored too.
//!
//! So a database written by an earlier build opens unchanged. The reverse
//! does not hold, as for tag 7 before it: a build that predates tag 8
//! refuses a manifest holding one, naming the unknown tag.

use std::io::{self};
use std::path::{Path, PathBuf};

// The module's own tests craft corrupt manifests byte by byte, which
// is the one thing that has to bypass the environment.
#[cfg(test)]
use std::fs::OpenOptions;
use std::sync::Arc;

use crate::env::{BufferedWriter, Env, WriteMode};

use super::checksum;
use super::seal::Keyring;
use super::sstable::{
    Deletions, LiveSst, MetadataPolicy, SsTableMeta, SsTableReader, sst_filename,
};
use crate::encryption::KeyId;

mod sealed;
use sealed::{MANIFEST_FORMAT_SEALED, ManifestSeal, SEALED_STAMP_LEN};

mod suspects;
mod tail;
pub(crate) use tail::DroppedTail;

mod legacy_overlap;
pub(crate) use legacy_overlap::Demotion;

/// Maximum number of levels in the LSM tree.
pub(crate) const MAX_LEVELS: usize = 7;

/// A snapshot of which SSTables exist at each level.
///
/// Each level holds `Arc<LiveSst>` - the metadata plus an open reader -
/// so that every file referenced by a live version has a pinned file
/// descriptor. Concurrent compaction can safely `unlink` a file as soon
/// as it's removed from the *current* version because the Arcs in older
/// versions keep the FD alive until those versions are dropped.
///
/// Order within a level matters. L0 is in age order, oldest first, because
/// its tables overlap and recency is position. Every deeper level is one
/// sorted run: its tables ascend by smallest key (a table added later sorts
/// after any with the same smallest key) and never overlap except at a shared
/// boundary key, `files[i].largest_key <= files[i + 1].smallest_key`. That is
/// what lets a read find the tables covering a key by binary search, see
/// [`overlapping`].
#[derive(Clone)]
pub(crate) struct Version {
    pub(crate) levels: Vec<Vec<Arc<LiveSst>>>,
    pub(crate) next_file_id: u64,
    /// Upper bound of every sequence in this version's tables, including
    /// ingested global sequences. Installing a table raises it in the same
    /// edit batch; only Reset may lower it while removing every table.
    pub(crate) last_seq: u64,
    pub(crate) min_wal_id: u64,
    /// How many of the tables have no deletion counts yet
    /// ([`SsTableMeta::deletions`] is `None`): what is left for the census.
    /// Kept as edits apply, so reading it walks nothing.
    pub(crate) uncounted: usize,
}

impl Version {
    pub(crate) fn new() -> Self {
        Self {
            levels: (0..MAX_LEVELS).map(|_| Vec::new()).collect(),
            next_file_id: 1,
            last_seq: 0,
            min_wal_id: 0,
            uncounted: 0,
        }
    }

    /// Number of SSTables at L0.
    pub(crate) fn l0_count(&self) -> usize {
        self.levels[0].len()
    }

    /// Total size of SSTables at a given level.
    pub(crate) fn level_size(&self, level: usize) -> u64 {
        self.levels[level].iter().map(|f| f.meta.file_size).sum()
    }

    /// Place `file` in `level`: at the end of L0, whose order is age, and
    /// deeper at its sorted position, after any table with the same smallest
    /// key.
    fn add_file(&mut self, level: usize, file: Arc<LiveSst>) {
        self.uncounted += usize::from(file.meta.deletions.is_none());
        let files = &mut self.levels[level];
        if level == 0 {
            files.push(file);
        } else {
            let at = files.partition_point(|f| f.meta.smallest_key <= file.meta.smallest_key);
            files.insert(at, file);
        }
    }

    /// The first level below L0, and the two neighbouring tables in it, that
    /// overlap beyond a shared boundary key, which [`overlapping`] cannot
    /// search. `None` when every level below L0 is a sorted run.
    pub(crate) fn find_overlap(&self) -> Option<(usize, &SsTableMeta, &SsTableMeta)> {
        self.levels
            .iter()
            .enumerate()
            .skip(1)
            .find_map(|(level, files)| {
                files
                    .windows(2)
                    .find(|pair| pair[0].meta.largest_key > pair[1].meta.smallest_key)
                    .map(|pair| (level, &pair[0].meta, &pair[1].meta))
            })
    }

    /// Whether every level below L0 is a sorted run of tables that overlap
    /// only at a shared boundary key.
    pub(crate) fn levels_are_sorted_runs(&self) -> bool {
        self.find_overlap().is_none()
    }

    /// Take table `file_id` out of `level`, if it is there.
    fn remove_file(&mut self, level: usize, file_id: u64) {
        let Self {
            levels, uncounted, ..
        } = self;
        levels[level].retain(|file| {
            let stays = file.meta.file_id != file_id;
            if !stays && file.meta.deletions.is_none() {
                *uncounted = uncounted.saturating_sub(1);
            }
            stays
        });
    }

    /// Record `deletions` as the counts of table `file_id` of `level`, in
    /// place: the table keeps its position, its reader and its file. A table
    /// that is not there is left alone.
    fn count_file(&mut self, level: usize, file_id: u64, deletions: Deletions) {
        let Self {
            levels, uncounted, ..
        } = self;
        let Some(slot) = levels[level]
            .iter_mut()
            .find(|file| file.meta.file_id == file_id)
        else {
            return;
        };
        if slot.meta.deletions.is_none() {
            *uncounted = uncounted.saturating_sub(1);
        }
        *slot = LiveSst::new(
            SsTableMeta {
                deletions: Some(deletions),
                ..slot.meta.clone()
            },
            Arc::clone(&slot.reader),
            slot.sequence_ceiling,
        );
    }
}

/// The tables of `files`, one level below L0 in order, whose key range
/// intersects `[first, last]`, with `first <= last`. They are a contiguous run
/// of the level, found by binary search: the first table ending at or after
/// `first`, through the last one starting at or before `last`.
#[inline]
pub(crate) fn overlapping<'level>(
    files: &'level [Arc<LiveSst>],
    first: &[u8],
    last: &[u8],
) -> &'level [Arc<LiveSst>] {
    let from = files.partition_point(|f| f.meta.largest_key.as_slice() < first);
    let len = files[from..].partition_point(|f| f.meta.smallest_key.as_slice() <= last);
    &files[from..from + len]
}

/// The tables of `files`, one level below L0 in order, whose key range covers
/// `key`. More than one only where a table ends on the key another begins at.
#[inline]
pub(crate) fn covering<'level>(
    files: &'level [Arc<LiveSst>],
    key: &[u8],
) -> &'level [Arc<LiveSst>] {
    overlapping(files, key, key)
}

/// A runtime mutation to the version. Carries `Arc<LiveSst>` for
/// `AddFile` so the caller is responsible for opening the reader before
/// the apply, and the manifest machinery never has to touch the
/// filesystem for a runtime edit.
#[derive(Clone)]
pub(crate) enum VersionEdit {
    AddFile {
        level: usize,
        file: Arc<LiveSst>,
    },
    RemoveFile {
        level: usize,
        file_id: u64,
    },
    /// The deletion counts of a table already at `level`, which had none:
    /// what the census found by reading it. Changes no read.
    CountFile {
        level: usize,
        file_id: u64,
        deletions: Deletions,
    },
    SetLastSeq(u64),
    SetNextFileId(u64),
    SetMinWalId(u64),
    Reset {
        next_file_id: u64,
        min_wal_id: u64,
    },
}

/// Serialized form of a version edit. All edits in one apply share a
/// checksummed frame, so recovery cannot install half a file replacement.
enum ManifestRecord {
    AddFile {
        level: usize,
        meta: SsTableMeta,
    },
    RemoveFile {
        level: usize,
        file_id: u64,
    },
    TableCounts {
        file_id: u64,
        deletions: Deletions,
    },
    SetLastSeq(u64),
    SetNextFileId(u64),
    SetMinWalId(u64),
    Reset {
        next_file_id: u64,
        min_wal_id: u64,
    },
    /// The range deletes of this manifest's tables have been checked for
    /// ones an earlier release left behind older keys they cover, and those
    /// were copied in front of them (`engine::buried_range_deletes`).
    RangeDeletesChecked,
}

const TAG_ADD_FILE: u8 = 1;
const TAG_REMOVE_FILE: u8 = 2;
const TAG_LAST_SEQ: u8 = 3;
const TAG_NEXT_FILE_ID: u8 = 4;
const TAG_MIN_WAL_ID: u8 = 5;
const TAG_RESET: u8 = 6;
/// An `AddFile` for an ingested table: the same fields, then the sequence
/// every entry of the table reads at (D48). A manifest written before this
/// record existed never carries it, so it reads unchanged; a build that
/// predates the record refuses one that does, naming the unknown tag.
const TAG_ADD_INGESTED_FILE: u8 = 7;
/// The deletion counts of a table this manifest lists (D14): its file id,
/// then its point deletions, its hidden entries and its newest point
/// deletion's sequence. Written after a new table's `AddFile` in the same
/// batch, and alone by the census for a table recorded without one. The
/// module docs say what a build that predates it does, and what this build
/// does with a table that has none.
const TAG_TABLE_COUNTS: u8 = 8;
/// [`ManifestRecord::RangeDeletesChecked`], no payload. Every manifest this
/// build writes carries it; one that does not was written by an earlier
/// release, whose first open by this build runs the check and records it.
/// A build that predates the record refuses a manifest holding it, naming
/// the unknown tag.
const TAG_RANGE_DELETES_CHECKED: u8 = 9;

impl VersionEdit {
    /// The records this edit is logged as, appended to `records`.
    fn to_records(&self, records: &mut Vec<ManifestRecord>) {
        records.push(match self {
            VersionEdit::AddFile { level, file } => {
                push_table(records, *level, file.meta.clone());
                return;
            }
            VersionEdit::RemoveFile { level, file_id } => ManifestRecord::RemoveFile {
                level: *level,
                file_id: *file_id,
            },
            VersionEdit::CountFile {
                file_id, deletions, ..
            } => ManifestRecord::TableCounts {
                file_id: *file_id,
                deletions: *deletions,
            },
            VersionEdit::SetLastSeq(seq) => ManifestRecord::SetLastSeq(*seq),
            VersionEdit::SetNextFileId(id) => ManifestRecord::SetNextFileId(*id),
            VersionEdit::SetMinWalId(id) => ManifestRecord::SetMinWalId(*id),
            VersionEdit::Reset {
                next_file_id,
                min_wal_id,
            } => ManifestRecord::Reset {
                next_file_id: *next_file_id,
                min_wal_id: *min_wal_id,
            },
        });
    }
}

/// The records of one table at `level`: its `AddFile`, then its counts when
/// it has them. The one place a table becomes records, so an edit and a
/// whole manifest image write a table the same way.
fn push_table(records: &mut Vec<ManifestRecord>, level: usize, meta: SsTableMeta) {
    let counts = meta.deletions.map(|deletions| ManifestRecord::TableCounts {
        file_id: meta.file_id,
        deletions,
    });
    records.push(ManifestRecord::AddFile { level, meta });
    records.extend(counts);
}

impl ManifestRecord {
    /// Whether a batch holding this record is synced before
    /// [`VersionSet::apply`] returns. The one rule both the writer and the
    /// open's tail judgment (`tail.rs`) read, so the two cannot disagree on
    /// which batches a crash may lose.
    ///
    /// A file-id reservation and a log retirement change no read if lost:
    /// the ids a lost reservation handed out name nothing a durable batch
    /// names, and the logs a lost retirement covered are replayed again,
    /// holding only writes that the log the open rewrote them into also
    /// holds. Each becomes durable with the next synced batch.
    ///
    /// A table's counts lost on their own change no read either: the table
    /// reads back with its counts unknown, and the census counts it again.
    ///
    /// The record that the range deletes were checked does not ask for a
    /// sync on its own either: in a new manifest it rides the first batch,
    /// and a later synced batch, which every table's record is, makes it
    /// durable before the database holds anything to check. The version set
    /// syncs the batch that records it in a manifest written without it
    /// (`VersionSet::sync_check_record`).
    fn requires_sync(&self) -> bool {
        !matches!(
            self,
            ManifestRecord::SetNextFileId(_)
                | ManifestRecord::SetMinWalId(_)
                | ManifestRecord::TableCounts { .. }
                | ManifestRecord::RangeDeletesChecked
        )
    }

    fn encode(&self, buf: &mut Vec<u8>) {
        match self {
            ManifestRecord::AddFile { level, meta } => {
                buf.push(match meta.global_seq {
                    Some(_) => TAG_ADD_INGESTED_FILE,
                    None => TAG_ADD_FILE,
                });
                buf.extend_from_slice(&(*level as u32).to_le_bytes());
                buf.extend_from_slice(&meta.file_id.to_le_bytes());
                buf.extend_from_slice(&(meta.smallest_key.len() as u32).to_le_bytes());
                buf.extend_from_slice(&meta.smallest_key);
                buf.extend_from_slice(&(meta.largest_key.len() as u32).to_le_bytes());
                buf.extend_from_slice(&meta.largest_key);
                buf.extend_from_slice(&meta.file_size.to_le_bytes());
                buf.extend_from_slice(&meta.num_entries.to_le_bytes());
                if let Some(seq) = meta.global_seq {
                    buf.extend_from_slice(&seq.to_le_bytes());
                }
            }
            ManifestRecord::RemoveFile { level, file_id } => {
                buf.push(TAG_REMOVE_FILE);
                buf.extend_from_slice(&(*level as u32).to_le_bytes());
                buf.extend_from_slice(&file_id.to_le_bytes());
            }
            ManifestRecord::TableCounts { file_id, deletions } => {
                buf.push(TAG_TABLE_COUNTS);
                buf.extend_from_slice(&file_id.to_le_bytes());
                buf.extend_from_slice(&deletions.points.to_le_bytes());
                buf.extend_from_slice(&deletions.dead.to_le_bytes());
                buf.extend_from_slice(&deletions.newest_seq.to_le_bytes());
            }
            ManifestRecord::SetLastSeq(seq) => {
                buf.push(TAG_LAST_SEQ);
                buf.extend_from_slice(&seq.to_le_bytes());
            }
            ManifestRecord::SetNextFileId(id) => {
                buf.push(TAG_NEXT_FILE_ID);
                buf.extend_from_slice(&id.to_le_bytes());
            }
            ManifestRecord::SetMinWalId(id) => {
                buf.push(TAG_MIN_WAL_ID);
                buf.extend_from_slice(&id.to_le_bytes());
            }
            ManifestRecord::Reset {
                next_file_id,
                min_wal_id,
            } => {
                buf.push(TAG_RESET);
                buf.extend_from_slice(&next_file_id.to_le_bytes());
                buf.extend_from_slice(&min_wal_id.to_le_bytes());
            }
            ManifestRecord::RangeDeletesChecked => buf.push(TAG_RANGE_DELETES_CHECKED),
        }
    }

    fn decode(data: &[u8], pos: &mut usize) -> io::Result<Option<Self>> {
        if *pos >= data.len() {
            return Ok(None);
        }

        let tag = data[*pos];
        *pos += 1;

        match tag {
            TAG_ADD_FILE | TAG_ADD_INGESTED_FILE => {
                let level = read_u32(data, pos)? as usize;
                validate_level_index(level)?;
                let file_id = read_u64(data, pos)?;
                let smallest_key = read_bytes(data, pos)?;
                let largest_key = read_bytes(data, pos)?;
                let file_size = read_u64(data, pos)?;
                let num_entries = read_u64(data, pos)?;
                let global_seq = if tag == TAG_ADD_INGESTED_FILE {
                    // Sequences start at 1, so 0 can only be damage.
                    match read_u64(data, pos)? {
                        0 => {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!("manifest records ingested table {file_id} at sequence 0"),
                            ));
                        }
                        seq => Some(seq),
                    }
                } else {
                    None
                };

                Ok(Some(ManifestRecord::AddFile {
                    level,
                    meta: SsTableMeta {
                        file_id,
                        smallest_key,
                        largest_key,
                        file_size,
                        num_entries,
                        global_seq,
                        deletions: None,
                    },
                }))
            }
            TAG_REMOVE_FILE => {
                let level = read_u32(data, pos)? as usize;
                validate_level_index(level)?;
                let file_id = read_u64(data, pos)?;
                Ok(Some(ManifestRecord::RemoveFile { level, file_id }))
            }
            TAG_TABLE_COUNTS => {
                let file_id = read_u64(data, pos)?;
                let deletions = Deletions {
                    points: read_u64(data, pos)?,
                    dead: read_u64(data, pos)?,
                    newest_seq: read_u64(data, pos)?,
                };
                Ok(Some(ManifestRecord::TableCounts { file_id, deletions }))
            }
            TAG_LAST_SEQ => {
                let seq = read_u64(data, pos)?;
                Ok(Some(ManifestRecord::SetLastSeq(seq)))
            }
            TAG_NEXT_FILE_ID => {
                let id = read_u64(data, pos)?;
                Ok(Some(ManifestRecord::SetNextFileId(id)))
            }
            TAG_MIN_WAL_ID => {
                let id = read_u64(data, pos)?;
                Ok(Some(ManifestRecord::SetMinWalId(id)))
            }
            TAG_RESET => {
                let next_file_id = read_u64(data, pos)?;
                let min_wal_id = read_u64(data, pos)?;
                Ok(Some(ManifestRecord::Reset {
                    next_file_id,
                    min_wal_id,
                }))
            }
            TAG_RANGE_DELETES_CHECKED => Ok(Some(ManifestRecord::RangeDeletesChecked)),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown manifest record tag: {}", tag),
            )),
        }
    }
}

fn read_u32(data: &[u8], pos: &mut usize) -> io::Result<u32> {
    if *pos + 4 > data.len() {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "short read"));
    }
    let val = u32::from_le_bytes(data[*pos..*pos + 4].try_into().unwrap());
    *pos += 4;
    Ok(val)
}

fn read_u64(data: &[u8], pos: &mut usize) -> io::Result<u64> {
    if *pos + 8 > data.len() {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "short read"));
    }
    let val = u64::from_le_bytes(data[*pos..*pos + 8].try_into().unwrap());
    *pos += 8;
    Ok(val)
}

fn read_bytes(data: &[u8], pos: &mut usize) -> io::Result<Vec<u8>> {
    let len = read_u32(data, pos)? as usize;
    if *pos + len > data.len() {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "short read"));
    }
    let bytes = data[*pos..*pos + len].to_vec();
    *pos += len;
    Ok(bytes)
}

fn validate_level_index(level: usize) -> io::Result<()> {
    if level < MAX_LEVELS {
        return Ok(());
    }

    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!("manifest level {level} out of range 0..{MAX_LEVELS}"),
    ))
}

/// The sequence a `SetLastSeq` leaves behind. A stamp never lowers the
/// value: a flush stamps the sequence its memtable was sealed at and an
/// ingest stamps the one it allocated, and the two are applied in the
/// order their tables finish, not the order the sequences were handed
/// out. The log still records the stamp as issued, so `replay_manifest`
/// applies the same rule to a log written before this rule existed.
fn raised_last_seq(current: u64, stamp: u64) -> u64 {
    current.max(stamp)
}

/// A whole MANIFEST describing one version: the stamp, then one frame
/// holding the file-id counter, the last sequence, the oldest live WAL, the
/// record that the range deletes were checked when `range_deletes_checked`,
/// and an `AddFile` per table, level by level in the order given.
///
/// The one encoder of a canonical manifest: the rewrite that bounds the log
/// and a backup restore both write it, so the two cannot drift apart.
///
/// With a `keyring` the image is sealed, under a fresh salt and the
/// keyring's current key, exactly as the open's rewrite seals one; a
/// current key the keyring cannot provide fails with
/// [`crate::Error::UnknownKey`].
pub(crate) fn encode_manifest_image(
    next_file_id: u64,
    last_seq: u64,
    min_wal_id: u64,
    range_deletes_checked: bool,
    files: impl IntoIterator<Item = (usize, SsTableMeta)>,
    keyring: Option<&Arc<Keyring>>,
) -> io::Result<Vec<u8>> {
    let sealing = VersionSet::fresh_sealing(keyring)?;
    Ok(encode_image(
        next_file_id,
        last_seq,
        min_wal_id,
        range_deletes_checked,
        files,
        sealing.as_ref(),
    )?
    .0)
}

/// [`encode_manifest_image`], sealed under `sealing` when given: its salt in
/// the stamp and the batch under the keyring's current key, which is handed
/// back.
fn encode_image(
    next_file_id: u64,
    last_seq: u64,
    min_wal_id: u64,
    range_deletes_checked: bool,
    files: impl IntoIterator<Item = (usize, SsTableMeta)>,
    sealing: Option<&ManifestSeal>,
) -> io::Result<(Vec<u8>, Option<KeyId>)> {
    let mut records = vec![
        ManifestRecord::SetNextFileId(next_file_id),
        ManifestRecord::SetLastSeq(last_seq),
        ManifestRecord::SetMinWalId(min_wal_id),
    ];
    if range_deletes_checked {
        records.push(ManifestRecord::RangeDeletesChecked);
    }
    for (level, meta) in files {
        validate_level_index(level)?;
        push_table(&mut records, level, meta);
    }
    let mut image = VersionSet::stamp_bytes(sealing.map(|s| &s.salt));
    let (batch, sealed_under) = VersionSet::encode_records(&records, image.len() as u64, sealing)?;
    image.extend_from_slice(&batch);
    Ok((image, sealed_under))
}

/// Identifier at the head of a MANIFEST: `REGOMAN` plus a format byte.
const MANIFEST_MAGIC: [u8; 7] = *b"REGOMAN";

/// On-disk MANIFEST format this build writes.
const MANIFEST_FORMAT_V1: u8 = 1;

/// Stamp layout: magic, format, checksum.
const MANIFEST_STAMP_LEN: usize = 12;

/// How far past its canonical size a MANIFEST may grow before it is
/// rewritten.
///
/// The log is append-only, so without this it grows for the life of the
/// database and recovery replays every edit ever made. Rewriting costs
/// one record per live SSTable, and only after the log has grown to a
/// multiple of that, so the amortized cost per edit is constant while
/// the file stays within a bounded factor of its minimum.
const MANIFEST_COMPACT_FACTOR: u64 = 4;

/// Floor for the rewrite trigger, so a small database does not rewrite
/// its manifest on almost every edit.
const MANIFEST_COMPACT_FLOOR: u64 = 64 * 1024;

/// Rough size of one `AddFile` record, used only to size the rewrite
/// trigger. Being approximate costs a slightly different trigger point,
/// never correctness: the rewrite is driven by the real file length.
const APPROX_ADD_FILE_RECORD_BYTES: u64 = 256;

/// Manages the current version and persists version edits to a manifest log.
pub(crate) struct VersionSet {
    // No lock of its own: `apply` takes `&mut self`, and every caller
    // reaches the set through the version store's guard.
    current: Arc<Version>,
    manifest_path: PathBuf,
    /// Bytes the manifest holds on disk, tracked rather than stat'ed so
    /// the rewrite check costs nothing on the common path.
    manifest_bytes: u64,
    /// Absent on read-only opens or after uncertain manifest I/O. Edits
    /// and rewrites are refused until recovery reopens the log.
    manifest_writer: Option<BufferedWriter>,
    env: Arc<dyn Env>,
    /// The damaged end the open dropped, for the engine to report.
    dropped_tail: Option<DroppedTail>,
    /// Set when the database is encrypted at rest. Tables are opened
    /// through it, and a manifest it finds unsealed is rewritten sealed.
    keyring: Option<Arc<Keyring>>,
    /// Set when the manifest on disk is sealed: every batch appended to it
    /// is sealed under the keyring's current key and bound to its salt.
    sealing: Option<ManifestSeal>,
    /// Every key a batch of the manifest on disk is sealed under, so a
    /// rotation can tell whether the file still names a retired key.
    sealed_under: Vec<KeyId>,
    /// What this open moved out of overlapping levels, until the engine
    /// takes it.
    demotion: Option<Demotion>,
    /// The edit that records that move, and the record that the range
    /// deletes were checked, not in the manifest yet: [`Self::apply`]
    /// writes them in front of its next batch.
    unlogged: Vec<ManifestRecord>,
    /// The manifest on disk records that the range deletes were checked
    /// ([`ManifestRecord::RangeDeletesChecked`]).
    range_deletes_checked: bool,
    /// The manifest was written without that record, so its tables may hold
    /// what the check copies: the batch that records it is synced, and an
    /// open that returned has recorded it.
    sync_check_record: bool,
}

struct ManifestReplay {
    version: Version,
    valid_len: usize,
    /// Bytes of the stamp replay found; `0` when the file is too short to
    /// hold one, which is a crash while the manifest was created.
    stamp_len: usize,
    /// The salt of a sealed manifest, `None` for an unsealed one.
    salt: Option<[u8; 16]>,
    /// Every key a replayed batch was sealed under.
    key_ids: Vec<KeyId>,
    repair: legacy_overlap::Repair,
    /// What `repair` moved, with the moved tables' range tombstones.
    demotion: Option<Demotion>,
    /// A replayed batch recorded that the range deletes were checked.
    range_deletes_checked: bool,
}

impl VersionSet {
    /// Create or recover a VersionSet from the given directory, with an
    /// explicit policy for how the readers it opens hold their index and
    /// filter blocks.
    ///
    /// Recovery is where this matters most: it opens a reader for every
    /// SSTable the manifest references, so the policy decides whether
    /// that whole set of indexes and filters is pinned or bounded by
    /// the block cache.
    ///
    /// With a `keyring`, a sealed manifest is read under it and a manifest
    /// found unsealed is rewritten sealed before this returns, once; without
    /// one, a sealed manifest refuses with
    /// [`crate::Error::KeyProviderRequired`].
    pub(crate) fn open_with_policy(
        env: &Arc<dyn Env>,
        db_dir: &Path,
        sst_dir: &Path,
        policy: MetadataPolicy,
        keyring: Option<Arc<Keyring>>,
    ) -> io::Result<Self> {
        let manifest_path = db_dir.join("MANIFEST");

        let mut demotion = None;
        let mut unlogged = Vec::new();
        let mut range_deletes_checked = false;
        let mut sync_check_record = false;
        let manifest_bytes;
        let mut dropped_tail = None;
        let salt;
        let mut sealed_under = Vec::new();
        let (version, writer) = if env.exists(&manifest_path) {
            let data = env.read(&manifest_path)?;
            let mut replay =
                Self::replay_manifest(env, &data, sst_dir, policy, keyring.as_deref())?;
            dropped_tail = Self::judge_end(
                &**env,
                &replay,
                &data,
                sst_dir,
                &manifest_path,
                keyring.as_deref(),
            )?;
            // A torn tail means the log lost records; keep every table
            // until a clean replay says which ones are unreferenced. And
            // only when the directory lock excludes other processes: where
            // it does not, another one could be flushing into one of these
            // ids right now.
            let replayed_cleanly = !data.is_empty() && replay.valid_len == data.len();
            let highest_table = if replayed_cleanly && env.capabilities().file_lock {
                super::orphan_sweep::sweep_unreferenced_tables(&**env, sst_dir, &replay.version)?
                    .highest_kept
            } else {
                super::orphan_sweep::highest_table_id(&**env, sst_dir)?
            };
            // Every table left in the directory keeps its id: a table or a
            // log that took it would be written over the file.
            replay.version.next_file_id = super::orphan_sweep::next_id_past_tables(
                replay.version.next_file_id,
                highest_table,
            );
            demotion = replay.demotion;
            unlogged = replay.repair.records;
            range_deletes_checked = replay.range_deletes_checked;
            // A stamp-less file is a creation a crash caught: it lists no
            // table, so it is a new manifest.
            sync_check_record = replay.stamp_len > 0 && !range_deletes_checked;

            // Trim through its own handle, and close it before the
            // append handle is opened. A torn or corrupt tail is the
            // ordinary shape of a crash, so this runs on a normal
            // reopen; shortening a file needs write access that an
            // append handle does not carry on every platform, which is
            // what [`WriteMode::Update`] exists for.
            if replay.valid_len < data.len() {
                let mut trim = env.open_write(&manifest_path, WriteMode::Update)?;
                trim.set_len(replay.valid_len as u64)?;
                trim.sync_all()?;
            }
            // A file too short to carry a stamp is one a crash caught
            // during creation, and the guard above has already cleared
            // it of hiding live tables. The append path never writes a
            // stamp, so it is written here instead: without it the next
            // open would find a stamp-less record stream and refuse.
            if replay.stamp_len == 0 {
                let (file, stamp_salt, len) =
                    Self::create_stamped(env, &manifest_path, keyring.is_some())?;
                manifest_bytes = len;
                salt = stamp_salt;
                (replay.version, file)
            } else {
                let file = env.open_write(&manifest_path, WriteMode::Append)?;
                manifest_bytes = replay.valid_len as u64;
                salt = replay.salt;
                sealed_under = replay.key_ids;
                (replay.version, BufferedWriter::new(file))
            }
        } else {
            let (file, stamp_salt, len) =
                Self::create_stamped(env, &manifest_path, keyring.is_some())?;
            manifest_bytes = len;
            salt = stamp_salt;
            let mut version = Version::new();
            version.next_file_id = super::orphan_sweep::next_id_past_tables(
                version.next_file_id,
                super::orphan_sweep::highest_table_id(&**env, sst_dir)?,
            );
            (version, file)
        };

        // A manifest that does not say the range deletes were checked gets
        // the record with the open's first edit, after the engine has run
        // the check and logged what it copied. A new one has nothing to
        // check, and records it the same way.
        if !range_deletes_checked {
            unlogged.push(ManifestRecord::RangeDeletesChecked);
        }
        let sealing = Self::sealing(keyring.as_ref(), salt)?;
        let convert = keyring.is_some() && sealing.is_none();
        let mut set = Self {
            current: Arc::new(version),
            manifest_path,
            manifest_bytes,
            manifest_writer: Some(writer),
            env: Arc::clone(env),
            dropped_tail,
            keyring,
            sealing,
            sealed_under,
            demotion,
            unlogged,
            range_deletes_checked,
            sync_check_record,
        };
        // The one-time conversion of a database written without encryption
        // and opened with a key provider: its manifest names every table
        // and key range in plaintext, so it is rewritten sealed now rather
        // than whenever it next grows past its rewrite threshold. An open
        // with a move or a check to record rewrites it with the batch that
        // records them (`apply`): the rewrite would record the move now,
        // before the engine has made the range tombstones it copies durable.
        if convert && set.unlogged.is_empty() {
            set.compact_manifest()?;
        }
        Ok(set)
    }

    /// Create a manifest at `path` holding only its stamp, sealed with a
    /// fresh salt when `sealed`, and make it durable. Returns the append
    /// writer, the salt and the stamp's length.
    fn create_stamped(
        env: &Arc<dyn Env>,
        path: &Path,
        sealed: bool,
    ) -> io::Result<(BufferedWriter, Option<[u8; 16]>, u64)> {
        let salt = sealed.then(sealed::fresh_salt).transpose()?;
        let stamp = Self::stamp_bytes(salt.as_ref());
        let mut file = env.open_write(path, WriteMode::Truncate)?;
        file.write_all(&stamp)?;
        file.sync_all()?;
        crate::env::sync_parent_dir(&**env, path)?;
        Ok((BufferedWriter::new(file), salt, stamp.len() as u64))
    }

    /// The stamp a manifest begins with: sealed under `salt` when there is
    /// one, else the format 1 stamp.
    fn stamp_bytes(salt: Option<&[u8; 16]>) -> Vec<u8> {
        match salt {
            Some(salt) => sealed::encode_stamp(&MANIFEST_MAGIC, salt).to_vec(),
            None => Self::encode_stamp().to_vec(),
        }
    }

    /// How batches are sealed for a manifest whose salt is `salt`. A sealed
    /// manifest without a keyring cannot happen: its stamp refused the open.
    fn sealing(
        keyring: Option<&Arc<Keyring>>,
        salt: Option<[u8; 16]>,
    ) -> io::Result<Option<ManifestSeal>> {
        match (keyring, salt) {
            (Some(keyring), Some(salt)) => Ok(Some(ManifestSeal {
                keyring: Arc::clone(keyring),
                salt,
            })),
            (None, Some(_)) => Err(crate::Error::KeyProviderRequired.into_io_error()),
            (_, None) => Ok(None),
        }
    }

    /// How a manifest written whole is sealed: under a fresh salt when there
    /// is a keyring, not at all when there is none. The rewrite and a
    /// backup restore both seal through this.
    fn fresh_sealing(keyring: Option<&Arc<Keyring>>) -> io::Result<Option<ManifestSeal>> {
        let salt = keyring.is_some().then(sealed::fresh_salt).transpose()?;
        Self::sealing(keyring, salt)
    }

    /// Create or recover a VersionSet through the standard
    /// environment.
    #[cfg(any(test, feature = "fuzzing"))]
    pub(crate) fn open(db_dir: &Path, sst_dir: &Path) -> io::Result<Self> {
        Self::open_with_policy(
            &crate::env::std_env(),
            db_dir,
            sst_dir,
            MetadataPolicy::Pinned,
            None,
        )
    }

    /// Recover an existing VersionSet without mutating the manifest.
    ///
    /// This is used by read-only opens: replay still tolerates a
    /// truncated trailing record exactly like the read-write path, but
    /// the file is not repaired in place and no append writer is kept.
    pub(crate) fn open_read_only(
        env: &Arc<dyn Env>,
        db_dir: &Path,
        sst_dir: &Path,
        policy: MetadataPolicy,
        keyring: Option<Arc<Keyring>>,
    ) -> io::Result<Self> {
        let manifest_path = db_dir.join("MANIFEST");
        let data = env.read(&manifest_path)?;
        let replay = Self::replay_manifest(env, &data, sst_dir, policy, keyring.as_deref())?;
        let dropped_tail = Self::judge_end(
            &**env,
            &replay,
            &data,
            sst_dir,
            &manifest_path,
            keyring.as_deref(),
        )?;
        let sealing = Self::sealing(keyring.as_ref(), replay.salt)?;

        Ok(Self {
            current: Arc::new(replay.version),
            manifest_path,
            manifest_bytes: replay.valid_len as u64,
            manifest_writer: None,
            env: Arc::clone(env),
            dropped_tail,
            keyring,
            sealing,
            sealed_under: replay.key_ids,
            // Read-only: the view is repaired, the manifest never is.
            demotion: replay.demotion,
            unlogged: Vec::new(),
            range_deletes_checked: replay.range_deletes_checked,
            sync_check_record: false,
        })
    }

    /// Whether, on an encrypted database, the manifest on disk is unsealed
    /// or holds a batch sealed under a key other than the current one, so
    /// a rewrite is what retires that key.
    pub(crate) fn has_stale_seals(&self) -> bool {
        let Some(keyring) = &self.keyring else {
            return false;
        };
        let current = keyring.current_id();
        self.sealing.is_none() || self.sealed_under.iter().any(|&id| id != current)
    }

    /// Get the current version.
    pub(crate) fn current(&self) -> Arc<Version> {
        Arc::clone(&self.current)
    }

    /// What this open moved out of overlapping levels, handed over once:
    /// `None` when it moved nothing, which is every open after the move is
    /// in the manifest. The caller owes the moved tables' range tombstones
    /// a place in front of every key they cover before its first
    /// [`Self::apply`], which is what records the move.
    pub(crate) fn take_demotion(&mut self) -> Option<Demotion> {
        self.demotion.take()
    }

    /// Whether the manifest records that the range deletes of its tables
    /// were checked for ones an earlier release left behind older keys they
    /// cover. `false` on the first open of a database such a release wrote,
    /// and on the first open of a new one: the caller then owes the check,
    /// and copies of what it finds in front of every key they cover, before
    /// its first [`Self::apply`], which records it. A read-only open owes
    /// the check every time.
    pub(crate) fn range_deletes_checked(&self) -> bool {
        self.range_deletes_checked
    }

    /// Path of the manifest file on disk.
    pub(crate) fn manifest_path(&self) -> &Path {
        &self.manifest_path
    }

    /// The damaged end of the manifest this open dropped as a crash's
    /// unsynced tail, if there was one. A read-write open has already
    /// truncated it; a read-only open left it in the file.
    pub(crate) fn dropped_tail(&self) -> Option<DroppedTail> {
        self.dropped_tail
    }

    fn writer_unavailable(&self) -> io::Error {
        io::Error::other(format!(
            "{} is not writable; reopen the database before updating the MANIFEST",
            self.manifest_path.display(),
        ))
    }

    /// Apply a batch of edits atomically: update the in-memory version
    /// and persist the serialized records to the manifest log.
    pub(crate) fn apply(&mut self, edits: &[VersionEdit]) -> io::Result<()> {
        for edit in edits {
            match edit {
                VersionEdit::AddFile { level, .. }
                | VersionEdit::RemoveFile { level, .. }
                | VersionEdit::CountFile { level, .. } => {
                    validate_level_index(*level)?;
                }
                VersionEdit::SetLastSeq(_)
                | VersionEdit::SetNextFileId(_)
                | VersionEdit::SetMinWalId(_)
                | VersionEdit::Reset { .. } => {}
            }
        }

        let mut version = (*self.current()).clone();

        for edit in edits {
            match edit {
                VersionEdit::AddFile { level, file } => {
                    version.add_file(*level, Arc::clone(file));
                }
                VersionEdit::RemoveFile { level, file_id } => {
                    version.remove_file(*level, *file_id);
                }
                VersionEdit::CountFile {
                    level,
                    file_id,
                    deletions,
                } => {
                    version.count_file(*level, *file_id, *deletions);
                }
                VersionEdit::SetLastSeq(seq) => {
                    version.last_seq = raised_last_seq(version.last_seq, *seq);
                }
                VersionEdit::SetNextFileId(id) => {
                    version.next_file_id = *id;
                }
                VersionEdit::SetMinWalId(id) => {
                    version.min_wal_id = *id;
                }
                VersionEdit::Reset {
                    next_file_id,
                    min_wal_id,
                } => {
                    version.levels = (0..MAX_LEVELS).map(|_| Vec::new()).collect();
                    version.uncounted = 0;
                    version.last_seq = 0;
                    version.next_file_id = *next_file_id;
                    version.min_wal_id = *min_wal_id;
                }
            }
        }

        #[cfg(debug_assertions)]
        for edit in edits {
            if let VersionEdit::AddFile { file, .. } = edit {
                debug_assert!(
                    file.sequence_ceiling <= version.last_seq,
                    "a table sequence exceeds its published version ceiling"
                );
            }
        }

        // Checked once the whole batch is in: a compaction removes its inputs
        // and adds its outputs in one batch, so the level is only expected to
        // be sound at the end of it.
        debug_assert!(
            version.levels_are_sorted_runs(),
            "a level below L0 holds tables that overlap beyond a shared boundary key"
        );

        // The move an open made of overlapping levels goes in the same
        // batch, in front: the log then replays to the version the edits
        // were applied to, and a crash keeps both or neither.
        let mut records = std::mem::take(&mut self.unlogged);
        let moved = records.len();
        let recorded_move = moved > 0;
        let records_check = records
            .iter()
            .any(|record| matches!(record, ManifestRecord::RangeDeletesChecked));
        edits.iter().for_each(|edit| edit.to_records(&mut records));
        let (encoded, sealed_under) =
            match Self::encode_records(&records, self.manifest_bytes, self.sealing.as_ref()) {
                Ok(encoded) => encoded,
                Err(e) => {
                    // Nothing was written, so the move is still owed to the
                    // next batch. Past this point an error drops the writer,
                    // and only a reopen may decide what the file holds.
                    records.truncate(moved);
                    self.unlogged = records;
                    return Err(e);
                }
            };
        let requires_sync = records.iter().any(ManifestRecord::requires_sync)
            || (records_check && self.sync_check_record);
        // An append or sync error leaves an uncertain tail. Only reopen
        // may decide which complete frames survived; neither a later
        // append nor a rewrite from the old version may bypass that tail.
        let mut writer = self
            .manifest_writer
            .take()
            .ok_or_else(|| self.writer_unavailable())?;
        writer.write_all(&encoded)?;
        if requires_sync {
            writer.sync_all()?;
        } else {
            writer.flush()?;
        }
        self.manifest_writer = Some(writer);

        let live_files: u64 = version.levels.iter().map(|l| l.len() as u64).sum();
        self.current = Arc::new(version);
        self.manifest_bytes += encoded.len() as u64;
        self.range_deletes_checked |= records_check;
        if let Some(id) = sealed_under
            && !self.sealed_under.contains(&id)
        {
            self.sealed_under.push(id);
        }

        // The log is append-only, so a database that runs for years
        // replays every edit it ever made unless the file is rewritten.
        // Rewriting costs one record per live table and happens only once
        // the log has grown to a multiple of that, so the cost per edit
        // is constant and the file stays within a bounded factor of its
        // smallest possible size. The conversion the open left to this
        // batch (see `open_with_policy`) is the same rewrite.
        let convert = recorded_move && self.keyring.is_some() && self.sealing.is_none();
        if convert || self.manifest_bytes > Self::compact_threshold(live_files) {
            self.compact_manifest()?;
        }

        Ok(())
    }

    /// Size at which the manifest is rewritten, from the number of live
    /// SSTables it has to name.
    fn compact_threshold(live_files: u64) -> u64 {
        let canonical = MANIFEST_STAMP_LEN as u64 + live_files * APPROX_ADD_FILE_RECORD_BYTES;
        MANIFEST_COMPACT_FLOOR.max(canonical.saturating_mul(MANIFEST_COMPACT_FACTOR))
    }

    /// Rewrite the manifest from scratch, emitting the current version as
    /// a single compact sequence of records. Readers in the live
    /// `Version` are preserved - we never close their file descriptors.
    ///
    /// With a keyring the new manifest is sealed, under a fresh salt and the
    /// current key, whatever the old one was: this is how a manifest is
    /// sealed for the first time and how a rotation reaches every batch.
    pub(crate) fn compact_manifest(&mut self) -> io::Result<()> {
        if self.manifest_writer.is_none() {
            return Err(self.writer_unavailable());
        }
        let sealing = Self::fresh_sealing(self.keyring.as_ref())?;
        let version = self.current();
        let (image, sealed_under) = encode_image(
            version.next_file_id,
            version.last_seq,
            version.min_wal_id,
            self.range_deletes_checked,
            version
                .levels
                .iter()
                .enumerate()
                .flat_map(|(level, files)| {
                    files.iter().map(move |file| (level, file.meta.clone()))
                }),
            sealing.as_ref(),
        )?;

        let tmp_path = self.manifest_path.with_extension("tmp");
        {
            let mut file = self.env.open_write(&tmp_path, WriteMode::Truncate)?;
            file.write_all(&image)?;
            file.sync_all()?;
        }
        // Close the log before replacing it. Windows refuses to replace
        // a file that still has an open handle, so a rewrite performed
        // while this writer was live failed with "Access is denied" and
        // took every manifest rewrite on that platform with it. On unix
        // the rename would have worked either way: the old inode simply
        // outlives its name. Dropping the writer first is correct on
        // both, and the reopen below is where the new log is picked up.
        self.manifest_writer = None;
        self.env.rename(&tmp_path, &self.manifest_path)?;
        crate::env::sync_parent_dir(&*self.env, &self.manifest_path)?;
        self.manifest_bytes = image.len() as u64;
        self.sealing = sealing;
        self.sealed_under = sealed_under.into_iter().collect();
        // The image is the current version, a move included. A check
        // not recorded yet is not in it: the next open runs it again.
        self.unlogged.clear();

        let file = self
            .env
            .open_write(&self.manifest_path, WriteMode::Append)?;
        self.manifest_writer = Some(BufferedWriter::new(file));

        Ok(())
    }

    /// Encode the stamp a manifest begins with.
    pub(crate) fn encode_stamp() -> [u8; MANIFEST_STAMP_LEN] {
        let mut out = [0u8; MANIFEST_STAMP_LEN];
        out[0..7].copy_from_slice(&MANIFEST_MAGIC);
        out[7] = MANIFEST_FORMAT_V1;
        let checksum = checksum::manifest_record(0, &out[0..8]);
        out[8..12].copy_from_slice(&checksum.to_le_bytes());
        out
    }

    /// Length of the stamp at the head of `data`.
    ///
    /// The stamp is mandatory: it is written when the manifest is
    /// created, before any record, so a file too short to hold one is a
    /// crash during creation rather than the loss of an acknowledged
    /// edit. Nothing is consumed from it, so replay ends short of the
    /// file length and the open guard still weighs the tables on disk
    /// rather than treating the replay as clean. A file long enough to
    /// carry a stamp but not carrying one is refused.
    ///
    /// A sealed stamp also hands back the manifest's salt. Without a
    /// `keyring` it refuses with [`crate::Error::KeyProviderRequired`]: the
    /// batches after it cannot be read, and must not read as none.
    fn stamp_len(data: &[u8], keyring: Option<&Keyring>) -> io::Result<(usize, Option<[u8; 16]>)> {
        if data.len() < MANIFEST_STAMP_LEN {
            return Ok((0, None));
        }
        if data[0..7] != MANIFEST_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "MANIFEST does not begin with the REGOMAN stamp",
            ));
        }
        let format = data[7];
        if format == MANIFEST_FORMAT_SEALED {
            let Some(salt) = sealed::decode_stamp(data)? else {
                return Ok((0, None));
            };
            if keyring.is_none() {
                return Err(crate::Error::KeyProviderRequired.into_io_error());
            }
            return Ok((SEALED_STAMP_LEN, Some(salt)));
        }
        let stored = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        if stored != checksum::manifest_record(0, &data[0..8]) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "MANIFEST stamp checksum mismatch",
            ));
        }
        if format > MANIFEST_FORMAT_SEALED {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "MANIFEST format {format} was written by a newer regolith than this build, \
                     which understands up to {MANIFEST_FORMAT_SEALED}"
                ),
            ));
        }
        Ok((MANIFEST_STAMP_LEN, None))
    }

    /// One batch holding `records`, whose `len` field lands at `offset` of
    /// the manifest: sealed under the current key when `sealing` is set, in
    /// which case that key is handed back too.
    fn encode_records(
        records: &[ManifestRecord],
        offset: u64,
        sealing: Option<&ManifestSeal>,
    ) -> io::Result<(Vec<u8>, Option<KeyId>)> {
        if records.is_empty() {
            return Ok((Vec::new(), None));
        }
        if let Some(sealing) = sealing {
            let mut plain = Vec::new();
            for record in records {
                record.encode(&mut plain);
            }
            let sealer = sealing.keyring.current()?;
            let mut buf = Vec::with_capacity(plain.len() + 64);
            sealed::encode_batch(&sealer, &sealing.salt, offset, &plain, &mut buf)?;
            return Ok((buf, Some(sealer.id())));
        }
        // V1 readers already decode every edit inside a frame. Keep that
        // format, but checksum the whole apply so a torn compaction drops
        // both its removals and additions instead of just the additions.
        let mut buf = vec![0; 4];
        for record in records {
            record.encode(&mut buf);
        }
        let len = u32::try_from(buf.len() - 4).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "MANIFEST edit batch exceeds u32 length",
            )
        })?;
        buf[..4].copy_from_slice(&len.to_le_bytes());
        let checksum = checksum::manifest_record(len, &buf[4..]);
        buf.extend_from_slice(&checksum.to_le_bytes());
        Ok((buf, None))
    }

    /// What the open makes of the end of the manifest, when replay
    /// stopped short of it.
    ///
    /// With its stamp in place the end is judged as a write-ahead log's is
    /// (`tail.rs`): a crash's unsynced tail is dropped and handed back for
    /// the engine to report, and damage that a later batch proves was
    /// synced refuses the open, naming the file and both offsets.
    ///
    /// Without its stamp the file is one a crash caught while the database
    /// was created, because the stamp is synced before any table exists. A
    /// table that may hold data beside it is then no crash's doing, so the
    /// open refuses, naming the tables, rather than serve an empty database
    /// over them. A table that provably holds nothing is passed over.
    fn judge_end(
        env: &dyn Env,
        replay: &ManifestReplay,
        data: &[u8],
        sst_dir: &Path,
        manifest_path: &Path,
        keyring: Option<&Keyring>,
    ) -> io::Result<Option<DroppedTail>> {
        if replay.stamp_len == 0 {
            let suspects = suspects::suspect_tables(env, sst_dir, keyring);
            if suspects.count() > 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "{} is damaged at offset 0: it has no header, yet {} table file(s) in {} \
                         may hold data ({}); the database is left untouched",
                        manifest_path.display(),
                        suspects.count(),
                        sst_dir.display(),
                        suspects.describe(),
                    ),
                ));
            }
            return Ok((!data.is_empty()).then_some(DroppedTail {
                offset: 0,
                bytes: data.len() as u64,
            }));
        }
        if replay.valid_len == data.len() {
            return Ok(None);
        }
        let sealed = keyring
            .zip(replay.salt.as_ref())
            .map(|(keyring, salt)| tail::Opener { keyring, salt });
        tail::judge(data, replay.valid_len, manifest_path, sealed).map(Some)
    }

    /// Replay every record, then open a reader for each surviving file
    /// through `env` under `policy`.
    ///
    /// In a sealed manifest a batch is checked twice: its checksum first,
    /// which a torn batch fails and which ends replay as in format 1, then
    /// its tag under the key it names, which only a wrong key or tampering
    /// fails, and which refuses the open. A batch naming a key `keyring`
    /// does not provide refuses with [`crate::Error::UnknownKey`].
    fn replay_manifest(
        env: &Arc<dyn Env>,
        data: &[u8],
        sst_dir: &Path,
        policy: MetadataPolicy,
        keyring: Option<&Keyring>,
    ) -> io::Result<ManifestReplay> {
        // Two-pass replay. The first pass walks every record and tracks
        // the *logical* state of each level - which file ids are live -
        // without touching the filesystem. Only after replay completes
        // do we open readers for the surviving files.
        //
        // This matters for compaction-heavy histories: when compaction
        // adds an L1 file and removes the L0 inputs, both records land
        // in the manifest, but the inputs' physical files are unlinked
        // from disk by `delete_old_files`. An eager open at AddFile
        // time would fail on the unlinked files even though a later
        // RemoveFile record cancels them out.
        let mut surviving: Vec<Vec<SsTableMeta>> = vec![Vec::new(); MAX_LEVELS];
        // The counts each listed table was recorded with, by file id. A
        // table's own record carries none, so they are joined to the
        // survivors at the end, in one lookup each.
        let mut counts: std::collections::HashMap<u64, Deletions> =
            std::collections::HashMap::new();
        let mut last_seq: u64 = 0;
        let mut next_file_id: u64 = 1;
        let mut min_wal_id: u64 = 0;
        let mut range_deletes_checked = false;
        // The stamp is not a record. `valid_len` starts past it so a
        // clean replay ends exactly at the file length, which is what
        // `judge_end` compares against.
        let (stamp, salt) = Self::stamp_len(data, keyring)?;
        let mut offset = stamp;
        let mut valid_len = stamp;
        let mut key_ids = Vec::new();

        while offset < data.len() {
            // The first batch that does not read back whole ends replay;
            // `judge_end` decides what the bytes from there on are.
            let Some(batch) = tail::batch_at(data, offset) else {
                break;
            };
            // A sealed batch's edits, decrypted into a buffer of their own.
            let opened;
            let record_data = match (&salt, keyring) {
                (Some(salt), Some(keyring)) => {
                    let (id, edits) =
                        sealed::open_batch(keyring, salt, offset as u64, batch.records)?;
                    if !key_ids.contains(&id) {
                        key_ids.push(id);
                    }
                    opened = edits;
                    opened.as_slice()
                }
                _ => batch.records,
            };
            offset = batch.end;

            let mut pos = 0;
            while let Some(record) = ManifestRecord::decode(record_data, &mut pos)? {
                match record {
                    ManifestRecord::AddFile { level, meta } => {
                        surviving[level].push(meta);
                    }
                    ManifestRecord::RemoveFile { level, file_id } => {
                        surviving[level].retain(|m| m.file_id != file_id);
                        counts.remove(&file_id);
                    }
                    ManifestRecord::TableCounts { file_id, deletions } => {
                        counts.insert(file_id, deletions);
                    }
                    ManifestRecord::SetLastSeq(seq) => {
                        last_seq = raised_last_seq(last_seq, seq);
                    }
                    ManifestRecord::SetNextFileId(id) => {
                        next_file_id = id;
                    }
                    ManifestRecord::SetMinWalId(id) => {
                        min_wal_id = id;
                    }
                    ManifestRecord::Reset {
                        next_file_id: reset_next_file_id,
                        min_wal_id: reset_min_wal_id,
                    } => {
                        for level in &mut surviving {
                            level.clear();
                        }
                        counts.clear();
                        last_seq = 0;
                        next_file_id = reset_next_file_id;
                        min_wal_id = reset_min_wal_id;
                    }
                    ManifestRecord::RangeDeletesChecked => range_deletes_checked = true,
                }
            }
            valid_len = offset;
        }

        let repair = legacy_overlap::Repair::prepare(&mut surviving)?;

        // Below L0 a level is one sorted run. The log lists a level's tables in
        // the order they arrived, which is how `Version::add_file` placed
        // them, so a stable sort puts each back where it was.
        for files in surviving.iter_mut().skip(1) {
            files.sort_by(|a, b| a.smallest_key.cmp(&b.smallest_key));
        }

        // Second pass: open readers for the survivors.
        let mut version = Version::new();
        version.last_seq = last_seq;
        version.next_file_id = next_file_id;
        version.min_wal_id = min_wal_id;
        for (level, files) in surviving.into_iter().enumerate() {
            for mut meta in files {
                // Counts naming a table no longer listed were dropped with
                // it, or name nothing: either way they are not looked up.
                // Counts that cannot be this table's are not believed.
                meta.deletions = counts
                    .remove(&meta.file_id)
                    .filter(|deletions| deletions.fits(meta.num_entries));
                version.uncounted += usize::from(meta.deletions.is_none());
                let path = sst_dir.join(sst_filename(meta.file_id));
                let reader = Arc::new(
                    SsTableReader::open_with(env, &path, meta.file_id, policy, keyring)
                        .map_err(|e| {
                            if crate::Error::is_typed(&e) {
                                return e;
                            }
                            std::io::Error::new(e.kind(), format!("open {}: {e}", path.display()))
                        })?
                        .with_global_seq(meta.global_seq),
                );
                version.levels[level].push(LiveSst::new(meta, reader, last_seq));
            }
        }
        // Every level left below L0 is one sorted run now: the repair above
        // moved the ones that were not. A read finds a key's tables in such
        // a level by binary search and in no other, and the log is untrusted
        // input, so this is checked rather than taken from the repair.
        if let Some((level, left, right)) = version.find_overlap() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "manifest lists tables {} and {} at level {level} with overlapping key ranges",
                    left.file_id, right.file_id
                ),
            ));
        }

        Ok(ManifestReplay {
            valid_len,
            stamp_len: stamp,
            salt,
            key_ids,
            demotion: repair.demotion(&version.levels[0]),
            repair,
            version,
            range_deletes_checked,
        })
    }
}

/// Test-only: rewrite the manifest of the closed, unencrypted database at
/// `db_dir` as a build before the deletion counts would have written it:
/// every batch as it is, without its tag 8 records. The tables then read
/// back with their counts unknown.
#[cfg(test)]
pub(crate) fn forget_counts(db_dir: &Path) {
    let path = db_dir.join("MANIFEST");
    let data = std::fs::read(&path).unwrap();
    let mut out = data[..MANIFEST_STAMP_LEN].to_vec();
    let mut offset = MANIFEST_STAMP_LEN;
    while let Some(batch) = tail::batch_at(&data, offset) {
        let mut records = Vec::new();
        let mut pos = 0;
        while let Some(record) = ManifestRecord::decode(batch.records, &mut pos).unwrap() {
            if !matches!(record, ManifestRecord::TableCounts { .. }) {
                records.push(record);
            }
        }
        let (encoded, _) = VersionSet::encode_records(&records, out.len() as u64, None).unwrap();
        out.extend_from_slice(&encoded);
        offset = batch.end;
    }
    assert_eq!(offset, data.len(), "the manifest replays whole");
    std::fs::write(&path, out).unwrap();
}

#[cfg(test)]
mod atomic_tests;
#[cfg(test)]
mod buried_delete_tests;
#[cfg(test)]
mod counts_tests;
#[cfg(test)]
mod demotion_tests;
#[cfg(test)]
mod legacy_overlap_fault_tests;
#[cfg(test)]
mod legacy_overlap_tests;
#[cfg(test)]
mod legacy_test_support;
#[cfg(test)]
mod level_order_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use tempfile::TempDir;

    /// Build a real on-disk SSTable and open a reader for it. Used by
    /// tests that need a non-trivial `LiveSst` instance.
    pub(super) fn make_live_sst(
        dir: &Path,
        file_id: u64,
        smallest: &[u8],
        largest: &[u8],
    ) -> Arc<LiveSst> {
        make_live_sst_at_seq(dir, file_id, smallest, largest, 0)
    }

    fn make_live_sst_at_seq(
        dir: &Path,
        file_id: u64,
        smallest: &[u8],
        largest: &[u8],
        seq: u64,
    ) -> Arc<LiveSst> {
        use super::super::internal_key::{VALUE_TYPE_VALUE, encode_internal_key};
        use super::super::sstable::SsTableWriter;
        use crate::options::CompressionType;

        let path = dir.join(sst_filename(file_id));
        let mut writer =
            SsTableWriter::new(&path, 4096, 10, CompressionType::None, None, false, 4096).unwrap();
        writer
            .add(
                &encode_internal_key(smallest, seq, VALUE_TYPE_VALUE),
                b"value",
            )
            .unwrap();
        if smallest != largest {
            writer
                .add(
                    &encode_internal_key(largest, seq, VALUE_TYPE_VALUE),
                    b"value",
                )
                .unwrap();
        }
        let summary = writer.finish().unwrap().unwrap();
        let file_size = std::fs::metadata(&path).unwrap().len();
        let reader = Arc::new(SsTableReader::open(&path, file_id).unwrap());
        LiveSst::new(
            SsTableMeta {
                file_id,
                smallest_key: summary.smallest_user_key,
                largest_key: summary.largest_user_key,
                file_size,
                num_entries: summary.num_entries,
                global_seq: None,
                deletions: None,
            },
            reader,
            summary.newest_seq,
        )
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "a table sequence exceeds its published version ceiling")]
    fn a_new_table_requires_its_sequence_ceiling() {
        let dir = TempDir::new().unwrap();
        let sst_dir = dir.path().join("sst");
        std::fs::create_dir_all(&sst_dir).unwrap();
        let file = make_live_sst_at_seq(&sst_dir, 1, b"a", b"z", 2);
        let mut vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
        vs.apply(&[VersionEdit::AddFile { level: 0, file }])
            .unwrap();
    }

    #[test]
    fn a_new_table_and_its_sequence_ceiling_are_one_batch() {
        let dir = TempDir::new().unwrap();
        let sst_dir = dir.path().join("sst");
        std::fs::create_dir_all(&sst_dir).unwrap();
        let file = make_live_sst_at_seq(&sst_dir, 1, b"a", b"z", 2);
        let mut vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
        vs.apply(&[
            VersionEdit::AddFile { level: 0, file },
            VersionEdit::SetLastSeq(2),
        ])
        .unwrap();
        assert_eq!(vs.current().last_seq, 2);
    }

    fn second_record_checksum_offset(path: &Path) -> usize {
        let data = std::fs::read(path).unwrap();
        // Records begin after the stamp.
        let base = MANIFEST_STAMP_LEN;
        let first_len = u32::from_le_bytes(data[base..base + 4].try_into().unwrap()) as usize;
        let second_start = base + 4 + first_len + 4;
        let second_len =
            u32::from_le_bytes(data[second_start..second_start + 4].try_into().unwrap()) as usize;
        second_start + 4 + second_len
    }

    fn test_meta(file_id: u64) -> SsTableMeta {
        SsTableMeta {
            file_id,
            smallest_key: b"a".to_vec(),
            largest_key: b"z".to_vec(),
            file_size: 128,
            num_entries: 2,
            global_seq: None,
            deletions: None,
        }
    }

    #[test]
    fn manifest_checksum_covers_length_header() {
        let mut record = Vec::new();
        ManifestRecord::SetLastSeq(7).encode(&mut record);
        let len = record.len() as u32;
        let baseline = checksum::manifest_record(len, &record);
        assert_ne!(baseline, checksum::manifest_record(len + 1, &record));
    }

    #[test]
    fn test_apply_and_replay_roundtrip() {
        let dir = TempDir::new().unwrap();
        let sst_dir = dir.path().join("sst");
        std::fs::create_dir_all(&sst_dir).unwrap();

        let file1 = make_live_sst(&sst_dir, 1, b"aaa", b"zzz");

        let edits = vec![
            VersionEdit::AddFile {
                level: 0,
                file: Arc::clone(&file1),
            },
            VersionEdit::SetLastSeq(42),
            VersionEdit::SetNextFileId(10),
        ];

        {
            let mut vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
            vs.apply(&edits).unwrap();
            let v = vs.current();
            assert_eq!(v.levels[0].len(), 1);
            assert_eq!(v.levels[0][0].meta.file_id, 1);
            assert_eq!(v.last_seq, 42);
            assert_eq!(v.next_file_id, 10);
            assert_eq!(v.min_wal_id, 0);
        }

        // Recover by replaying the manifest; readers are reopened.
        let vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
        let v = vs.current();
        assert_eq!(v.levels[0].len(), 1);
        assert_eq!(v.levels[0][0].meta.file_id, 1);
        assert_eq!(v.last_seq, 42);
        assert_eq!(v.next_file_id, 10);
        assert_eq!(v.min_wal_id, 0);
    }

    #[test]
    fn test_remove_file_hides_it_from_new_version() {
        let dir = TempDir::new().unwrap();
        let sst_dir = dir.path().join("sst");
        std::fs::create_dir_all(&sst_dir).unwrap();

        let file1 = make_live_sst(&sst_dir, 1, b"a", b"c");
        let file2 = make_live_sst(&sst_dir, 2, b"d", b"f");

        let mut vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
        vs.apply(&[
            VersionEdit::AddFile {
                level: 0,
                file: Arc::clone(&file1),
            },
            VersionEdit::AddFile {
                level: 0,
                file: Arc::clone(&file2),
            },
        ])
        .unwrap();

        // Holding a snapshot of the version *before* removal keeps both
        // files alive - this is the invariant that lets get/iter reads
        // survive concurrent compaction.
        let pinned = vs.current();
        assert_eq!(pinned.levels[0].len(), 2);

        vs.apply(&[VersionEdit::RemoveFile {
            level: 0,
            file_id: 1,
        }])
        .unwrap();

        let v = vs.current();
        assert_eq!(v.levels[0].len(), 1);
        assert_eq!(v.levels[0][0].meta.file_id, 2);
        // Pinned snapshot still sees both files.
        assert_eq!(pinned.levels[0].len(), 2);
    }

    #[test]
    fn initial_version_has_empty_levels_and_defaults() {
        let v = Version::new();
        assert_eq!(v.levels.len(), MAX_LEVELS);
        assert!(v.levels.iter().all(|l| l.is_empty()));
        assert_eq!(v.next_file_id, 1);
        assert_eq!(v.last_seq, 0);
        assert_eq!(v.min_wal_id, 0);
        assert_eq!(v.l0_count(), 0);
        for level in 0..MAX_LEVELS {
            assert_eq!(v.level_size(level), 0);
        }
    }

    #[test]
    fn manifest_path_returned_as_written() {
        let dir = TempDir::new().unwrap();
        let sst_dir = dir.path().join("sst");
        std::fs::create_dir_all(&sst_dir).unwrap();
        let vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
        assert_eq!(vs.manifest_path(), dir.path().join("MANIFEST"));
    }

    #[test]
    fn manifest_record_encode_decode_round_trip() {
        // Exercise every tag through the encode/decode path used by
        // the replay loop.
        let records = [
            ManifestRecord::AddFile {
                level: 2,
                meta: SsTableMeta {
                    file_id: 99,
                    smallest_key: b"aaa".to_vec(),
                    largest_key: b"zzz".to_vec(),
                    file_size: 4096,
                    num_entries: 128,
                    global_seq: None,
                    deletions: None,
                },
            },
            ManifestRecord::AddFile {
                level: 3,
                meta: SsTableMeta {
                    file_id: 100,
                    smallest_key: b"b".to_vec(),
                    largest_key: b"y".to_vec(),
                    file_size: 512,
                    num_entries: 3,
                    global_seq: Some(77),
                    deletions: None,
                },
            },
            ManifestRecord::RemoveFile {
                level: 1,
                file_id: 7,
            },
            ManifestRecord::SetLastSeq(999),
            ManifestRecord::SetNextFileId(42),
            ManifestRecord::SetMinWalId(11),
            ManifestRecord::Reset {
                next_file_id: 77,
                min_wal_id: 76,
            },
        ];
        for r in &records {
            let mut buf = Vec::new();
            r.encode(&mut buf);
            let mut pos = 0;
            let decoded = match ManifestRecord::decode(&buf, &mut pos) {
                Ok(Some(d)) => d,
                other => panic!("expected decoded record, got {:?}", other.is_ok()),
            };
            // Re-encode and compare - equality via round-trip avoids
            // having to add PartialEq to ManifestRecord.
            let mut rebuf = Vec::new();
            decoded.encode(&mut rebuf);
            assert_eq!(buf, rebuf);
            assert_eq!(pos, buf.len());
        }
    }

    #[test]
    fn an_ingested_tables_record_carries_its_sequence_and_an_older_record_reads_without_one() {
        let decoded = |bytes: &[u8]| {
            let mut pos = 0;
            match ManifestRecord::decode(bytes, &mut pos) {
                Ok(Some(ManifestRecord::AddFile { meta, .. })) => meta.global_seq,
                Ok(_) => panic!("decoded something other than an AddFile"),
                Err(e) => panic!("decode failed: {e}"),
            }
        };
        let mut ingested = Vec::new();
        ManifestRecord::AddFile {
            level: 5,
            meta: SsTableMeta {
                global_seq: Some(77),
                ..test_meta(3)
            },
        }
        .encode(&mut ingested);
        assert_eq!(ingested[0], TAG_ADD_INGESTED_FILE);
        assert_eq!(decoded(&ingested), Some(77));

        // A table regolith wrote keeps the record every earlier manifest
        // holds, byte for byte, and reads back without a sequence.
        let mut written = Vec::new();
        ManifestRecord::AddFile {
            level: 5,
            meta: test_meta(3),
        }
        .encode(&mut written);
        assert_eq!(written[0], TAG_ADD_FILE);
        assert_eq!(written.len() + 8, ingested.len());
        assert_eq!(written[1..], ingested[1..written.len()]);
        assert_eq!(decoded(&written), None);

        // Sequences start at 1, so a recorded 0 is damage.
        let len = ingested.len();
        ingested[len - 8..].copy_from_slice(&0u64.to_le_bytes());
        let mut pos = 0;
        let err = match ManifestRecord::decode(&ingested, &mut pos) {
            Err(e) => e,
            Ok(_) => panic!("a record at sequence 0 was accepted"),
        };
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn manifest_record_decode_rejects_unknown_tag() {
        let data = [0xFFu8];
        let mut pos = 0;
        let kind = match ManifestRecord::decode(&data, &mut pos) {
            Err(e) => e.kind(),
            Ok(_) => panic!("expected error on unknown tag"),
        };
        assert_eq!(kind, io::ErrorKind::InvalidData);
    }

    #[test]
    fn manifest_record_decode_rejects_invalid_level_indexes() {
        let records = [
            ManifestRecord::AddFile {
                level: MAX_LEVELS,
                meta: test_meta(1),
            },
            ManifestRecord::RemoveFile {
                level: MAX_LEVELS,
                file_id: 1,
            },
        ];

        for record in records {
            let mut data = Vec::new();
            record.encode(&mut data);
            let mut pos = 0;
            let kind = match ManifestRecord::decode(&data, &mut pos) {
                Err(e) => e.kind(),
                Ok(_) => panic!("expected invalid level error"),
            };
            assert_eq!(kind, io::ErrorKind::InvalidData);
        }
    }

    #[test]
    fn manifest_record_decode_returns_none_at_eof() {
        let mut pos = 0;
        let got = ManifestRecord::decode(&[], &mut pos).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn apply_rejects_invalid_level_indexes() {
        let dir = TempDir::new().unwrap();
        let sst_dir = dir.path().join("sst");
        std::fs::create_dir_all(&sst_dir).unwrap();

        let file = make_live_sst(&sst_dir, 1, b"a", b"z");
        let mut vs = VersionSet::open(dir.path(), &sst_dir).unwrap();

        let kind = match vs.apply(&[VersionEdit::AddFile {
            level: MAX_LEVELS,
            file,
        }]) {
            Err(e) => e.kind(),
            Ok(_) => panic!("expected invalid level error"),
        };
        assert_eq!(kind, io::ErrorKind::InvalidData);
        assert_eq!(vs.current().levels.iter().map(Vec::len).sum::<usize>(), 0);

        let kind = match vs.apply(&[VersionEdit::RemoveFile {
            level: MAX_LEVELS,
            file_id: 1,
        }]) {
            Err(e) => e.kind(),
            Ok(_) => panic!("expected invalid level error"),
        };
        assert_eq!(kind, io::ErrorKind::InvalidData);
    }

    #[test]
    fn open_rejects_manifest_with_invalid_level_index() {
        let dir = TempDir::new().unwrap();
        let sst_dir = dir.path().join("sst");
        std::fs::create_dir_all(&sst_dir).unwrap();

        let records = [ManifestRecord::AddFile {
            level: MAX_LEVELS,
            meta: test_meta(1),
        }];
        let mut bytes = VersionSet::encode_stamp().to_vec();
        bytes.extend(
            VersionSet::encode_records(&records, MANIFEST_STAMP_LEN as u64, None)
                .unwrap()
                .0,
        );
        std::fs::write(dir.path().join("MANIFEST"), bytes).unwrap();

        let kind = match VersionSet::open(dir.path(), &sst_dir) {
            Err(e) => e.kind(),
            Ok(_) => panic!("expected invalid level error"),
        };
        assert_eq!(kind, io::ErrorKind::InvalidData);
    }

    #[test]
    fn replay_survives_truncated_trailer() {
        // Write a manifest, then truncate it inside the last record.
        // Replay should stop cleanly and expose the valid prefix.
        let dir = TempDir::new().unwrap();
        let sst_dir = dir.path().join("sst");
        std::fs::create_dir_all(&sst_dir).unwrap();

        let file1 = make_live_sst(&sst_dir, 1, b"a", b"m");
        let mut vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
        vs.apply(&[VersionEdit::AddFile {
            level: 0,
            file: Arc::clone(&file1),
        }])
        .unwrap();
        vs.apply(&[VersionEdit::SetLastSeq(50)]).unwrap();
        drop(vs);

        // Truncate 2 bytes off the end - enough to damage the final
        // record's checksum or tail.
        let path = dir.path().join("MANIFEST");
        let current = std::fs::metadata(&path).unwrap().len();
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(current - 2)
            .unwrap();

        let vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
        let v = vs.current();
        // The AddFile should have survived (it was the first record);
        // the SetLastSeq may or may not survive depending on where the
        // truncation landed. Either way, we should NOT panic.
        assert!(v.levels[0].len() <= 1);
    }

    #[test]
    fn reset_record_clears_files_and_sets_wal_floor_atomically() {
        let dir = TempDir::new().unwrap();
        let sst_dir = dir.path().join("sst");
        std::fs::create_dir_all(&sst_dir).unwrap();

        let file1 = make_live_sst(&sst_dir, 1, b"a", b"m");
        let file2 = make_live_sst(&sst_dir, 2, b"n", b"z");
        {
            let mut vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
            vs.apply(&[
                VersionEdit::AddFile {
                    level: 0,
                    file: Arc::clone(&file1),
                },
                VersionEdit::AddFile {
                    level: 1,
                    file: Arc::clone(&file2),
                },
                VersionEdit::SetLastSeq(50),
                VersionEdit::SetNextFileId(9),
            ])
            .unwrap();
            vs.apply(&[VersionEdit::Reset {
                next_file_id: 10,
                min_wal_id: 9,
            }])
            .unwrap();
        }

        let vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
        let v = vs.current();
        assert!(v.levels.iter().all(Vec::is_empty));
        assert_eq!(v.last_seq, 0);
        assert_eq!(v.next_file_id, 10);
        assert_eq!(v.min_wal_id, 9);
    }

    #[test]
    fn open_truncates_truncated_manifest_tail_before_append() {
        let dir = TempDir::new().unwrap();
        let sst_dir = dir.path().join("sst");
        std::fs::create_dir_all(&sst_dir).unwrap();

        {
            let mut vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
            vs.apply(&[VersionEdit::SetLastSeq(7)]).unwrap();
            vs.apply(&[VersionEdit::SetLastSeq(11)]).unwrap();
        }

        let path = dir.path().join("MANIFEST");
        let current = std::fs::metadata(&path).unwrap().len();
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(current - 2)
            .unwrap();

        {
            let mut vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
            assert_eq!(vs.current().last_seq, 7);
            vs.apply(&[VersionEdit::SetLastSeq(99)]).unwrap();
        }

        let vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
        assert_eq!(vs.current().last_seq, 99);
    }

    #[test]
    fn open_truncates_corrupt_manifest_tail_before_append() {
        let dir = TempDir::new().unwrap();
        let sst_dir = dir.path().join("sst");
        std::fs::create_dir_all(&sst_dir).unwrap();

        {
            let mut vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
            vs.apply(&[VersionEdit::SetLastSeq(7)]).unwrap();
            vs.apply(&[VersionEdit::SetLastSeq(11)]).unwrap();
        }

        let path = dir.path().join("MANIFEST");
        let checksum_offset = second_record_checksum_offset(&path);
        let mut data = std::fs::read(&path).unwrap();
        data[checksum_offset] ^= 0xFF;
        std::fs::write(&path, data).unwrap();

        {
            let mut vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
            assert_eq!(vs.current().last_seq, 7);
            vs.apply(&[VersionEdit::SetLastSeq(99)]).unwrap();
        }

        let vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
        assert_eq!(vs.current().last_seq, 99);
    }

    /// A `SetLastSeq` below the running value must not lower it, whether
    /// the lower stamp arrives on its own or batched with an even lower
    /// one, and the raised value must survive a reopen. Fails if `apply`
    /// reverts to a store: the first assertion below would then read 7.
    #[test]
    fn a_lower_set_last_seq_leaves_the_higher_value() {
        let dir = TempDir::new().unwrap();
        let sst_dir = dir.path().join("sst");
        std::fs::create_dir_all(&sst_dir).unwrap();

        {
            let mut vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
            vs.apply(&[VersionEdit::SetLastSeq(11)]).unwrap();
            vs.apply(&[VersionEdit::SetLastSeq(7)]).unwrap();
            assert_eq!(vs.current().last_seq, 11);

            vs.apply(&[VersionEdit::SetLastSeq(9), VersionEdit::SetLastSeq(4)])
                .unwrap();
            assert_eq!(vs.current().last_seq, 11);

            vs.apply(&[VersionEdit::SetLastSeq(12)]).unwrap();
            assert_eq!(vs.current().last_seq, 12);
        }

        // The log holds 11, 7, 9, 4, 12 verbatim; replay must reach 12,
        // the running maximum, not 12's raw value from a differently
        // ordered replay.
        let vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
        assert_eq!(vs.current().last_seq, 12);
    }

    /// `replay_manifest` must apply the same maximum rule `apply` does,
    /// and `Reset` must still be the one path that lowers `last_seq`.
    /// Fails if replay reverts to a store (reads 7 after the first
    /// reopen, since the log ends on the lower record); fails if `Reset`
    /// stops zeroing (reads 11 instead of 3 after the second).
    #[test]
    fn replay_takes_the_highest_stamp_since_the_last_reset() {
        let dir = TempDir::new().unwrap();
        let sst_dir = dir.path().join("sst");
        std::fs::create_dir_all(&sst_dir).unwrap();

        {
            let mut vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
            vs.apply(&[VersionEdit::SetLastSeq(11)]).unwrap();
            vs.apply(&[VersionEdit::SetLastSeq(7)]).unwrap();
        }

        {
            let mut vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
            assert_eq!(vs.current().last_seq, 11);

            vs.apply(&[VersionEdit::Reset {
                next_file_id: 5,
                min_wal_id: 4,
            }])
            .unwrap();
            vs.apply(&[VersionEdit::SetLastSeq(3)]).unwrap();
            assert_eq!(vs.current().last_seq, 3);
        }

        let vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
        assert_eq!(vs.current().last_seq, 3);
        assert_eq!(vs.current().next_file_id, 5);
        assert_eq!(vs.current().min_wal_id, 4);
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 32, .. ProptestConfig::default() })]

        /// Random flush and ingest stamps, applied in order, must never
        /// lower `current().last_seq`, live or after a reopen. Each
        /// `SetLastSeq` apply syncs the manifest, so the case count
        /// bounds the fsyncs this test pays at 32 x 24. The one mutation
        /// that fails it is `raised_last_seq` returning `stamp` (a
        /// store): with up to 24 random `u64` stamps per case, a
        /// descending pair occurs in essentially every case.
        #[test]
        fn last_seq_never_decreases_across_random_stamps(
            stamps in proptest::collection::vec((any::<u64>(), any::<bool>()), 1..=24),
        ) {
            let dir = TempDir::new().unwrap();
            let sst_dir = dir.path().join("sst");
            std::fs::create_dir_all(&sst_dir).unwrap();

            let mut vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
            let mut running_max = 0u64;
            let mut previous = 0u64;

            for (i, (v, is_flush)) in stamps.iter().enumerate() {
                running_max = running_max.max(*v);
                if *is_flush {
                    vs.apply(&[
                        VersionEdit::SetNextFileId(i as u64 + 2),
                        VersionEdit::SetLastSeq(*v),
                    ])
                    .unwrap();
                } else {
                    vs.apply(&[VersionEdit::SetLastSeq(*v)]).unwrap();
                }

                let current = vs.current().last_seq;
                prop_assert_eq!(current, running_max);
                prop_assert!(current >= previous);
                previous = current;
            }

            drop(vs);
            let vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
            prop_assert_eq!(vs.current().last_seq, running_max);
        }
    }

    /// The manifest is an append-only log, so without a rewrite trigger a
    /// database that runs for years replays every edit it ever made. This
    /// applies far more edits than the trigger allows and asserts the
    /// file stays bounded rather than growing with the history.
    #[test]
    fn a_long_running_manifest_stays_bounded_instead_of_growing_forever() {
        let dir = TempDir::new().unwrap();
        let sst_dir = dir.path().join("sst");
        std::fs::create_dir_all(&sst_dir).unwrap();
        let path = dir.path().join("MANIFEST");

        let mut vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
        // `SetNextFileId`, not `SetLastSeq`: it is the one edit that
        // does not force a sync (`requires_manifest_sync`), and it grows
        // the log by the same record size, so it exercises exactly the
        // growth-and-rewrite behaviour under test without paying an
        // fsync per iteration. That distinction is the whole cost here.
        // At roughly 17 bytes a record this appends about 136 KiB
        // against a 64 KiB rewrite threshold, so the assertion below can
        // only pass if the log was rewritten at least twice.
        //
        // With the syncing edit this was 8,000 fsyncs, which a Windows
        // CI disk served at about 75 ms each and took past ten minutes,
        // while Linux finished it in milliseconds.
        const EDITS: u64 = 8_000;
        for id in 1..=EDITS {
            vs.apply(&[VersionEdit::SetNextFileId(id)]).unwrap();
        }
        drop(vs);

        let len = std::fs::metadata(&path).unwrap().len();
        let bound = VersionSet::compact_threshold(0);
        assert!(
            len <= bound,
            "manifest grew to {len} bytes against a {bound}-byte bound: \
             an append-only log that is never rewritten grows without limit"
        );

        // And it still replays to the state those edits describe.
        let reopened = VersionSet::open(dir.path(), &sst_dir).unwrap();
        assert_eq!(reopened.current().next_file_id, EDITS);
    }

    #[test]
    fn a_manifest_from_a_newer_format_is_refused() {
        let mut stamp = VersionSet::encode_stamp();
        stamp[7] = MANIFEST_FORMAT_SEALED + 1;
        let checksum = checksum::manifest_record(0, &stamp[0..8]);
        stamp[8..12].copy_from_slice(&checksum.to_le_bytes());

        let err =
            VersionSet::stamp_len(&stamp, None).expect_err("a newer format must not be parsed");
        assert!(err.to_string().contains("newer regolith"), "{err}");
    }

    #[test]
    fn compact_manifest_rewrites_to_canonical_form() {
        // Apply many edits, then compact. The resulting manifest
        // should replay to the same version.
        let dir = TempDir::new().unwrap();
        let sst_dir = dir.path().join("sst");
        std::fs::create_dir_all(&sst_dir).unwrap();

        let file1 = make_live_sst(&sst_dir, 1, b"a", b"c");
        let file2 = make_live_sst(&sst_dir, 2, b"d", b"f");

        let mut vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
        vs.apply(&[VersionEdit::Reset {
            next_file_id: 7,
            min_wal_id: 7,
        }])
        .unwrap();
        vs.apply(&[VersionEdit::AddFile {
            level: 0,
            file: Arc::clone(&file1),
        }])
        .unwrap();
        vs.apply(&[VersionEdit::AddFile {
            level: 1,
            file: Arc::clone(&file2),
        }])
        .unwrap();
        vs.apply(&[VersionEdit::SetLastSeq(500), VersionEdit::SetNextFileId(99)])
            .unwrap();

        let pre_size = std::fs::metadata(dir.path().join("MANIFEST"))
            .unwrap()
            .len();
        vs.compact_manifest().unwrap();
        let post_size = std::fs::metadata(dir.path().join("MANIFEST"))
            .unwrap()
            .len();
        // Compaction produces a single snapshot, so it is typically
        // not larger than the history it replaced.
        assert!(post_size <= pre_size + 64);

        drop(vs);
        let vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
        let v = vs.current();
        assert_eq!(v.levels[0].len(), 1);
        assert_eq!(v.levels[1].len(), 1);
        assert_eq!(v.last_seq, 500);
        assert_eq!(v.next_file_id, 99);
        assert_eq!(v.min_wal_id, 7);
    }
}
