//! SSTable file format: sorted on-disk tables of MVCC-encoded key-value pairs.
//!
//! Layout:
//! ```text
//! [data block 0][data block 1]...[data block n][range_tombstone block][bloom region][index block][footer]
//! ```
//!
//! The bloom region is two blooms concatenated behind a header:
//! `[prefix_bloom_len: 7 bytes LE][prefix form: u8][prefix_bloom_bytes][user_key_bloom_bytes]`.
//! A zero length means the file was written without a prefix extractor.
//! The form says which key the prefix bloom was built from (`prefix_call`).
//!
//! Data blocks store **internal keys** - `user_key || !seq || value_type` -
//! sorted so that newer versions of the same user key appear before older
//! ones. Tombstones are first-class entries; reads that land on a tombstone
//! at or before `snapshot_seq` return "deleted" and suppress older versions
//! in lower levels. The bloom filter is keyed on user keys so point lookups
//! can short-circuit regardless of which seq a reader is asking for.
//!
//! # Format versions and compatibility
//!
//! The trailing 8 bytes of every SSTable are the magic number, so the
//! footer's size is discoverable before anything else is parsed.
//!
//! ```text
//! MAGIC_V5 (version byte 0x05) flat index,        72-byte footer, metadata checksummed
//! MAGIC_V6 (version byte 0x06) partitioned index, 72-byte footer, metadata checksummed
//! MAGIC_V7 (version byte 0x07) flat index,        116-byte footer, every region sealed
//! MAGIC_V8 (version byte 0x08) partitioned index, 116-byte footer, every region sealed
//! ```
//!
//! All carry the `REGOSST` identifier. V5 and V6 are the formats regolith
//! has written since its first release; V7 and V8 are what it writes when
//! the database is encrypted at rest (see the `sealed` submodule). A file
//! whose trailing magic is anything else is refused with "invalid SSTable
//! magic number", which is the correct loud failure.
//!
//! # What a checksum covers
//!
//! In a V3/V4 table every byte belongs to exactly one checksummed
//! region. A data block is framed `[compression: u8][payload][checksum:
//! u32]`. Each metadata region (range-tombstone block, bloom region,
//! every partitioned index leaf, the index block) carries a 4-byte
//! trailer counted inside the size the footer or the leaf handle records
//! for it. The footer carries an 8-byte checksum over its seven fixed
//! fields and its magic, so a damaged offset, size or entry count is
//! caught before it is trusted.
//!
//! Two holes are deliberate and are not closed here. A V1/V2 table has
//! no metadata checksum at all, which is the price of reading yesterday's
//! files; it is protected only by the region-bound validation against the
//! file size. And these are xxh3 checksums, an accidental-corruption
//! guard and not a MAC: they catch bit rot and torn writes, not an
//! attacker who can rewrite the file and its checksum together.

use std::io::{self};
use std::path::{Path, PathBuf};

// The module's own tests craft corrupt SSTable files byte by byte,
// which is the one thing that has to bypass the environment.
#[cfg(test)]
use std::fs;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use std::sync::{Arc, OnceLock, Weak};

use std::ops::ControlFlow;

use super::block::{Block, BlockBuilder, BlockHandle, RESTART_INTERVAL, decode_entry_at};
use super::block_cache::BlockCache;
use super::bloom::{BloomFilterBuilder, encode_bloom_block};
use super::checksum;
use super::filter_block::{FilterBlock, prefix_header, prefix_section};
use super::index_block::IndexBlock;
#[cfg(test)]
use super::internal_key::user_key_of;
use super::internal_key::{VALUE_TYPE_DELETION, VALUE_TYPE_MERGE, decode_internal_key};
use super::io::Landed;
use super::io::scope;
use super::io::unit::{UnitKey, Work};
use super::lookup_key::LookupKey;
use super::prefix_call::{self, PrefixForm, PrefixProbe};
use super::range_tombstone::{RangeTombstone, RangeTombstoneSet, table_key_range};
use super::seal::{Keyring, OVERHEAD};
use crate::DbSlice;
use crate::env::{BufferedWriter, Env, ReadFile, WriteMode};
use crate::options::{CompressionType, PrefixExtractor};

mod block_codec;
mod deletions;
mod global_seq;
mod key_walk;
mod sealed;
#[cfg(test)]
mod size_limit_tests;

pub(crate) use deletions::{BlockCount, DeletionCounter, Deletions};
use sealed::{MAGIC_V7, MAGIC_V8, SEALED_FOOTER_SIZE};
pub(crate) use sealed::{TableSeal, authenticate_footer};

/// SSTable magic number: "REGOSST\x05" - flat index, 72-byte footer,
/// metadata regions checksummed. Written by regolith today.
const MAGIC_V5: u64 = 0x5245474F_53535405;

/// SSTable magic number: "REGOSST\x06" - partitioned index, 72-byte
/// footer, metadata regions checksummed. Written by regolith today.
const MAGIC_V6: u64 = 0x5245474F_53535406;

/// Footer size: seven 8-byte fields, an 8-byte metadata checksum, then
/// the magic.
const FOOTER_SIZE: usize = 72;

/// Bytes a metadata region carries after its payload. The trailer is
/// counted inside the region size the footer records, so bounds
/// arithmetic reads the same number either way.
const META_CHECKSUM_LEN: usize = 4;

const COMPRESSION_NONE: u8 = 0x00;
const COMPRESSION_LZ4: u8 = 0x01;
const COMPRESSION_SNAPPY: u8 = 0x02;

/// SSTable footer, written at the end of the file. Its length depends on
/// the format version the magic names: 64 bytes for V1/V2, 72 for V3/V4.
///
/// Layout on disk:
/// ```text
/// [data blocks][range_tombstone_block][bloom_region][index_block][footer]
/// ```
///
/// Footer bytes, all little-endian:
/// ```text
/// [ 0.. 8) range_tombstone_offset
/// [ 8..16) range_tombstone_size   (0 means no range tombstones)
/// [16..24) bloom_offset
/// [24..32) bloom_size
/// [32..40) index_offset
/// [40..48) index_size
/// [48..56) num_entries
/// V1/V2: [56..64) magic
/// V3/V4: [56..64) checksum over bytes 0..56 and the magic
///        [64..72) magic
/// ```
///
/// The seven fixed fields sit at the same offsets from the footer's start
/// in both layouts, and the magic is always the file's last 8 bytes, so
/// the footer's length is discoverable before it is parsed.
#[derive(Debug)]
struct Footer {
    range_tombstone_offset: u64,
    range_tombstone_size: u64,
    bloom_offset: u64,
    bloom_size: u64,
    index_offset: u64,
    index_size: u64,
    num_entries: u64,
    magic: u64,
    /// The whole footer of a sealed table, which `TableSeal::open_footer`
    /// verifies before anything it says is trusted. `None` for an unsealed
    /// table, whose footer checksum has already been checked.
    sealed: Option<[u8; SEALED_FOOTER_SIZE]>,
}

impl Footer {
    /// Whether `magic` names the partitioned index layout, where the
    /// footer's index handle points at a top-level index of leaves.
    fn magic_is_partitioned(magic: u64) -> bool {
        matches!(magic, MAGIC_V6 | MAGIC_V8)
    }

    /// Footer byte length implied by `magic`, or an error naming the
    /// magic when it is not one regolith writes or reads.
    fn size_for_magic(magic: u64) -> io::Result<usize> {
        match magic {
            MAGIC_V5 | MAGIC_V6 => Ok(FOOTER_SIZE),
            MAGIC_V7 | MAGIC_V8 => Ok(SEALED_FOOTER_SIZE),
            other => Err(invalid_data(format!(
                "invalid SSTable magic number: {other:#018x}"
            ))),
        }
    }

    fn size(&self) -> usize {
        if self.sealed.is_some() {
            SEALED_FOOTER_SIZE
        } else {
            FOOTER_SIZE
        }
    }

    /// Bytes each region carries beside its payload: a checksum trailer,
    /// or the nonce and tag of a sealed frame.
    fn region_overhead(&self) -> u64 {
        if self.sealed.is_some() {
            OVERHEAD as u64
        } else {
            META_CHECKSUM_LEN as u64
        }
    }

    /// The seven fixed fields, in their on-disk order.
    fn fields(&self) -> [u8; 56] {
        let mut buf = [0u8; 56];
        buf[0..8].copy_from_slice(&self.range_tombstone_offset.to_le_bytes());
        buf[8..16].copy_from_slice(&self.range_tombstone_size.to_le_bytes());
        buf[16..24].copy_from_slice(&self.bloom_offset.to_le_bytes());
        buf[24..32].copy_from_slice(&self.bloom_size.to_le_bytes());
        buf[32..40].copy_from_slice(&self.index_offset.to_le_bytes());
        buf[40..48].copy_from_slice(&self.index_size.to_le_bytes());
        buf[48..56].copy_from_slice(&self.num_entries.to_le_bytes());
        buf
    }

    /// Encode into the layout `self.magic` selects: sealed under `seal`
    /// when there is one.
    fn encode(&self, seal: Option<&TableSeal>) -> io::Result<Vec<u8>> {
        let fields = self.fields();
        if let Some(seal) = seal {
            return Ok(seal.encode_footer(&fields, self.magic)?.to_vec());
        }
        let mut buf = vec![0u8; FOOTER_SIZE];
        buf[0..56].copy_from_slice(&fields);
        let sum = checksum::sst_footer(&buf[0..56], self.magic).to_le_bytes();
        buf[56..64].copy_from_slice(&sum);
        buf[64..72].copy_from_slice(&self.magic.to_le_bytes());
        Ok(buf)
    }

    /// Decode a footer whose length is exactly what its magic implies.
    ///
    /// The magic is validated first, so a table from a format version
    /// regolith does not implement is reported as a magic error rather than
    /// as a checksum error. A sealed footer is parsed but not yet verified:
    /// that takes its key.
    fn decode(buf: &[u8]) -> io::Result<Self> {
        if buf.len() < FOOTER_SIZE {
            return Err(invalid_data("SSTable footer is truncated"));
        }
        let magic = u64::from_le_bytes(buf[buf.len() - 8..].try_into().unwrap());
        let expected = Self::size_for_magic(magic)?;
        if buf.len() != expected {
            return Err(invalid_data(format!(
                "SSTable footer is {} bytes but its format version needs {expected}",
                buf.len()
            )));
        }
        let sealed = if matches!(magic, MAGIC_V7 | MAGIC_V8) {
            let mut whole = [0u8; SEALED_FOOTER_SIZE];
            whole.copy_from_slice(buf);
            Some(whole)
        } else {
            let stored = u64::from_le_bytes(buf[56..64].try_into().unwrap());
            if stored != checksum::sst_footer(&buf[0..56], magic) {
                return Err(invalid_data("SSTable footer checksum mismatch"));
            }
            None
        };
        Ok(Self {
            range_tombstone_offset: u64::from_le_bytes(buf[0..8].try_into().unwrap()),
            range_tombstone_size: u64::from_le_bytes(buf[8..16].try_into().unwrap()),
            bloom_offset: u64::from_le_bytes(buf[16..24].try_into().unwrap()),
            bloom_size: u64::from_le_bytes(buf[24..32].try_into().unwrap()),
            index_offset: u64::from_le_bytes(buf[32..40].try_into().unwrap()),
            index_size: u64::from_le_bytes(buf[40..48].try_into().unwrap()),
            num_entries: u64::from_le_bytes(buf[48..56].try_into().unwrap()),
            magic,
            sealed,
        })
    }

    /// The seal of a sealed table once its footer verifies, or `None` for
    /// an unsealed one.
    fn open_seal(&self, keyring: Option<&Keyring>, path: &Path) -> io::Result<Option<TableSeal>> {
        self.sealed
            .as_ref()
            .map(|footer| TableSeal::open_footer(keyring, footer, path))
            .transpose()
    }
}

/// The payload of a metadata region read whole at `offset`: its checksum
/// trailer verified and cut off, or, in a table sealed under `seal`, its
/// frame opened. Truncates or shifts in place, so it costs no copy beyond
/// a sealed region's 12-byte shift.
fn unframe_meta_region(
    seal: Option<&TableSeal>,
    mut region: Vec<u8>,
    offset: u64,
    kind: u8,
    file_id: u64,
    name: &'static str,
) -> io::Result<Vec<u8>> {
    if let Some(seal) = seal {
        return seal.open_region(kind, offset, region, file_id, name);
    }
    let len = verify_meta_region(&region, kind, true, name)?.len();
    region.truncate(len);
    Ok(region)
}

/// Read the trailing footer of an SSTable. The magic is the file's last
/// 8 bytes, so the footer's length is known before it is parsed.
fn read_footer(file: &dyn ReadFile, file_size: u64) -> io::Result<Footer> {
    if file_size < 8 {
        return Err(invalid_data(
            "SSTable file too small to carry a magic number",
        ));
    }
    let mut magic_buf = [0u8; 8];
    file.read_exact_at(file_size - 8, &mut magic_buf)?;
    let footer_size = Footer::size_for_magic(u64::from_le_bytes(magic_buf))?;
    if file_size < footer_size as u64 {
        return Err(invalid_data("SSTable file too small for its footer"));
    }
    let mut footer_buf = vec![0u8; footer_size];
    file.read_exact_at(file_size - footer_size as u64, &mut footer_buf)?;
    Footer::decode(&footer_buf)
}

/// Strip and verify a metadata region's checksum trailer, returning the
/// payload the decoder should parse.
///
/// A `checksummed` of `false` returns the region unchanged, which is how
/// a V1/V2 table keeps reading: those files carry no trailer, so there is
/// nothing to strip and nothing to verify.
fn verify_meta_region<'a>(
    region: &'a [u8],
    kind: u8,
    checksummed: bool,
    name: &'static str,
) -> io::Result<&'a [u8]> {
    if !checksummed {
        return Ok(region);
    }
    if region.len() < META_CHECKSUM_LEN {
        return Err(invalid_data(format!(
            "{name} is too short to carry a checksum"
        )));
    }
    let (payload, trailer) = region.split_at(region.len() - META_CHECKSUM_LEN);
    let stored = u32::from_le_bytes(trailer.try_into().unwrap());
    if stored != checksum::sst_meta(kind, payload) {
        return Err(invalid_data(format!("{name} checksum mismatch")));
    }
    Ok(payload)
}

fn encode_range_tombstone_block(tombstones: &[RangeTombstone]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&(tombstones.len() as u32).to_le_bytes());
    for rt in tombstones {
        buf.extend_from_slice(&(rt.start.len() as u32).to_le_bytes());
        buf.extend_from_slice(&rt.start);
        buf.extend_from_slice(&(rt.end.len() as u32).to_le_bytes());
        buf.extend_from_slice(&rt.end);
        buf.extend_from_slice(&rt.seq.to_le_bytes());
    }
    buf
}

pub(crate) fn decode_range_tombstone_block(data: &[u8]) -> io::Result<Vec<RangeTombstone>> {
    if data.is_empty() {
        return Ok(Vec::new());
    }
    if data.len() < 4 {
        return Err(invalid_data("range tombstone block too short"));
    }
    let count = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
    let max_records_by_size = (data.len() - 4) / 16;
    let mut out = Vec::with_capacity(count.min(max_records_by_size));
    let mut pos = 4;
    for _ in 0..count {
        let start_len = read_u32(data, &mut pos, "range tombstone start length")? as usize;
        let start = read_bytes(data, &mut pos, start_len, "range tombstone start")?;
        let end_len = read_u32(data, &mut pos, "range tombstone end length")? as usize;
        let end = read_bytes(data, &mut pos, end_len, "range tombstone end")?;
        let seq = read_u64(data, &mut pos, "range tombstone seq")?;
        out.push(RangeTombstone::new(start, end, seq));
    }
    if pos != data.len() {
        return Err(invalid_data("range tombstone block has trailing bytes"));
    }
    Ok(out)
}

/// Metadata for an SSTable file, tracked by the manifest.
///
/// `smallest_key` and `largest_key` are **user keys**, used by the engine
/// and compaction for overlap checks. The level is tracked positionally
/// in [`super::manifest::Version::levels`], not here.
#[derive(Debug, Clone)]
pub(crate) struct SsTableMeta {
    pub(crate) file_id: u64,
    pub(crate) smallest_key: Vec<u8>,
    pub(crate) largest_key: Vec<u8>,
    pub(crate) file_size: u64,
    pub(crate) num_entries: u64,
    /// The sequence every entry of an ingested table reads at, whatever
    /// sequence the file stores (D48). `None` for a table regolith wrote,
    /// whose entries carry their own. A compaction rewrites an ingested
    /// table's entries at this sequence, so its outputs have `None`.
    pub(crate) global_seq: Option<u64>,
    /// The table's deletions and what they hide, counted as it was written.
    /// `None` for a table whose manifest record carries no counts: one
    /// written before they existed, restored from a backup, or ingested
    /// without a read of every entry. The census counts it later
    /// (`compaction::census`).
    pub(crate) deletions: Option<Deletions>,
}

/// A live SSTable: metadata plus an already-opened reader. Held by
/// [`super::manifest::Version`] so that as long as any version
/// referencing this file is alive, the underlying file descriptor stays
/// open - reads remain valid even after a concurrent compaction unlinks
/// the file from disk (the kernel keeps the inode alive via FD
/// refcounting).
///
/// `Arc<LiveSst>` is shared between versions and between a version and
/// any iterator built from it, so cloning is cheap.
pub(crate) struct LiveSst {
    /// Bound supplied by the writer, the ingest stamp, or the recovered
    /// manifest. This is an in-memory invariant, not a new file field.
    pub(crate) sequence_ceiling: u64,
    pub(crate) meta: SsTableMeta,
    pub(crate) reader: Arc<SsTableReader>,
    /// A compaction pass is working on this table and no other may pick it.
    /// Set and cleared only through `compaction::claims`.
    pub(crate) claimed: crate::portability::AtomicBool,
}

impl LiveSst {
    /// Pair a table's metadata with its reader. The reader must read at the
    /// sequence the metadata records, which is why both come from the one
    /// [`SsTableMeta::global_seq`].
    pub(crate) fn new(
        meta: SsTableMeta,
        reader: Arc<SsTableReader>,
        sequence_ceiling: u64,
    ) -> Arc<Self> {
        debug_assert!(meta.global_seq.is_none_or(|seq| seq <= sequence_ceiling));
        debug_assert_eq!(
            meta.global_seq, reader.global_seq,
            "a table's reader reads at the sequence its metadata records"
        );
        // Every table a read can reach is made here, so this is where its
        // reader learns how to hand a queued read its own handle.
        let _ = reader.me.set(Arc::downgrade(&reader));
        Arc::new(Self {
            sequence_ceiling,
            meta,
            reader,
            claimed: crate::portability::AtomicBool::new(false),
        })
    }
}

/// Where a reader sits in a file's block index.
///
/// A flat (V1) file indexes its data blocks by position within its one
/// index block. A partitioned (V2) file indexes them by leaf, so the
/// cursor carries the leaf it is walking. Holding that leaf as an `Arc`
/// is what makes an eviction of the leaf from the block cache unable to
/// invalidate a cursor already positioned inside it.
#[derive(Clone)]
pub(crate) enum SsTableBlockCursor {
    Flat(usize),
    Partitioned {
        leaf_idx: usize,
        entry_idx: usize,
        leaf: Arc<IndexBlock>,
    },
}

/// How a reader holds its index and filter blocks.
///
/// A partitioned file's top-level index is pinned under both policies:
/// it is the entry point to every other index read and is small by
/// construction (one entry per leaf), which is exactly what makes the
/// leaves below it affordable to fetch through the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MetadataPolicy {
    /// Decode once at open and hold for the reader's life. These bytes
    /// sit outside the `Options::block_cache_size` budget.
    Pinned,
    /// Load through the block cache on demand, so the configured cache
    /// budget covers index and filter bytes as well as data.
    Cached,
}

/// Where a reader's flat index or filter region lives.
enum MetaSlot<T> {
    /// Held for the reader's life, outside the cache budget.
    Pinned(Arc<T>),
    /// Fetched through the block cache from this file region.
    Cached(BlockHandle),
}

/// A metadata block reached either by borrowing the reader's pinned
/// copy or by holding a reference on the cache's copy.
///
/// The borrowed arm keeps the default (pinned) read path free of
/// reference-count traffic on the hottest lookup in the engine.
enum MetaRef<'a, T> {
    Borrowed(&'a T),
    Owned(Arc<T>),
}

impl<T> std::ops::Deref for MetaRef<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        match self {
            Self::Borrowed(value) => value,
            Self::Owned(value) => value,
        }
    }
}

/// Result of looking up a user key in a single SSTable.
///
/// The `seq` carried by [`LookupResult::Found`] / [`LookupResult::FoundTombstone`]
/// is the sequence number of the winning point entry, which the caller
/// compares against range-tombstone coverage from this and newer sources
/// to decide the final visibility.
///
/// `V` is what a hit carries: a [`DbSlice`] borrowing the block for
/// [`SsTableReader::get`], or a `usize` length for
/// [`SsTableReader::get_size`]. Both come from the same block scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LookupResult<V = DbSlice> {
    /// No visible version for this user key in this SSTable.
    NotInTable,
    /// Found a value at or before the requested snapshot. For the
    /// `DbSlice` form, `value` borrows the decoded block it was read
    /// from and the `Arc<Block>` inside it keeps those bytes alive.
    Found { seq: u64, value: V },
    /// Found a tombstone at or before the requested snapshot.
    FoundTombstone { seq: u64 },
}

impl<V> LookupResult<V> {
    /// Re-wrap a hit's payload, leaving misses and tombstones alone.
    pub(crate) fn map_value<W>(self, f: impl FnOnce(V) -> W) -> LookupResult<W> {
        match self {
            Self::NotInTable => LookupResult::NotInTable,
            Self::FoundTombstone { seq } => LookupResult::FoundTombstone { seq },
            Self::Found { seq, value } => LookupResult::Found {
                seq,
                value: f(value),
            },
        }
    }
}

/// Whether a point lookup hands back the value's bytes or only its
/// length.
///
/// `LengthOnly` is what [`crate::Db::has`] and [`crate::Db::get_size`]
/// read with. It answers from the same block scan and pays the same
/// block read (a bloom filter can only rule a key out, so ruling one in
/// still requires the data block), but it never takes the reference on
/// the owning block that a [`DbSlice`] would.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Materialize {
    /// Hand back the value as a [`DbSlice`] over its owner.
    Value,
    /// Hand back the value's length and nothing else.
    LengthOnly(Option<usize>),
}

/// What a point lookup across every source produced.
///
/// The engine's source walk (memtables, then L0, then L1+) resolves a
/// key once and projects this into either a value or a length, so
/// [`crate::Db::get`], [`crate::Db::get_slice`], [`crate::Db::has`] and
/// [`crate::Db::get_size`] all share one implementation of MVCC and
/// range-tombstone precedence rather than four copies of it.
pub(crate) enum PointValue {
    /// The value's bytes, borrowed from whatever already owns them.
    Value(DbSlice),
    /// The value's length, requested under [`Materialize::LengthOnly`].
    Length(usize),
}

impl PointValue {
    /// Project a resolved value according to what the caller asked for.
    pub(crate) fn of(value: DbSlice, materialize: Materialize) -> Self {
        match materialize {
            Materialize::Value => Self::Value(value),
            Materialize::LengthOnly(_) => Self::Length(value.len()),
        }
    }

    /// Length in bytes of the value, whichever form it took.
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Value(value) => value.len(),
            Self::Length(len) => *len,
        }
    }
}

/// The winning entry a borrowing block scan landed on, as a position
/// inside the block rather than as copied bytes.
enum BlockHit {
    /// A deletion terminator.
    Tombstone { seq: u64 },
    /// A live value at `value_offset..value_offset + value_len` of the
    /// block's entry region.
    Value {
        seq: u64,
        value_offset: usize,
        value_len: usize,
    },
}

/// Summary of a finished SSTable, returned by [`SsTableWriter::finish`].
#[derive(Debug)]
pub(crate) struct SsTableWriteSummary {
    pub(crate) newest_seq: u64,
    pub(crate) smallest_user_key: Vec<u8>,
    pub(crate) largest_user_key: Vec<u8>,
    pub(crate) num_entries: u64,
    /// The deletions among the entries written and what they hide.
    pub(crate) deletions: Deletions,
}

fn encode_index_block(entries: &[(Vec<u8>, BlockHandle)]) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (key, handle) in entries {
        data.extend_from_slice(&(key.len() as u32).to_le_bytes());
        data.extend_from_slice(key);
        data.extend_from_slice(&handle.offset.to_le_bytes());
        data.extend_from_slice(&handle.size.to_le_bytes());
    }
    data
}

/// Compute the serialized byte size of an index block without allocating.
fn encoded_index_block_size(entries: &[(Vec<u8>, BlockHandle)]) -> usize {
    // 4 bytes for count, then per entry: 4 (key_len) + key + 8 (offset) + 8 (size)
    4 + entries.iter().map(|(k, _)| 4 + k.len() + 16).sum::<usize>()
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn read_u32(data: &[u8], pos: &mut usize, field: &'static str) -> io::Result<u32> {
    let end = pos
        .checked_add(4)
        .ok_or_else(|| invalid_data(format!("{field} offset overflows")))?;
    if end > data.len() {
        return Err(invalid_data(format!("{field} is truncated")));
    }
    let value = u32::from_le_bytes(data[*pos..end].try_into().unwrap());
    *pos = end;
    Ok(value)
}

fn read_u64(data: &[u8], pos: &mut usize, field: &'static str) -> io::Result<u64> {
    let end = pos
        .checked_add(8)
        .ok_or_else(|| invalid_data(format!("{field} offset overflows")))?;
    if end > data.len() {
        return Err(invalid_data(format!("{field} is truncated")));
    }
    let value = u64::from_le_bytes(data[*pos..end].try_into().unwrap());
    *pos = end;
    Ok(value)
}

fn read_bytes(
    data: &[u8],
    pos: &mut usize,
    len: usize,
    field: &'static str,
) -> io::Result<Vec<u8>> {
    let end = pos
        .checked_add(len)
        .ok_or_else(|| invalid_data(format!("{field} length overflows")))?;
    if end > data.len() {
        return Err(invalid_data(format!("{field} is truncated")));
    }
    let bytes = data[*pos..end].to_vec();
    *pos = end;
    Ok(bytes)
}

/// Check every region the footer points at against `data_end`, the first
/// byte past the region area (the file size less the footer). Every
/// region is checksummed or sealed, so each also has to be long enough to
/// hold its trailer or its nonce and tag.
fn validate_footer_regions(footer: &Footer, data_end: u64) -> io::Result<()> {
    let trailer = footer.region_overhead();
    if footer.bloom_size < 8 + trailer {
        return Err(invalid_data("bloom region too short"));
    }
    if footer.index_size < 4 + trailer {
        return Err(invalid_data("index block too short"));
    }
    validate_file_region(
        footer.bloom_offset,
        footer.bloom_size,
        data_end,
        "bloom region",
    )?;
    validate_file_region(
        footer.index_offset,
        footer.index_size,
        data_end,
        "index block",
    )?;
    if footer.range_tombstone_size > 0 {
        if footer.range_tombstone_size < 4 + trailer {
            return Err(invalid_data("range tombstone block too short"));
        }
        validate_file_region(
            footer.range_tombstone_offset,
            footer.range_tombstone_size,
            data_end,
            "range tombstone block",
        )?;
    }
    Ok(())
}

/// Append one metadata region followed by its checksum trailer, or as one
/// frame sealed under `seal`, and advance `offset` past it. The reader half
/// is `unframe_meta_region`; the two must agree on the framing or no table
/// opens.
fn write_meta_region(
    writer: &mut BufferedWriter,
    offset: &mut u64,
    payload: &[u8],
    kind: u8,
    seal: Option<&TableSeal>,
) -> io::Result<u64> {
    let size = if let Some(seal) = seal {
        let mut frame = Vec::with_capacity(payload.len() + OVERHEAD);
        seal.seal_region(kind, *offset, payload, &mut frame)?;
        writer.write_all(&frame)?;
        frame.len()
    } else {
        let trailer = checksum::sst_meta(kind, payload).to_le_bytes();
        writer.write_all(payload)?;
        writer.write_all(&trailer)?;
        payload.len() + trailer.len()
    } as u64;
    *offset += size;
    Ok(size)
}

fn check_data_block_limit(frame: u64, decoded: u64, limit: Option<usize>) -> io::Result<()> {
    if let Some(max_data_block_bytes) = limit
        && frame
            .checked_add(decoded)
            .is_none_or(|n| n > max_data_block_bytes as u64)
    {
        return Err(crate::Error::DataBlockLimitExceeded {
            max_data_block_bytes,
        }
        .into_io_error());
    }
    Ok(())
}

fn check_data_block_header(frame: u64, header: &[u8], limit: Option<usize>) -> io::Result<()> {
    let decoded = match header[0] {
        COMPRESSION_NONE => frame - 5,
        COMPRESSION_LZ4 | COMPRESSION_SNAPPY => {
            if frame < 9 || header.len() < 5 {
                return Err(invalid_data("compressed block header too short"));
            }
            u32::from_le_bytes(header[1..5].try_into().unwrap()) as u64
        }
        _ => return Err(invalid_data("unknown compression type")),
    };
    check_data_block_limit(frame, decoded, limit)
}

/// The raw block a data block's `payload` decodes to under its
/// compression type. Shared by sealed and checksummed frames.
fn decompress_block(compression_type: u8, payload: &[u8]) -> io::Result<Vec<u8>> {
    match compression_type {
        COMPRESSION_NONE => Ok(payload.to_vec()),
        COMPRESSION_LZ4 => lz4_flex::decompress_size_prepended(payload)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string())),
        COMPRESSION_SNAPPY => {
            if payload.len() < 4 {
                return Err(invalid_data("snappy block too short"));
            }
            let raw_len = u32::from_le_bytes(payload[0..4].try_into().unwrap()) as usize;
            let mut decoder = snap::raw::Decoder::new();
            let mut out = vec![0u8; raw_len];
            let n = decoder
                .decompress(&payload[4..], &mut out)
                .map_err(|e| invalid_data(format!("snappy decode: {e}")))?;
            if n != raw_len {
                return Err(invalid_data("snappy decoded length mismatch"));
            }
            out.truncate(n);
            Ok(out)
        }
        _ => Err(invalid_data(format!(
            "unknown compression type: {compression_type}"
        ))),
    }
}

fn read_file_region(
    file: &dyn ReadFile,
    offset: u64,
    size: u64,
    data_end: u64,
    name: &'static str,
) -> io::Result<Vec<u8>> {
    validate_file_region(offset, size, data_end, name)?;
    let len = usize::try_from(size)
        .map_err(|_| invalid_data(format!("{name} is too large to address")))?;
    let mut buf = vec![0u8; len];
    file.read_exact_at(offset, &mut buf)?;
    Ok(buf)
}

/// `data_end` is the first byte past the region area: the file size less
/// the footer, whose length depends on the format version.
fn validate_file_region(
    offset: u64,
    size: u64,
    data_end: u64,
    name: &'static str,
) -> io::Result<()> {
    let end = offset
        .checked_add(size)
        .ok_or_else(|| invalid_data(format!("{name} range overflows")))?;
    if end > data_end {
        return Err(invalid_data(format!("{name} extends past SSTable data")));
    }
    if usize::try_from(size).is_err() {
        return Err(invalid_data(format!("{name} is too large to address")));
    }
    Ok(())
}

// ─── Writer ─────────────────────────────────────────────────────────────────

/// Builds an SSTable file from sorted **internal keys**. Callers are
/// responsible for supplying keys in ascending internal-key order (newer
/// versions of a user key appear before older ones).
pub(crate) struct SsTableWriter {
    writer: BufferedWriter,
    path: PathBuf,
    env: Arc<dyn Env>,
    block_builder: BlockBuilder,
    index_entries: Vec<(Vec<u8>, BlockHandle)>,
    bloom_builder: BloomFilterBuilder,
    prefix_bloom_builder: Option<BloomFilterBuilder>,
    prefix_extractor: Option<Arc<dyn PrefixExtractor>>,
    /// Which key the prefix bloom's entries are built from, fixed when the
    /// table is started and recorded in its bloom region.
    prefix_form: PrefixForm,
    /// The prefix-bloom entry of the key being added, and of the key before
    /// it: the two buffers swap, so a run of one prefix is hashed once.
    prefix_entry: Vec<u8>,
    last_prefix: Option<Vec<u8>>,
    range_tombstones: Vec<RangeTombstone>,
    block_size: usize,
    current_offset: u64,
    num_entries: u64,
    newest_seq: u64,
    last_internal_key: Vec<u8>,
    smallest_user_key: Option<Vec<u8>>,
    largest_user_key: Option<Vec<u8>>,
    last_bloom_user_key: Vec<u8>,
    compression: CompressionType,
    partitioned_index: bool,
    metadata_block_size: usize,
    /// Set when the table is sealed: every region is written as a frame
    /// under this key and salt.
    seal: Option<TableSeal>,
    /// A sealed block's frame, reused from one block to the next.
    frame: Vec<u8>,
    /// The deletions among the entries added so far.
    deletions: DeletionCounter,
}

impl SsTableWriter {
    /// Start writing an SSTable at `path` on `env`, sealed under the
    /// current key of `keyring` when one is given.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_in(
        env: &Arc<dyn Env>,
        path: &Path,
        block_size: usize,
        bloom_bits_per_key: usize,
        compression: CompressionType,
        prefix_extractor: Option<Arc<dyn PrefixExtractor>>,
        partitioned_index: bool,
        metadata_block_size: usize,
        keyring: Option<&Keyring>,
    ) -> io::Result<Self> {
        let seal = keyring.map(TableSeal::fresh).transpose()?;
        let file = env.open_write(path, WriteMode::Truncate)?;
        let prefix_bloom_builder = prefix_extractor
            .as_ref()
            .map(|_| BloomFilterBuilder::new(bloom_bits_per_key));
        Ok(Self {
            writer: BufferedWriter::new(file),
            path: path.to_path_buf(),
            env: Arc::clone(env),
            block_builder: BlockBuilder::new(RESTART_INTERVAL),
            index_entries: Vec::new(),
            bloom_builder: BloomFilterBuilder::new(bloom_bits_per_key),
            prefix_bloom_builder,
            prefix_extractor,
            prefix_form: prefix_call::form_to_write(),
            prefix_entry: Vec::new(),
            last_prefix: None,
            range_tombstones: Vec::new(),
            block_size,
            current_offset: 0,
            num_entries: 0,
            newest_seq: 0,
            last_internal_key: Vec::new(),
            smallest_user_key: None,
            largest_user_key: None,
            last_bloom_user_key: Vec::new(),
            compression,
            partitioned_index,
            metadata_block_size,
            seal,
            frame: Vec::new(),
            deletions: DeletionCounter::default(),
        })
    }

    /// Tell the writer the range tombstones this table will carry, before
    /// its first entry, so the entries they hide are counted as they are
    /// added. Whoever writes a table knows them up front: a flush has its
    /// memtable's, a compaction the ones it carries on. Said or not, the
    /// tombstones themselves are attached with
    /// [`Self::add_range_tombstone`].
    pub(crate) fn hidden_by(&mut self, tombstones: Arc<RangeTombstoneSet>) {
        self.deletions.hidden_by(tombstones);
    }

    /// Start writing an SSTable through the standard environment.
    #[cfg(test)]
    pub(crate) fn new(
        path: &Path,
        block_size: usize,
        bloom_bits_per_key: usize,
        compression: CompressionType,
        prefix_extractor: Option<Arc<dyn PrefixExtractor>>,
        partitioned_index: bool,
        metadata_block_size: usize,
    ) -> io::Result<Self> {
        Self::new_in(
            &crate::env::std_env(),
            path,
            block_size,
            bloom_bits_per_key,
            compression,
            prefix_extractor,
            partitioned_index,
            metadata_block_size,
            None,
        )
    }

    /// Attach a range tombstone to the SSTable being built. Range
    /// tombstones are written into a dedicated meta block between the
    /// data blocks and the bloom block, so they're independent of the
    /// point-entry stream and do not affect `add`'s sort-order
    /// invariant.
    pub(crate) fn add_range_tombstone(&mut self, start: &[u8], end: &[u8], seq: u64) {
        self.newest_seq = self.newest_seq.max(seq);
        self.range_tombstones
            .push(RangeTombstone::new(start.to_vec(), end.to_vec(), seq));
    }

    /// Add an `(internal_key, value)` pair. Internal keys must arrive in
    /// sorted order. For tombstones, pass an empty value.
    pub(crate) fn add(&mut self, internal_key: &[u8], value: &[u8]) -> io::Result<()> {
        let (user_key, seq, value_type) = decode_internal_key(internal_key);
        self.newest_seq = self.newest_seq.max(seq);
        let first_of_key = user_key != self.last_bloom_user_key.as_slice();
        self.deletions.see(user_key, seq, value_type, first_of_key);

        // Only add each distinct user key to the bloom filter once.
        if first_of_key {
            self.bloom_builder.add_key(user_key);
            self.last_bloom_user_key = user_key.to_vec();

            // Prefix bloom: keys arrive in sorted user-key order, so
            // distinct prefixes form contiguous runs. Only hash each
            // distinct prefix once.
            if let (Some(extractor), Some(builder)) = (
                self.prefix_extractor.as_deref(),
                self.prefix_bloom_builder.as_mut(),
            ) && prefix_call::entry(
                self.prefix_form,
                extractor,
                user_key,
                &mut self.prefix_entry,
            ) && self.last_prefix.as_deref() != Some(self.prefix_entry.as_slice())
            {
                builder.add_key(&self.prefix_entry);
                std::mem::swap(
                    self.last_prefix.get_or_insert_default(),
                    &mut self.prefix_entry,
                );
            }
        }

        self.block_builder.add(internal_key, value);
        self.last_internal_key = internal_key.to_vec();
        self.num_entries += 1;

        if self.smallest_user_key.is_none() {
            self.smallest_user_key = Some(user_key.to_vec());
        }
        match &mut self.largest_user_key {
            Some(existing) if existing.as_slice() == user_key => {}
            slot => *slot = Some(user_key.to_vec()),
        }

        if self.block_builder.estimated_size() >= self.block_size {
            self.flush_block()?;
        }

        Ok(())
    }

    /// Finalize the SSTable. Returns `None` only if **nothing** was
    /// added - no point entries and no range tombstones. A file with
    /// only range tombstones (and no point entries) is still a valid
    /// SSTable; its smallest/largest user key range is derived from
    /// the tombstone bounds instead of point-entry bounds.
    pub(crate) fn finish(mut self) -> io::Result<Option<SsTableWriteSummary>> {
        if self.num_entries == 0 && self.range_tombstones.is_empty() {
            return Ok(None);
        }

        if !self.block_builder.is_empty() {
            self.flush_block()?;
        }

        // Range tombstone meta block comes right after the data blocks
        // and before the bloom filter. A size of 0 means no tombstones.
        let range_tombstone_set =
            RangeTombstoneSet::from_vec(std::mem::take(&mut self.range_tombstones));
        let range_tombstone_data = if range_tombstone_set.is_empty() {
            Vec::new()
        } else {
            encode_range_tombstone_block(range_tombstone_set.as_slice())
        };
        let range_tombstone_offset = self.current_offset;
        let range_tombstone_size = if range_tombstone_data.is_empty() {
            0
        } else {
            write_meta_region(
                &mut self.writer,
                &mut self.current_offset,
                &range_tombstone_data,
                checksum::META_KIND_RANGE_TOMBSTONE,
                self.seal.as_ref(),
            )?
        };

        // Bloom region layout (see `filter_block`):
        //   [prefix section len: 7 bytes LE][prefix form: u8][prefix section][user_key_bloom_bytes]
        //
        // The prefix section names the extractor that built the prefix
        // bloom, then holds the bloom. A length of 0 means "no prefix
        // bloom" - the file was built without a prefix extractor. The
        // header is written unconditionally, so the reader needs no
        // separate case for it.
        let prefix_bloom_data = match (self.prefix_bloom_builder.take(), &self.prefix_extractor) {
            (Some(builder), Some(extractor)) => prefix_section(
                self.prefix_form,
                extractor.name(),
                encode_bloom_block(&builder.build()),
            ),
            _ => Vec::new(),
        };
        let user_bloom = self.bloom_builder.build();
        let user_bloom_data = encode_bloom_block(&user_bloom);

        let prefix_header = prefix_header(self.prefix_form, prefix_bloom_data.len())?;
        let mut bloom_region = Vec::with_capacity(
            prefix_header.len() + prefix_bloom_data.len() + user_bloom_data.len(),
        );
        bloom_region.extend_from_slice(&prefix_header);
        bloom_region.extend_from_slice(&prefix_bloom_data);
        bloom_region.extend_from_slice(&user_bloom_data);
        let bloom_offset = self.current_offset;
        let bloom_size = write_meta_region(
            &mut self.writer,
            &mut self.current_offset,
            &bloom_region,
            checksum::META_KIND_BLOOM,
            self.seal.as_ref(),
        )?;

        let (index_offset, index_size, magic) = if self.partitioned_index {
            // Partition the flat index entries into leaf sub-blocks whose
            // serialized size stays within `metadata_block_size`, write
            // each leaf as a raw encoded index block, and build a
            // top-level index pointing to each leaf.
            let mut top_level: Vec<(Vec<u8>, BlockHandle)> = Vec::new();
            let mut chunk_start = 0usize;
            while chunk_start < self.index_entries.len() {
                let mut chunk_end = chunk_start + 1;
                while chunk_end < self.index_entries.len() {
                    let candidate = &self.index_entries[chunk_start..chunk_end + 1];
                    let size = encoded_index_block_size(candidate);
                    if size > self.metadata_block_size {
                        break;
                    }
                    chunk_end += 1;
                }
                let chunk = &self.index_entries[chunk_start..chunk_end];
                let leaf_data = encode_index_block(chunk);
                let last_key = match chunk.last() {
                    Some((key, _)) => key.clone(),
                    None => return Err(invalid_data("index partitioning produced an empty leaf")),
                };
                let leaf_offset = self.current_offset;
                let leaf_size = write_meta_region(
                    &mut self.writer,
                    &mut self.current_offset,
                    &leaf_data,
                    checksum::META_KIND_INDEX_LEAF,
                    self.seal.as_ref(),
                )?;

                top_level.push((
                    last_key,
                    BlockHandle {
                        offset: leaf_offset,
                        size: leaf_size,
                    },
                ));
                chunk_start = chunk_end;
            }
            let top_data = encode_index_block(&top_level);
            let top_offset = self.current_offset;
            let top_size = write_meta_region(
                &mut self.writer,
                &mut self.current_offset,
                &top_data,
                checksum::META_KIND_INDEX,
                self.seal.as_ref(),
            )?;
            let magic = if self.seal.is_some() {
                MAGIC_V8
            } else {
                MAGIC_V6
            };
            (top_offset, top_size, magic)
        } else {
            let index_data = encode_index_block(&self.index_entries);
            let idx_offset = self.current_offset;
            let idx_size = write_meta_region(
                &mut self.writer,
                &mut self.current_offset,
                &index_data,
                checksum::META_KIND_INDEX,
                self.seal.as_ref(),
            )?;
            let magic = if self.seal.is_some() {
                MAGIC_V7
            } else {
                MAGIC_V5
            };
            (idx_offset, idx_size, magic)
        };

        let footer = Footer {
            range_tombstone_offset,
            range_tombstone_size,
            bloom_offset,
            bloom_size,
            index_offset,
            index_size,
            num_entries: self.num_entries,
            magic,
            sealed: None,
        };
        self.writer.write_all(&footer.encode(self.seal.as_ref())?)?;
        self.writer.sync_all()?;
        crate::env::sync_parent_dir(&*self.env, &self.path)?;

        let points = self
            .smallest_user_key
            .take()
            .zip(self.largest_user_key.take());
        let (smallest_user_key, largest_user_key) =
            table_key_range(points, range_tombstone_set.iter()).expect("checked non-empty above");

        Ok(Some(SsTableWriteSummary {
            newest_seq: self.newest_seq,
            smallest_user_key,
            largest_user_key,
            num_entries: self.num_entries,
            deletions: self.deletions.counted(),
        }))
    }

    fn flush_block(&mut self) -> io::Result<()> {
        let last_internal = self.last_internal_key.clone();
        let block_builder =
            std::mem::replace(&mut self.block_builder, BlockBuilder::new(RESTART_INTERVAL));
        let raw_data = block_builder.finish();

        let block_offset = self.current_offset;

        let (codec_byte, payload) = match block_codec::block_codec(self.compression, raw_data.len())
        {
            CompressionType::None => (COMPRESSION_NONE, raw_data.clone()),
            CompressionType::Lz4 => (COMPRESSION_LZ4, lz4_flex::compress_prepend_size(&raw_data)),
            CompressionType::Snappy => {
                let mut encoder = snap::raw::Encoder::new();
                let compressed = encoder.compress_vec(&raw_data).map_err(|e| {
                    io::Error::new(io::ErrorKind::InvalidData, format!("snappy encode: {e}"))
                })?;
                // Prepend the original length so the reader can pre-size
                // its decode buffer - matches `lz4_flex::compress_prepend_size`.
                let mut framed = Vec::with_capacity(4 + compressed.len());
                framed.extend_from_slice(&(raw_data.len() as u32).to_le_bytes());
                framed.extend_from_slice(&compressed);
                (COMPRESSION_SNAPPY, framed)
            }
        };

        let total = if let Some(seal) = &self.seal {
            self.frame.clear();
            seal.seal_data_block(block_offset, codec_byte, &payload, &mut self.frame)?;
            self.writer.write_all(&self.frame)?;
            self.frame.len()
        } else {
            let checksum = checksum::sst_block(codec_byte, &payload);
            self.writer.write_all(&[codec_byte])?;
            self.writer.write_all(&payload)?;
            self.writer.write_all(&checksum.to_le_bytes())?;
            1 + payload.len() + 4
        };
        self.current_offset += total as u64;

        let block_size = self.current_offset - block_offset;
        self.index_entries.push((
            last_internal,
            BlockHandle {
                offset: block_offset,
                size: block_size,
            },
        ));

        Ok(())
    }
}

// ─── Reader ─────────────────────────────────────────────────────────────────

/// Reads an SSTable file.
///
/// The file handle is held for the reader's lifetime so a concurrent
/// compaction unlinking the path cannot corrupt an in-progress read
/// (the OS keeps the bytes alive via file-descriptor refcounting).
///
/// # What this reader keeps resident
///
/// Under [`MetadataPolicy::Pinned`] the index and the filter region are
/// decoded at open and held for the reader's life, outside the block
/// cache's byte budget. Under [`MetadataPolicy::Cached`] they are still
/// validated at open but not retained: they are fetched through the
/// block cache on demand, so `Options::block_cache_size` bounds them.
///
/// A partitioned file's index **leaves** always go through the block
/// cache regardless of the policy. Only the top-level index is pinned,
/// and it holds one entry per leaf.
///
/// [`SsTableReader::pinned_metadata_bytes`] reports exactly what this
/// reader is holding outside the cache budget.
pub(crate) struct SsTableReader {
    /// Positional reads, so concurrent readers need no lock between them.
    file: Box<dyn ReadFile>,
    /// First byte past the region area: the file size less the footer.
    /// Every region bound is checked against this, so a damaged offset
    /// can never send a read into the footer or past the file.
    data_end: u64,
    pub(crate) file_id: u64,
    /// The flat index of a V1 file, or the top-level index of a V2
    /// file. A V2 top-level index is always `Pinned`.
    index: MetaSlot<IndexBlock>,
    /// The user-key bloom filter plus the optional prefix bloom.
    filter: MetaSlot<FilterBlock>,
    /// Set only when the block cache refused a metadata insert, which
    /// happens under `strict_capacity_limit` with an entry larger than
    /// one shard. Pinning the block is the right answer there: the
    /// alternative is re-reading it from disk on every lookup. The
    /// overshoot is visible through `pinned_metadata_bytes`.
    index_fallback: OnceLock<Arc<IndexBlock>>,
    filter_fallback: OnceLock<Arc<FilterBlock>>,
    range_tombstones: RangeTombstoneSet,
    /// `true` when the file was written with `MAGIC_V6` or `MAGIC_V8`
    /// (partitioned index). `self.index` then holds only the compact
    /// top-level entries; each entry's `handle` points to a leaf sub-block
    /// read via [`SsTableReader::read_index_leaf`].
    partitioned: bool,
    /// Set when the table is sealed: every region read from it is opened
    /// under this key and salt instead of checked against a checksum.
    seal: Option<TableSeal>,
    /// The sequence every entry reads at, for an ingested table. See the
    /// `global_seq` module for the seams that apply it.
    global_seq: Option<u64>,
    /// This reader's own handle, set when its table goes live
    /// ([`LiveSst::new`]). A `CacheOnly` miss hands it to the unit that
    /// reads the block later, on whichever thread claims it.
    me: OnceLock<Weak<SsTableReader>>,
    /// Number of index leaves actually read from disk, i.e. block-cache
    /// misses. Cached leaf hits do not count.
    #[cfg(test)]
    index_leaf_reads: AtomicUsize,
}

pub(crate) struct SsTableInternalIter<'a> {
    reader: &'a SsTableReader,
    cache: &'a BlockCache,
    next_cursor: Option<SsTableBlockCursor>,
    current_block: Option<Arc<Block>>,
    current_pos: usize,
    current_key: Vec<u8>,
}

impl<'a> SsTableInternalIter<'a> {
    pub(crate) fn next_entry(&mut self) -> io::Result<Option<(Vec<u8>, Vec<u8>)>> {
        loop {
            if let Some(block) = &self.current_block {
                let data = block.entry_data();
                if self.current_pos < data.len() {
                    let (consumed, value_offset, value_len) =
                        decode_entry_at(data, self.current_pos, &mut self.current_key);
                    self.current_pos += consumed;
                    let value = data[value_offset..value_offset + value_len].to_vec();
                    return Ok(Some((block.owned_key(&self.current_key), value)));
                }
            }

            let Some(cursor) = self.next_cursor.take() else {
                return Ok(None);
            };
            self.next_cursor = self.reader.next_block_cursor(&cursor, self.cache)?;

            let block = self.reader.load_block_at_cursor(&cursor, self.cache)?;
            self.current_block = Some(block);
            self.current_pos = 0;
            self.current_key.clear();
        }
    }
}

impl SsTableReader {
    /// Open an SSTable file, pinning its index and filter.
    ///
    /// This is [`MetadataPolicy::Pinned`]: the same residency the
    /// reader has always had. Callers that want the block cache to
    /// bound index and filter bytes use [`SsTableReader::open_with`].
    #[cfg(test)]
    pub(crate) fn open(path: &Path, file_id: u64) -> io::Result<Self> {
        Self::open_with(
            &crate::env::std_env(),
            path,
            file_id,
            MetadataPolicy::Pinned,
            None,
        )
    }

    /// Open an SSTable file under an explicit metadata policy.
    ///
    /// The index and the filter region are read and decoded here under
    /// both policies, so a corrupt index or filter fails the open
    /// rather than surfacing on a later read. Under
    /// [`MetadataPolicy::Cached`] the decoded blocks are then dropped
    /// and re-fetched through the block cache on first use, which is
    /// what puts them inside the configured budget.
    ///
    /// A sealed table opens only through `keyring`, under the key its
    /// footer names: with no keyring it refuses with
    /// [`crate::Error::KeyProviderRequired`], and a key the keyring does
    /// not provide refuses with [`crate::Error::UnknownKey`].
    pub(crate) fn open_with(
        env: &Arc<dyn Env>,
        path: &Path,
        file_id: u64,
        policy: MetadataPolicy,
        keyring: Option<&Keyring>,
    ) -> io::Result<Self> {
        let file = env.open_read(path)?;
        let file_size = file.len()?;

        let footer = read_footer(&*file, file_size)?;
        let data_end = file_size - footer.size() as u64;
        validate_footer_regions(&footer, data_end)?;
        let seal = footer.open_seal(keyring, path)?;

        let filter_handle = BlockHandle {
            offset: footer.bloom_offset,
            size: footer.bloom_size,
        };
        let filter_region = read_file_region(
            &*file,
            filter_handle.offset,
            filter_handle.size,
            data_end,
            "bloom region",
        )?;
        let filter_block = FilterBlock::decode(&unframe_meta_region(
            seal.as_ref(),
            filter_region,
            filter_handle.offset,
            checksum::META_KIND_BLOOM,
            file_id,
            "bloom region",
        )?)?;
        let filter = match policy {
            MetadataPolicy::Pinned => MetaSlot::Pinned(Arc::new(filter_block)),
            MetadataPolicy::Cached => MetaSlot::Cached(filter_handle),
        };

        let index_handle = BlockHandle {
            offset: footer.index_offset,
            size: footer.index_size,
        };
        let index_region = read_file_region(
            &*file,
            index_handle.offset,
            index_handle.size,
            data_end,
            "index block",
        )?;
        // `IndexBlock::decode` takes the buffer by value, so the verified
        // payload is truncated in place rather than copied out.
        let index_block = IndexBlock::decode(unframe_meta_region(
            seal.as_ref(),
            index_region,
            index_handle.offset,
            checksum::META_KIND_INDEX,
            file_id,
            "index block",
        )?)?;

        let range_tombstones = if footer.range_tombstone_size == 0 {
            Vec::new()
        } else {
            let rt_region = read_file_region(
                &*file,
                footer.range_tombstone_offset,
                footer.range_tombstone_size,
                data_end,
                "range tombstone block",
            )?;
            decode_range_tombstone_block(&unframe_meta_region(
                seal.as_ref(),
                rt_region,
                footer.range_tombstone_offset,
                checksum::META_KIND_RANGE_TOMBSTONE,
                file_id,
                "range tombstone block",
            )?)?
        };

        let partitioned = Footer::magic_is_partitioned(footer.magic);
        // A partitioned file's top-level index is the entry point to
        // every leaf read, so it stays pinned under both policies.
        let index = if partitioned || policy == MetadataPolicy::Pinned {
            MetaSlot::Pinned(Arc::new(index_block))
        } else {
            MetaSlot::Cached(index_handle)
        };

        Ok(Self {
            file,
            data_end,
            file_id,
            index,
            filter,
            index_fallback: OnceLock::new(),
            filter_fallback: OnceLock::new(),
            range_tombstones: RangeTombstoneSet::from_vec(range_tombstones),
            partitioned,
            seal,
            global_seq: None,
            me: OnceLock::new(),
            #[cfg(test)]
            index_leaf_reads: AtomicUsize::new(0),
        })
    }

    /// The key this table is sealed under, or `None` when it is not sealed.
    pub(crate) fn seal_key(&self) -> Option<crate::KeyId> {
        self.seal.as_ref().map(TableSeal::key_id)
    }

    /// Bytes a data block's frame carries beside its payload, so the
    /// smallest frame a handle can name.
    fn min_block_frame(&self) -> u64 {
        if self.seal.is_some() {
            OVERHEAD as u64 + 1
        } else {
            5
        }
    }

    /// This reader, reading every entry and range tombstone of the file at
    /// `seq` from now on (an ingested table), or at the sequences the file
    /// stores (`None`).
    ///
    /// Bind before the first data block is read under this reader's file
    /// id: a block is stamped with the sequence as it enters the block
    /// cache, so one cached before the bind would be served at the stored
    /// sequences. The file must hold at most one entry per user key.
    pub(crate) fn with_global_seq(mut self, seq: Option<u64>) -> Self {
        if let Some(seq) = seq {
            self.range_tombstones = RangeTombstoneSet::from_vec(global_seq::tombstones_at(
                self.range_tombstones.as_slice(),
                seq,
            ));
        }
        self.global_seq = seq;
        self
    }

    /// This reader under another file id, the one the block cache keys its
    /// blocks by from now on. An ingest validates a file under a probe id
    /// and installs it under the id it allocated, without reading the
    /// index and filter a second time.
    pub(crate) fn rebind(mut self, file_id: u64) -> Self {
        self.file_id = file_id;
        self
    }

    /// Whether every entry of this table is newer than `snapshot_seq`, so
    /// a lookup at that snapshot finds nothing here without reading it.
    fn hidden_at(&self, snapshot_seq: u64) -> bool {
        self.global_seq.is_some_and(|seq| seq > snapshot_seq)
    }

    /// Where an index seek for `target` lands among `index`'s entries,
    /// steered for an ingested table (see `global_seq::steer`).
    fn index_seek(&self, index: &IndexBlock, target: &[u8]) -> usize {
        match self
            .global_seq
            .and_then(|seq| global_seq::steer(target, seq))
        {
            Some((user_key, trailer)) => index.seek_split(user_key, &trailer),
            None => index.seek(target),
        }
    }

    /// Bytes this reader holds outside the block cache's budget: its
    /// pinned index, its pinned filter, any block the cache refused,
    /// and its range tombstones.
    ///
    /// Summed across a version's files this is the engine's
    /// `regolith.pinned-metadata-bytes` property, which is what makes the
    /// residency this reader is responsible for observable instead of
    /// merely asserted.
    pub(crate) fn pinned_metadata_bytes(&self) -> usize {
        let index = match &self.index {
            MetaSlot::Pinned(block) => block.charge(),
            MetaSlot::Cached(_) => 0,
        };
        let filter = match &self.filter {
            MetaSlot::Pinned(block) => block.charge(),
            MetaSlot::Cached(_) => 0,
        };
        let index_refused = self.index_fallback.get().map_or(0, |block| block.charge());
        let filter_refused = self.filter_fallback.get().map_or(0, |block| block.charge());
        let tombstones: usize = self
            .range_tombstones
            .as_slice()
            .iter()
            .map(|rt| {
                std::mem::size_of::<RangeTombstone>() + rt.start.capacity() + rt.end.capacity()
            })
            .sum();
        index + filter + index_refused + filter_refused + tombstones
    }

    /// The flat or top-level index, from wherever this reader keeps it.
    fn index(&self, cache: &BlockCache) -> io::Result<MetaRef<'_, IndexBlock>> {
        let handle = match &self.index {
            MetaSlot::Pinned(block) => return Ok(MetaRef::Borrowed(block)),
            MetaSlot::Cached(handle) => *handle,
        };
        if let Some(block) = self.index_fallback.get() {
            return Ok(MetaRef::Borrowed(block));
        }
        if let Some(block) = cache.get_index(self.file_id, handle.offset) {
            return Ok(MetaRef::Owned(block));
        }
        if let Some(landed) = self.queue_miss(cache, handle, None, |reader| {
            Box::new(move |cache| reader.load_index(handle, cache).map(Landed::Index))
        }) {
            return Ok(MetaRef::Owned(landed?.into_index()?));
        }
        self.load_index(handle, cache).map(MetaRef::Owned)
    }

    /// Read a cached flat index from the device, into the block cache, or
    /// pinned when the cache refuses it.
    fn load_index(&self, handle: BlockHandle, cache: &BlockCache) -> io::Result<Arc<IndexBlock>> {
        let data = self.read_metadata_region(handle, checksum::META_KIND_INDEX, "index block")?;
        let block = Arc::new(IndexBlock::decode(data)?);
        if !cache.insert_index(self.file_id, handle.offset, Arc::clone(&block)) {
            let _ = self.index_fallback.set(Arc::clone(&block));
        }
        Ok(block)
    }

    /// The filter region, from wherever this reader keeps it.
    fn filter(&self, cache: &BlockCache) -> io::Result<MetaRef<'_, FilterBlock>> {
        let handle = match &self.filter {
            MetaSlot::Pinned(block) => return Ok(MetaRef::Borrowed(block)),
            MetaSlot::Cached(handle) => *handle,
        };
        if let Some(block) = self.filter_fallback.get() {
            return Ok(MetaRef::Borrowed(block));
        }
        if let Some(block) = cache.get_filter(self.file_id, handle.offset) {
            return Ok(MetaRef::Owned(block));
        }
        if let Some(landed) = self.queue_miss(cache, handle, None, |reader| {
            Box::new(move |cache| reader.load_filter(handle, cache).map(Landed::Filter))
        }) {
            return Ok(MetaRef::Owned(landed?.into_filter()?));
        }
        self.load_filter(handle, cache).map(MetaRef::Owned)
    }

    /// Read a cached filter region from the device, into the block cache, or
    /// pinned when the cache refuses it.
    fn load_filter(&self, handle: BlockHandle, cache: &BlockCache) -> io::Result<Arc<FilterBlock>> {
        let region =
            self.read_metadata_region(handle, checksum::META_KIND_BLOOM, "bloom region")?;
        let block = Arc::new(FilterBlock::decode(&region)?);
        if !cache.insert_filter(self.file_id, handle.offset, Arc::clone(&block)) {
            let _ = self.filter_fallback.set(Arc::clone(&block));
        }
        Ok(block)
    }

    /// The device seam. The block cache just missed `handle`: in a
    /// `CacheOnly` scope, hand the read to the scope's queue, and return
    /// what landed for it or the `WouldBlock` that records it; `None` in a
    /// `Blocking` scope, whose caller reads the device itself. `work` makes
    /// the read the unit runs later, from this reader's own handle.
    fn queue_miss(
        &self,
        cache: &BlockCache,
        handle: BlockHandle,
        guard: Option<usize>,
        work: impl FnOnce(Arc<SsTableReader>) -> Work,
    ) -> Option<io::Result<Landed>> {
        let queue = scope::current()?;
        let key = UnitKey {
            file_id: self.file_id,
            offset: handle.offset,
            guard,
        };
        let bytes = usize::try_from(handle.size).unwrap_or(usize::MAX);
        Some(cache.io().miss(queue, key, bytes, || {
            let reader = self.me.get().and_then(Weak::upgrade).ok_or_else(|| {
                io::Error::other("this table cannot be read through an I/O queue")
            })?;
            Ok(work(reader))
        }))
    }

    /// Read one metadata region, bounded by the start of the footer and
    /// verified against its checksum trailer.
    ///
    /// Both halves matter: `data_end` keeps a region from running into
    /// the footer, and the trailer is what makes a corrupt index, filter
    /// or leaf fail its read instead of surfacing as a wrong answer. The
    /// payload is truncated in place, so verifying costs no copy.
    fn read_metadata_region(
        &self,
        handle: BlockHandle,
        kind: u8,
        name: &'static str,
    ) -> io::Result<Vec<u8>> {
        let region =
            read_file_region(&*self.file, handle.offset, handle.size, self.data_end, name)?;
        unframe_meta_region(
            self.seal.as_ref(),
            region,
            handle.offset,
            kind,
            self.file_id,
            name,
        )
    }

    /// Read a leaf index sub-block, through the block cache.
    ///
    /// Used when `self.partitioned` is true; the `handle` comes from
    /// one of the top-level index entries. Leaves are charged against
    /// `Options::block_cache_size` and are evictable, which is what
    /// bounds a partitioned file's index memory; before this they were
    /// re-read *and* re-decoded from disk on every seek.
    fn read_index_leaf(
        &self,
        handle: BlockHandle,
        cache: &BlockCache,
    ) -> io::Result<Arc<IndexBlock>> {
        if let Some(leaf) = cache.get_index(self.file_id, handle.offset) {
            return Ok(leaf);
        }
        if let Some(landed) = self.queue_miss(cache, handle, None, |reader| {
            Box::new(move |cache| reader.read_index_leaf(handle, cache).map(Landed::Index))
        }) {
            return landed?.into_index();
        }

        #[cfg(test)]
        self.index_leaf_reads.fetch_add(1, Ordering::Relaxed);

        let buf = self.read_metadata_region(
            handle,
            checksum::META_KIND_INDEX_LEAF,
            "partitioned index leaf",
        )?;
        let leaf = Arc::new(IndexBlock::decode(buf)?);
        cache.insert_index(self.file_id, handle.offset, Arc::clone(&leaf));
        Ok(leaf)
    }

    #[cfg(test)]
    fn index_leaf_read_count(&self) -> usize {
        self.index_leaf_reads.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn reset_index_leaf_read_count(&self) {
        self.index_leaf_reads.store(0, Ordering::Relaxed);
    }

    /// Number of entries in the pinned index. For a partitioned file
    /// this is the leaf count; for a flat file it is the data-block
    /// count. `None` when the index is not pinned.
    #[cfg(test)]
    fn pinned_index_len(&self) -> Option<usize> {
        match &self.index {
            MetaSlot::Pinned(block) => Some(block.len()),
            MetaSlot::Cached(_) => None,
        }
    }

    fn cursor_from_leaf(
        &self,
        leaf_idx: usize,
        entry_idx: usize,
        leaf: Arc<IndexBlock>,
    ) -> Option<SsTableBlockCursor> {
        if entry_idx >= leaf.len() {
            return None;
        }
        Some(SsTableBlockCursor::Partitioned {
            leaf_idx,
            entry_idx,
            leaf,
        })
    }

    pub(crate) fn first_block_cursor(
        &self,
        cache: &BlockCache,
    ) -> io::Result<Option<SsTableBlockCursor>> {
        let index = self.index(cache)?;
        if !self.partitioned {
            return Ok((!index.is_empty()).then_some(SsTableBlockCursor::Flat(0)));
        }

        for leaf_idx in 0..index.len() {
            let Some(handle) = index.handle(leaf_idx) else {
                continue;
            };
            let leaf = self.read_index_leaf(handle, cache)?;
            if let Some(cursor) = self.cursor_from_leaf(leaf_idx, 0, leaf) {
                return Ok(Some(cursor));
            }
        }
        Ok(None)
    }

    pub(crate) fn last_block_cursor(
        &self,
        cache: &BlockCache,
    ) -> io::Result<Option<SsTableBlockCursor>> {
        let index = self.index(cache)?;
        if !self.partitioned {
            return Ok(index.len().checked_sub(1).map(SsTableBlockCursor::Flat));
        }

        for leaf_idx in (0..index.len()).rev() {
            let Some(handle) = index.handle(leaf_idx) else {
                continue;
            };
            let leaf = self.read_index_leaf(handle, cache)?;
            if !leaf.is_empty() {
                let entry_idx = leaf.len() - 1;
                return Ok(self.cursor_from_leaf(leaf_idx, entry_idx, leaf));
            }
        }
        Ok(None)
    }

    pub(crate) fn seek_block_cursor(
        &self,
        target: &[u8],
        cache: &BlockCache,
    ) -> io::Result<Option<SsTableBlockCursor>> {
        let index = self.index(cache)?;
        let mut leaf_idx = self.index_seek(&index, target);
        if leaf_idx >= index.len() {
            return Ok(None);
        }
        if !self.partitioned {
            return Ok(Some(SsTableBlockCursor::Flat(leaf_idx)));
        }

        while leaf_idx < index.len() {
            let Some(handle) = index.handle(leaf_idx) else {
                break;
            };
            let leaf = self.read_index_leaf(handle, cache)?;
            let entry_idx = self.index_seek(&leaf, target);
            if let Some(cursor) = self.cursor_from_leaf(leaf_idx, entry_idx, leaf) {
                return Ok(Some(cursor));
            }
            leaf_idx += 1;
        }
        Ok(None)
    }

    pub(crate) fn next_block_cursor(
        &self,
        cursor: &SsTableBlockCursor,
        cache: &BlockCache,
    ) -> io::Result<Option<SsTableBlockCursor>> {
        match cursor {
            SsTableBlockCursor::Flat(idx) => {
                let next = idx + 1;
                let index = self.index(cache)?;
                Ok((next < index.len()).then_some(SsTableBlockCursor::Flat(next)))
            }
            SsTableBlockCursor::Partitioned {
                leaf_idx,
                entry_idx,
                leaf,
            } => {
                let next_entry = entry_idx + 1;
                if next_entry < leaf.len() {
                    return Ok(Some(SsTableBlockCursor::Partitioned {
                        leaf_idx: *leaf_idx,
                        entry_idx: next_entry,
                        leaf: Arc::clone(leaf),
                    }));
                }
                let index = self.index(cache)?;
                for next_leaf_idx in leaf_idx + 1..index.len() {
                    let Some(handle) = index.handle(next_leaf_idx) else {
                        continue;
                    };
                    let next_leaf = self.read_index_leaf(handle, cache)?;
                    if let Some(cursor) = self.cursor_from_leaf(next_leaf_idx, 0, next_leaf) {
                        return Ok(Some(cursor));
                    }
                }
                Ok(None)
            }
        }
    }

    pub(crate) fn prev_block_cursor(
        &self,
        cursor: &SsTableBlockCursor,
        cache: &BlockCache,
    ) -> io::Result<Option<SsTableBlockCursor>> {
        match cursor {
            SsTableBlockCursor::Flat(idx) => {
                if *idx == 0 {
                    Ok(None)
                } else {
                    Ok(Some(SsTableBlockCursor::Flat(idx - 1)))
                }
            }
            SsTableBlockCursor::Partitioned {
                leaf_idx,
                entry_idx,
                leaf,
            } => {
                if *entry_idx > 0 {
                    return Ok(Some(SsTableBlockCursor::Partitioned {
                        leaf_idx: *leaf_idx,
                        entry_idx: entry_idx - 1,
                        leaf: Arc::clone(leaf),
                    }));
                }
                let index = self.index(cache)?;
                for prev_leaf_idx in (0..*leaf_idx).rev() {
                    let Some(handle) = index.handle(prev_leaf_idx) else {
                        continue;
                    };
                    let prev_leaf = self.read_index_leaf(handle, cache)?;
                    if !prev_leaf.is_empty() {
                        let entry_idx = prev_leaf.len() - 1;
                        return Ok(self.cursor_from_leaf(prev_leaf_idx, entry_idx, prev_leaf));
                    }
                }
                Ok(None)
            }
        }
    }

    pub(crate) fn load_block_at_cursor(
        &self,
        cursor: &SsTableBlockCursor,
        cache: &BlockCache,
    ) -> io::Result<Arc<Block>> {
        let handle = self.cursor_handle(cursor, cache)?;
        self.read_block(handle, cache)
    }

    /// The data-block handle a cursor points at.
    pub(crate) fn cursor_handle(
        &self,
        cursor: &SsTableBlockCursor,
        cache: &BlockCache,
    ) -> io::Result<BlockHandle> {
        match cursor {
            SsTableBlockCursor::Flat(idx) => self
                .index(cache)?
                .handle(*idx)
                .ok_or_else(|| invalid_data("block index out of bounds")),
            SsTableBlockCursor::Partitioned {
                entry_idx, leaf, ..
            } => leaf
                .handle(*entry_idx)
                .ok_or_else(|| invalid_data("partitioned block index out of bounds")),
        }
    }

    /// Resolve a lookup key against the (possibly partitioned) index.
    ///
    /// Binary-searches the first data block whose last key is
    /// `>= search_key`. Returns that data block's [`BlockHandle`], or
    /// `None` if every block's last key is strictly less than
    /// `search_key`.
    ///
    /// For non-partitioned (V1) files, this is a single binary
    /// search on the in-memory `self.index`. For partitioned (V2)
    /// files, this is two binary searches: one on the top-level
    /// index, then one on the single leaf that covers `search_key`
    /// (loaded from disk on demand).
    fn find_block_handle(
        &self,
        search_key: &[u8],
        cache: &BlockCache,
    ) -> io::Result<Option<BlockHandle>> {
        self.seek_block_cursor(search_key, cache)?
            .map(|cursor| self.cursor_handle(&cursor, cache))
            .transpose()
    }

    /// Whether this SSTable *might* contain a key of the prefix seek
    /// `probe` is for, asked in the form this table's prefix bloom was
    /// built in. Returns `true` conservatively when the file was built
    /// without a prefix bloom (no negative information available).
    /// Returns `false` only when the prefix bloom is present and
    /// positively rules the prefix out.
    ///
    /// Fallible because the filter region may live in the block cache
    /// and need re-reading; under [`MetadataPolicy::Pinned`] it cannot
    /// fail.
    pub(crate) fn may_have_prefix(
        &self,
        probe: &PrefixProbe,
        cache: &BlockCache,
    ) -> io::Result<bool> {
        Ok(self.filter(cache)?.may_have_prefix(probe))
    }

    /// The form of this table's prefix bloom, when it has one.
    #[cfg(test)]
    pub(crate) fn prefix_form(&self, cache: &BlockCache) -> io::Result<Option<PrefixForm>> {
        Ok(self.filter(cache)?.prefix_form())
    }

    /// Largest seq of any range tombstone in this SSTable that covers
    /// `user_key` and is visible at `snapshot_seq`. Returns `0` when
    /// nothing covers it - `0` is safe because real seqs start at 1.
    pub(crate) fn covering_range_tombstone_seq(&self, user_key: &[u8], snapshot_seq: u64) -> u64 {
        if self.range_tombstones.is_empty() {
            return 0;
        }
        self.range_tombstones
            .max_covering_seq(user_key, snapshot_seq)
    }

    /// Borrow this SSTable's range tombstones. Used by compaction to
    /// merge them into the output file and by the iterator to honor
    /// RT coverage during scans.
    pub(crate) fn range_tombstones(&self) -> &[RangeTombstone] {
        self.range_tombstones.as_slice()
    }

    /// Whether one of this table's range tombstones covers every key of
    /// `[first, last]`.
    pub(crate) fn range_tombstone_over(&self, first: &[u8], last: &[u8]) -> bool {
        self.range_tombstones.covers_whole(first, last)
    }

    /// This table's range tombstones that reach into `[start, end)`, each
    /// cut to it.
    pub(crate) fn range_tombstones_within(&self, start: &[u8], end: &[u8]) -> Vec<RangeTombstone> {
        self.range_tombstones.clipped_overlaps(start, end)
    }

    /// Point lookup for `user_key` visible at `snapshot_seq`.
    ///
    /// Skips past merge operands (they're not valid point-lookup
    /// terminators) and returns the first `Value` / `Deletion` entry
    /// visible at the requested snapshot. A caller that cares about
    /// merge chains uses [`SsTableReader::collect_merge_chain`]
    /// instead.
    pub(crate) fn get(
        &self,
        lk: &LookupKey,
        key_buf: &mut Vec<u8>,
        cache: &BlockCache,
    ) -> io::Result<LookupResult<DbSlice>> {
        Ok(match self.probe(lk, key_buf, cache, None)? {
            None => LookupResult::NotInTable,
            Some((_, BlockHit::Tombstone { seq })) => LookupResult::FoundTombstone { seq },
            Some((
                block,
                BlockHit::Value {
                    seq,
                    value_offset,
                    value_len,
                },
            )) => {
                let value = DbSlice::from_block(block, value_offset, value_len)
                    .ok_or_else(|| invalid_data("block value extends past block"))?;
                LookupResult::Found { seq, value }
            }
        })
    }

    /// [`SsTableReader::get`] reporting the winning value's length
    /// instead of its bytes.
    ///
    /// Runs the identical bloom check, index search, block read and
    /// block scan; it only declines to take a reference on the block
    /// the value lives in, so nothing is pinned after it returns.
    pub(crate) fn get_size(
        &self,
        lk: &LookupKey,
        key_buf: &mut Vec<u8>,
        cache: &BlockCache,
        limit: Option<usize>,
    ) -> io::Result<LookupResult<usize>> {
        Ok(match self.probe(lk, key_buf, cache, limit)? {
            None => LookupResult::NotInTable,
            Some((_, BlockHit::Tombstone { seq })) => LookupResult::FoundTombstone { seq },
            Some((_, BlockHit::Value { seq, value_len, .. })) => LookupResult::Found {
                seq,
                value: value_len,
            },
        })
    }

    /// The block a point lookup for `lk` has to scan, or `None` when the
    /// bloom filter or the index rules this table out. A bloom rejection
    /// is recorded here; a positive is recorded by the caller once its
    /// scan finds the key, since only the scan knows.
    fn candidate_block(
        &self,
        lk: &LookupKey,
        cache: &BlockCache,
        limit: Option<usize>,
    ) -> io::Result<Option<Arc<Block>>> {
        if self.hidden_at(lk.snapshot_seq()) {
            return Ok(None);
        }
        if !self.filter(cache)?.may_contain(lk.prefixed_user_key()) {
            cache.record_bloom_useful();
            return Ok(None);
        }
        let Some(handle) = self.find_block_handle(lk.internal(), cache)? else {
            return Ok(None);
        };
        self.read_block_with_limit(handle, cache, limit).map(Some)
    }

    /// The shared half of a point lookup: bloom, index search, block
    /// read and block scan, stopping at the winning entry's position
    /// inside its block. `None` means this table has no visible entry
    /// for the key.
    ///
    /// The decoded block is handed back by value so a caller that
    /// wants the bytes can build a [`DbSlice`] over it without a
    /// second reference-count round trip, and a caller that only wants
    /// the length can drop it.
    fn probe(
        &self,
        lk: &LookupKey,
        key_buf: &mut Vec<u8>,
        cache: &BlockCache,
        limit: Option<usize>,
    ) -> io::Result<Option<(Arc<Block>, BlockHit)>> {
        let Some(block) = self.candidate_block(lk, cache, limit)? else {
            return Ok(None);
        };
        let user_key = lk.prefixed_user_key();
        let search_key = lk.internal();
        // The scan borrows the block, so it reports where the winning
        // value sits rather than copying it out.
        // A `Break(None)` means the scan walked past the requested user
        // key without finding a terminator for it.
        let hit = block.scan_from(search_key, key_buf, |ik, value_offset, value_len| {
            let (uk, seq, vt) = decode_internal_key(ik);
            if uk != user_key {
                return ControlFlow::Break(None);
            }
            match vt {
                VALUE_TYPE_MERGE => ControlFlow::Continue(()),
                VALUE_TYPE_DELETION => ControlFlow::Break(Some(BlockHit::Tombstone { seq })),
                _ => ControlFlow::Break(Some(BlockHit::Value {
                    seq,
                    value_offset,
                    value_len,
                })),
            }
        });

        match hit.flatten() {
            None => Ok(None),
            Some(hit) => {
                cache.record_bloom_full_positive();
                Ok(Some((block, hit)))
            }
        }
    }

    /// The newest entry this table holds for the key at or below the
    /// lookup's snapshot, of any kind: `(seq, value_type)`.
    ///
    /// Unlike [`SsTableReader::get`], a merge operand counts. The
    /// conflict check asks when a key last changed, and a newer operand
    /// changes it even though a read has to walk past it to a base.
    pub(crate) fn latest_version(
        &self,
        lk: &LookupKey,
        key_buf: &mut Vec<u8>,
        cache: &BlockCache,
    ) -> io::Result<Option<(u64, u8)>> {
        let Some(block) = self.candidate_block(lk, cache, None)? else {
            return Ok(None);
        };
        let user_key = lk.prefixed_user_key();
        let newest = block
            .scan_from(lk.internal(), key_buf, |ik, _, _| {
                let (uk, seq, vt) = decode_internal_key(ik);
                ControlFlow::Break((uk == user_key).then_some((seq, vt)))
            })
            .flatten();
        if newest.is_some() {
            cache.record_bloom_full_positive();
        }
        Ok(newest)
    }

    /// Collect every entry in internal-key order with no dedup or
    /// filtering.
    ///
    /// Test-only: every production caller streams through
    /// [`SsTableReader::iter_internal_stream`] instead, so neither a
    /// flush, a compaction nor an ingest ever holds a whole file.
    #[cfg(test)]
    pub(crate) fn iter_internal(&self, cache: &BlockCache) -> io::Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let mut stream = self.iter_internal_stream(cache)?;
        let mut result = Vec::new();
        while let Some(entry) = stream.next_entry()? {
            result.push(entry);
        }
        Ok(result)
    }

    /// Stream every entry in internal-key order with no dedup or
    /// filtering. At most one decoded data block is buffered at a time.
    pub(crate) fn iter_internal_stream<'a>(
        &'a self,
        cache: &'a BlockCache,
    ) -> io::Result<SsTableInternalIter<'a>> {
        Ok(SsTableInternalIter {
            reader: self,
            cache,
            next_cursor: self.first_block_cursor(cache)?,
            current_block: None,
            current_pos: 0,
            current_key: Vec::new(),
        })
    }

    /// Approximate on-disk bytes whose user key falls in
    /// `[start, end)`. Computed from the index alone - no data-block
    /// decompression. Flat-index tables search the in-memory block
    /// index directly; partitioned-index tables only read index leaves
    /// whose top-level key range intersects the requested bounds. The
    /// estimate is accurate to about one data block per partially-covered
    /// range boundary, matching the "within ~block_size" contract in the
    /// `Db::get_approximate_sizes` docs.
    pub(crate) fn approximate_size_in_range(
        &self,
        start: &[u8],
        end: &[u8],
        cache: &BlockCache,
    ) -> u64 {
        if start >= end {
            return 0;
        }
        let Ok(index) = self.index(cache) else {
            return 0;
        };
        if index.is_empty() {
            return 0;
        }
        let lo = LookupKey::from_prefixed(start, u64::MAX);
        let hi = LookupKey::from_prefixed(end, u64::MAX);
        let (lo_probe, hi_probe) = (lo.internal(), hi.internal());
        if !self.partitioned {
            return index.approximate_size_in_range(lo_probe, hi_probe);
        }

        let first_leaf = index.seek(lo_probe);
        let end_leaf = index.seek(hi_probe).min(index.len() - 1);
        if first_leaf > end_leaf {
            return 0;
        }

        let mut total = 0;
        for leaf_idx in first_leaf..=end_leaf {
            let Some(handle) = index.handle(leaf_idx) else {
                continue;
            };
            let leaf = match self.read_index_leaf(handle, cache) {
                Ok(leaf) => leaf,
                Err(_) => return 0,
            };
            total += leaf.approximate_size_in_range(lo_probe, hi_probe);
        }
        total
    }

    pub(crate) fn read_block(
        &self,
        handle: BlockHandle,
        cache: &BlockCache,
    ) -> io::Result<Arc<Block>> {
        self.read_block_with_limit(handle, cache, None)
    }

    fn read_block_with_limit(
        &self,
        handle: BlockHandle,
        cache: &BlockCache,
        limit: Option<usize>,
    ) -> io::Result<Arc<Block>> {
        if let Some(block) = cache.get(self.file_id, handle.offset) {
            check_data_block_limit(handle.size, block.decoded_buffer_capacity() as u64, limit)?;
            return Ok(block);
        }
        self.fetch_block(handle, cache, limit)
    }

    /// Read the data block at `handle`, which the block cache just missed:
    /// from the device, or, in a `CacheOnly` scope, through the scope's
    /// queue. `limit` is the per-block allocation guard, applied to a block
    /// that landed for the queue as to one the cache held.
    pub(crate) fn fetch_block(
        &self,
        handle: BlockHandle,
        cache: &BlockCache,
        limit: Option<usize>,
    ) -> io::Result<Arc<Block>> {
        if let Some(landed) = self.queue_miss(cache, handle, limit, |reader| {
            Box::new(move |cache| {
                reader
                    .read_block_with_limit(handle, cache, limit)
                    .map(Landed::Data)
            })
        }) {
            let block = landed?.into_data()?;
            check_data_block_limit(handle.size, block.decoded_buffer_capacity() as u64, limit)?;
            return Ok(block);
        }

        if handle.size < self.min_block_frame() {
            return Err(invalid_data("block frame too short"));
        }
        if limit.is_some() && self.seal.is_some() {
            // A sealed frame's codec and decoded length are encrypted, so
            // only the frame is bounded before it is read; the decoded
            // length is checked once it is opened, before decompressing.
            validate_file_region(handle.offset, handle.size, self.data_end, "data block")?;
            check_data_block_limit(handle.size, 0, limit)?;
        } else if limit.is_some() {
            validate_file_region(handle.offset, handle.size, self.data_end, "data block")?;
            // Only the codec and decoded-length prefix are needed before allocating the frame.
            let mut header = [0u8; 5];
            self.file.read_exact_at(handle.offset, &mut header[..1])?;
            if matches!(header[0], COMPRESSION_LZ4 | COMPRESSION_SNAPPY) {
                if handle.size < 9 {
                    return Err(invalid_data("compressed block header too short"));
                }
                self.file
                    .read_exact_at(handle.offset + 1, &mut header[1..])?;
            }
            check_data_block_header(handle.size, &header, limit)?;
        }
        let block_data = read_file_region(
            &*self.file,
            handle.offset,
            handle.size,
            self.data_end,
            "data block",
        )?;
        self.decode_block_frame_with_limit(handle, &block_data, cache, limit)
    }

    /// Read a contiguous span of the data area, clamped to what the file
    /// actually holds.
    ///
    /// A forward scan reads block after adjacent block, and one positional
    /// read per block is one round trip per block. On a network-backed
    /// volume that latency, not bandwidth, is what the scan waits on. The
    /// sequential reader above pulls a span covering many blocks at once
    /// and decodes them out of it.
    ///
    /// `first` is the block the span starts with. A `CacheOnly` scope reads
    /// no span: `first` alone goes through its queue, and the span comes
    /// back empty, so the caller takes the block from where it landed.
    pub(crate) fn read_span(
        &self,
        first: BlockHandle,
        len: u64,
        cache: &BlockCache,
    ) -> io::Result<Vec<u8>> {
        if scope::current().is_some() {
            return self.fetch_block(first, cache, None).map(|_| Vec::new());
        }
        let offset = first.offset;
        let len = len.min(self.data_end.saturating_sub(offset));
        read_file_region(&*self.file, offset, len, self.data_end, "data span")
    }

    /// Turn one block frame into a decoded block, whatever read produced
    /// the bytes. Shared by the per-block read and the span reader so the
    /// checksum, the compression handling and the cache insert cannot
    /// drift between the two paths.
    ///
    pub(crate) fn decode_block_frame(
        &self,
        handle: BlockHandle,
        block_data: &[u8],
        cache: &BlockCache,
    ) -> io::Result<Arc<Block>> {
        self.decode_block_frame_with_limit(handle, block_data, cache, None)
    }

    fn decode_block_frame_with_limit(
        &self,
        handle: BlockHandle,
        block_data: &[u8],
        cache: &BlockCache,
        limit: Option<usize>,
    ) -> io::Result<Arc<Block>> {
        let raw_data = if let Some(seal) = &self.seal {
            // Frame: [nonce][sealed payload || compression_type: u8][tag].
            // The tag authenticates the block, its kind, its table and its
            // offset; an uncompressed block is decrypted into its own buffer.
            let (compression_type, payload) =
                seal.open_data_block(handle.offset, block_data, self.file_id)?;
            if limit.is_some() {
                let decoded = sealed::decoded_len(compression_type, &payload)?;
                check_data_block_limit(block_data.len() as u64, decoded, limit)?;
            }
            if compression_type == COMPRESSION_NONE {
                payload
            } else {
                decompress_block(compression_type, &payload)?
            }
        } else {
            if block_data.len() < 5 {
                return Err(invalid_data("block frame too short"));
            }

            // Frame: [compression_type: u8][payload][checksum: u32].
            // The checksum is an accidental-corruption guard, not a MAC.
            let compression_type = block_data[0];
            let checksum_offset = block_data.len() - 4;
            let stored_checksum =
                u32::from_le_bytes(block_data[checksum_offset..].try_into().unwrap());
            let compressed_data = &block_data[1..checksum_offset];

            let computed_checksum = checksum::sst_block(compression_type, compressed_data);
            if stored_checksum != computed_checksum {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "block checksum mismatch",
                ));
            }

            // Recheck the actual frame: the file may have changed since the header preflight.
            if limit.is_some() {
                check_data_block_header(block_data.len() as u64, block_data, limit)?;
            }
            decompress_block(compression_type, compressed_data)?
        };

        let block = Block::decode_data_block(raw_data)?;
        let block = Arc::new(match self.global_seq {
            Some(seq) => block.stamped(seq),
            None => block,
        });
        cache.insert(self.file_id, handle.offset, Arc::clone(&block));
        Ok(block)
    }
}

/// Whether `path` is a complete SSTable that carries data: at least one
/// point entry or one range tombstone.
///
/// The footer is read first, so probing a whole table directory usually
/// costs one small read per file rather than an index and bloom load per
/// file. An `Err` means the file is not a readable SSTable at all, which
/// the caller must not read as "holds nothing": a file whose footer will
/// not parse cannot be proved empty. Neither can a sealed table whose
/// footer does not verify through `keyring`, or that there is no keyring
/// for.
pub(crate) fn table_carries_data(
    env: &dyn Env,
    path: &Path,
    keyring: Option<&Keyring>,
) -> io::Result<bool> {
    let file = env.open_read(path)?;
    let file_size = file.len()?;
    if file_size < 8 {
        return Err(invalid_data(
            "SSTable file too small to carry a magic number",
        ));
    }
    let mut magic_buf = [0u8; 8];
    file.read_exact_at(file_size - 8, &mut magic_buf)?;
    let footer_size = Footer::size_for_magic(u64::from_le_bytes(magic_buf))?;
    let data_end = file_size
        .checked_sub(footer_size as u64)
        .ok_or_else(|| invalid_data("SSTable file too small for its footer"))?;
    let mut footer_buf = vec![0u8; footer_size];
    file.read_exact_at(data_end, &mut footer_buf)?;
    let footer = Footer::decode(&footer_buf)?;
    validate_footer_regions(&footer, data_end)?;
    let seal = footer.open_seal(keyring, path)?;
    if footer.num_entries > 0 || footer.range_tombstone_size > 0 {
        return Ok(true);
    }
    // A footer that claims nothing is not taken at its word. A V1 or V2
    // footer carries no checksum, so two zeroed `u64`s make a table
    // holding real data claim to hold none, and the open guard that
    // trusts it then discards the table as a crash artifact. When the
    // claim is "empty", the index block has to agree.
    let index_region = read_file_region(
        &*file,
        footer.index_offset,
        footer.index_size,
        data_end,
        "index block",
    )?;
    let file_id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.parse().ok())
        .unwrap_or(0);
    let index = unframe_meta_region(
        seal.as_ref(),
        index_region,
        footer.index_offset,
        checksum::META_KIND_INDEX,
        file_id,
        "index block",
    )?;
    Ok(!IndexBlock::decode(index)?.is_empty())
}

/// Format an SSTable filename from a numeric ID.
pub(crate) fn sst_filename(id: u64) -> String {
    format!("{:06}.sst", id)
}

/// Delete an SSTable file from `env`.
pub(crate) fn remove_sst_in(env: &dyn Env, path: &Path) -> io::Result<()> {
    env.remove_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Point lookup with the per-read key encoder and scratch buffer
    /// the engine threads down, so the assertions below stay about the
    /// SSTable rather than about plumbing.
    fn probe_get(
        reader: &SsTableReader,
        key: &[u8],
        snapshot_seq: u64,
        cache: &BlockCache,
    ) -> io::Result<LookupResult> {
        let lk = LookupKey::from_prefixed(key, snapshot_seq);
        let mut key_buf = Vec::new();
        reader.get(&lk, &mut key_buf, cache)
    }

    /// The length-only twin of [`probe_get`].
    fn probe_get_size(
        reader: &SsTableReader,
        key: &[u8],
        snapshot_seq: u64,
        cache: &BlockCache,
    ) -> io::Result<LookupResult<usize>> {
        let lk = LookupKey::from_prefixed(key, snapshot_seq);
        let mut key_buf = Vec::new();
        reader.get_size(&lk, &mut key_buf, cache, None)
    }
    use crate::column_family::prefix_key;
    use crate::engine::internal_key::{VALUE_TYPE_VALUE, encode_internal_key};
    use tempfile::TempDir;

    fn ik(key: &[u8], seq: u64) -> Vec<u8> {
        encode_internal_key(key, seq, VALUE_TYPE_VALUE)
    }

    fn tombstone(key: &[u8], seq: u64) -> Vec<u8> {
        encode_internal_key(key, seq, VALUE_TYPE_DELETION)
    }

    #[test]
    fn test_sstable_write_read() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.sst");
        let cache = BlockCache::new(64 * 1024 * 1024);

        {
            let mut writer =
                SsTableWriter::new(&path, 4096, 10, CompressionType::Lz4, None, false, 4096)
                    .unwrap();
            // Add 100 distinct user keys at seq=1 in sorted order.
            for i in 0..100 {
                let user_key = format!("key_{:04}", i);
                let value = format!("value_{}", i);
                writer
                    .add(&ik(user_key.as_bytes(), 1), value.as_bytes())
                    .unwrap();
            }
            let summary = writer.finish().unwrap().unwrap();
            assert_eq!(summary.num_entries, 100);
            assert_eq!(summary.smallest_user_key, b"key_0000");
            assert_eq!(summary.largest_user_key, b"key_0099");
        }

        let reader = SsTableReader::open(&path, 1).unwrap();

        // Point lookups at u64::MAX see the latest version.
        assert_eq!(
            probe_get(&reader, b"key_0042", u64::MAX, &cache).unwrap(),
            LookupResult::Found {
                seq: 1,
                value: DbSlice::from_vec(b"value_42".to_vec())
            }
        );
        assert_eq!(
            probe_get(&reader, b"nonexistent", u64::MAX, &cache).unwrap(),
            LookupResult::NotInTable
        );
    }

    #[test]
    fn test_sstable_mvcc_visibility() {
        // Two versions of the same key at seq=1 and seq=3, plus a tombstone
        // at seq=5. A snapshot at seq=4 must see the seq=3 version; seq=2
        // must see seq=1; seq=6 must see the tombstone.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("mvcc.sst");
        let cache = BlockCache::new(1024 * 1024);

        {
            let mut writer =
                SsTableWriter::new(&path, 4096, 10, CompressionType::None, None, false, 4096)
                    .unwrap();
            // Must be written in internal-key order: newest seq first.
            writer.add(&tombstone(b"k", 5), b"").unwrap();
            writer.add(&ik(b"k", 3), b"v3").unwrap();
            writer.add(&ik(b"k", 1), b"v1").unwrap();
            writer.finish().unwrap().unwrap();
        }

        let reader = SsTableReader::open(&path, 7).unwrap();
        assert_eq!(
            probe_get(&reader, b"k", 6, &cache).unwrap(),
            LookupResult::FoundTombstone { seq: 5 }
        );
        assert_eq!(
            probe_get(&reader, b"k", 4, &cache).unwrap(),
            LookupResult::Found {
                seq: 3,
                value: DbSlice::from_vec(b"v3".to_vec())
            }
        );
        assert_eq!(
            probe_get(&reader, b"k", 2, &cache).unwrap(),
            LookupResult::Found {
                seq: 1,
                value: DbSlice::from_vec(b"v1".to_vec())
            }
        );
        assert_eq!(
            probe_get(&reader, b"k", 0, &cache).unwrap(),
            LookupResult::NotInTable
        );
    }

    #[test]
    fn test_sstable_no_compression() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nocomp.sst");
        let cache = BlockCache::new(1024 * 1024);

        {
            let mut writer =
                SsTableWriter::new(&path, 4096, 10, CompressionType::None, None, false, 4096)
                    .unwrap();
            writer.add(&ik(b"hello", 1), b"world").unwrap();
            writer.add(&ik(b"test", 1), b"data").unwrap();
            writer.finish().unwrap().unwrap();
        }

        let reader = SsTableReader::open(&path, 2).unwrap();
        assert_eq!(
            probe_get(&reader, b"hello", u64::MAX, &cache).unwrap(),
            LookupResult::Found {
                seq: 1,
                value: DbSlice::from_vec(b"world".to_vec())
            }
        );
        assert_eq!(
            probe_get(&reader, b"test", u64::MAX, &cache).unwrap(),
            LookupResult::Found {
                seq: 1,
                value: DbSlice::from_vec(b"data".to_vec())
            }
        );
    }

    #[test]
    fn test_sstable_range_tombstones_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("rt.sst");

        {
            let mut writer =
                SsTableWriter::new(&path, 4096, 10, CompressionType::None, None, false, 4096)
                    .unwrap();
            writer.add(&ik(b"a", 1), b"v_a").unwrap();
            writer.add(&ik(b"m", 2), b"v_m").unwrap();
            writer.add(&ik(b"z", 3), b"v_z").unwrap();
            writer.add_range_tombstone(b"b", b"n", 10);
            writer.add_range_tombstone(b"p", b"y", 15);
            let summary = writer.finish().unwrap().unwrap();
            assert_eq!(summary.num_entries, 3);
        }

        let reader = SsTableReader::open(&path, 1).unwrap();
        assert_eq!(reader.range_tombstones().len(), 2);
        assert_eq!(reader.covering_range_tombstone_seq(b"a", 100), 0);
        assert_eq!(reader.covering_range_tombstone_seq(b"b", 100), 10);
        assert_eq!(reader.covering_range_tombstone_seq(b"m", 100), 10);
        assert_eq!(reader.covering_range_tombstone_seq(b"n", 100), 0);
        assert_eq!(reader.covering_range_tombstone_seq(b"p", 100), 15);
        assert_eq!(reader.covering_range_tombstone_seq(b"z", 100), 0);
        // RT at seq 15 is invisible to older snapshots.
        assert_eq!(reader.covering_range_tombstone_seq(b"p", 14), 0);
    }

    #[test]
    fn test_sstable_rt_only_no_points() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("rt_only.sst");

        {
            let mut writer =
                SsTableWriter::new(&path, 4096, 10, CompressionType::None, None, false, 4096)
                    .unwrap();
            writer.add_range_tombstone(b"aa", b"kk", 5);
            writer.add_range_tombstone(b"mm", b"zz", 7);
            let summary = writer.finish().unwrap().unwrap();
            assert_eq!(summary.num_entries, 0);
            assert_eq!(summary.smallest_user_key, b"aa");
            assert_eq!(summary.largest_user_key, b"zz");
        }

        let reader = SsTableReader::open(&path, 2).unwrap();
        assert_eq!(reader.range_tombstones().len(), 2);
        assert_eq!(reader.covering_range_tombstone_seq(b"bb", 100), 5);
        assert_eq!(reader.covering_range_tombstone_seq(b"nn", 100), 7);
    }

    /// A table of three tenants in family 1 and one in family 2, written
    /// with a four-byte extractor.
    fn prefix_table(path: &Path) -> SsTableReader {
        let extractor: Arc<dyn PrefixExtractor> = Arc::new(crate::options::FixedLengthPrefix(4));
        let mut writer = SsTableWriter::new(
            path,
            4096,
            10,
            CompressionType::None,
            Some(extractor),
            false,
            4096,
        )
        .unwrap();
        for (family, tenant) in [(1, "aaaa"), (1, "bbbb"), (1, "cccc"), (2, "dddd")] {
            for i in 0..4 {
                let key = prefix_key(family, format!("{tenant}:key{i}").as_bytes());
                writer.add(&ik(&key, 1), b"v").unwrap();
            }
        }
        writer.finish().unwrap().unwrap();
        SsTableReader::open(path, 1).unwrap()
    }

    /// Whether `reader` might hold a key of `family` that starts with the
    /// caller's `prefix`, by its prefix bloom and a four-byte extractor.
    fn might_hold(reader: &SsTableReader, cache: &BlockCache, family: u32, prefix: &[u8]) -> bool {
        let prefixed = prefix_key(family, prefix);
        let probe = PrefixProbe::new(&crate::options::FixedLengthPrefix(4), &prefixed);
        reader.may_have_prefix(&probe, cache).unwrap()
    }

    #[test]
    fn test_sstable_prefix_bloom_roundtrip() {
        let dir = TempDir::new().unwrap();
        let cache = BlockCache::new(1024 * 1024);
        let reader = prefix_table(&dir.path().join("prefix.sst"));
        assert_eq!(reader.prefix_form(&cache).unwrap(), Some(PrefixForm::Named));

        // Present prefixes
        assert!(might_hold(&reader, &cache, 1, b"aaaa"));
        assert!(might_hold(&reader, &cache, 1, b"bbbb"));
        assert!(might_hold(&reader, &cache, 1, b"cccc:key"));
        assert!(might_hold(&reader, &cache, 2, b"dddd"));
        // A prefix one family holds is not one the other holds.
        assert!(!might_hold(&reader, &cache, 2, b"aaaa"));
        assert!(!might_hold(&reader, &cache, 1, b"dddd"));
        // Absent prefixes (with 10 bits/key the FP rate is ~1%; these
        // specific strings should all be rejected).
        let mut false_positives = 0;
        for i in 0..200u32 {
            let p = format!("zz{:02}", i);
            if might_hold(&reader, &cache, 1, p.as_bytes()) {
                false_positives += 1;
            }
        }
        assert!(
            false_positives < 20,
            "too many prefix bloom false positives: {}",
            false_positives
        );

        // Point lookups still work - user-key bloom is independent.
        assert_eq!(
            probe_get(&reader, &prefix_key(1, b"bbbb:key2"), u64::MAX, &cache).unwrap(),
            LookupResult::Found {
                seq: 1,
                value: DbSlice::from_vec(b"v".to_vec())
            }
        );
    }

    /// A table in a form that names no extractor says so, and a seek reads
    /// it without its prefix filter, whatever its bloom holds.
    #[test]
    fn a_table_that_names_no_extractor_is_not_ruled_out() {
        let dir = TempDir::new().unwrap();
        let cache = BlockCache::new(1024 * 1024);
        for (name, form) in [
            ("engine.sst", PrefixForm::EngineKey),
            ("caller.sst", PrefixForm::CallerKey),
        ] {
            let path = dir.path().join(name);
            let reader = prefix_call::writing_form(form, || prefix_table(&path));
            assert_eq!(reader.prefix_form(&cache).unwrap(), Some(form));
            assert!(might_hold(&reader, &cache, 1, b"aaaa"));
            assert!(might_hold(&reader, &cache, 1, b"zzzz"));
            assert!(might_hold(&reader, &cache, 3, b"aaaa"));
        }
    }

    /// With an extractor that carves nothing out of any key, the table's
    /// prefix bloom holds no entry and rules nothing out.
    #[test]
    fn a_prefix_bloom_with_no_entry_rules_nothing_out() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("short.sst");
        let cache = BlockCache::new(1024 * 1024);
        let extractor: Arc<dyn PrefixExtractor> = Arc::new(crate::options::FixedLengthPrefix(4));
        let mut writer = SsTableWriter::new(
            &path,
            4096,
            10,
            CompressionType::None,
            Some(extractor),
            false,
            4096,
        )
        .unwrap();
        // One of the database's own entries, and a key shorter than four.
        writer
            .add(&ik(&prefix_key(0, b"name:family"), 1), b"v")
            .unwrap();
        writer.add(&ik(&prefix_key(1, b"abc"), 1), b"v").unwrap();
        writer.finish().unwrap().unwrap();

        let reader = SsTableReader::open(&path, 1).unwrap();
        assert!(might_hold(&reader, &cache, 1, b"zzzz"));
        assert!(might_hold(&reader, &cache, 1, b"abc"));
    }

    #[test]
    fn test_sstable_without_prefix_bloom_is_superset() {
        // A file written without an extractor reports every prefix as
        // possibly present - readers must fall back to conservative
        // behavior, not crash.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("noprefix.sst");

        {
            let mut writer =
                SsTableWriter::new(&path, 4096, 10, CompressionType::None, None, false, 4096)
                    .unwrap();
            writer.add(&ik(b"aaaa:1", 1), b"v").unwrap();
            writer.add(&ik(b"bbbb:1", 1), b"v").unwrap();
            writer.finish().unwrap().unwrap();
        }

        let reader = SsTableReader::open(&path, 1).unwrap();
        let cache = BlockCache::new(1024 * 1024);
        // Every query comes back `true`: no negative information.
        assert_eq!(reader.prefix_form(&cache).unwrap(), None);
        assert!(might_hold(&reader, &cache, 1, b"aaaa"));
        assert!(might_hold(&reader, &cache, 1, b"zzzz"));
        assert!(might_hold(&reader, &cache, 9, b"anything"));
    }

    // ── filename / misc helpers ─────────────────────────────────

    #[test]
    fn sst_filename_pads_six_digits() {
        assert_eq!(sst_filename(0), "000000.sst");
        assert_eq!(sst_filename(1), "000001.sst");
        assert_eq!(sst_filename(999_999), "999999.sst");
    }

    // ── Footer encode/decode ────────────────────────────────────

    #[test]
    fn footer_decode_rejects_bad_magic() {
        let mut buf = [0u8; FOOTER_SIZE];
        // Leave magic at zero - any unknown value should be rejected.
        buf[56..64].copy_from_slice(&0xDEAD_BEEF_u64.to_le_bytes());
        let err = Footer::decode(&buf).expect_err("should reject");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("magic"), "got: {err}");
    }

    #[test]
    fn footer_decode_catches_a_flip_in_every_checksummed_field() {
        let f = Footer {
            range_tombstone_offset: 7,
            range_tombstone_size: 11,
            bloom_offset: 13,
            bloom_size: 17,
            index_offset: 19,
            index_size: 23,
            num_entries: 29,
            magic: MAGIC_V5,
            sealed: None,
        };
        let clean = f.encode(None).unwrap();
        for byte in 0..clean.len() {
            for bit in 0..8u8 {
                let mut damaged = clean.clone();
                damaged[byte] ^= 1 << bit;
                assert!(
                    Footer::decode(&damaged).is_err(),
                    "byte {byte} bit {bit} of a V3 footer was not caught"
                );
            }
        }
    }

    #[test]
    fn metadata_checksums_are_domain_separated_by_kind() {
        let payload = b"the same bytes in two different regions";
        let index = checksum::sst_meta(checksum::META_KIND_INDEX, payload);
        assert_ne!(
            index,
            checksum::sst_meta(checksum::META_KIND_BLOOM, payload)
        );
        assert_ne!(
            index,
            checksum::sst_meta(checksum::META_KIND_INDEX_LEAF, payload)
        );
        assert_ne!(
            index,
            checksum::sst_meta(checksum::META_KIND_RANGE_TOMBSTONE, payload)
        );
    }

    /// The writer emits the checksummed form, and the reader agrees with
    /// the module docs about which magic means what.
    #[test]
    fn a_freshly_written_table_carries_the_checksummed_format() {
        for partitioned in [false, true] {
            let dir = TempDir::new().unwrap();
            let path = dir.path().join("fresh.sst");
            let mut writer = SsTableWriter::new(
                &path,
                256,
                10,
                CompressionType::None,
                None,
                partitioned,
                256,
            )
            .unwrap();
            for i in 0..64u32 {
                let key = super::super::internal_key::encode_internal_key(
                    format!("k{i:04}").as_bytes(),
                    1,
                    VALUE_TYPE_VALUE,
                );
                writer.add(&key, format!("v{i}").as_bytes()).unwrap();
            }
            writer.finish().unwrap().unwrap();

            let on_disk = fs::read(&path).unwrap();
            let magic = u64::from_le_bytes(on_disk[on_disk.len() - 8..].try_into().unwrap());
            assert_eq!(
                magic,
                if partitioned { MAGIC_V6 } else { MAGIC_V5 },
                "a fresh table must carry the REGOSST magic"
            );
            assert_eq!(
                &magic.to_be_bytes()[..7],
                b"REGOSST",
                "the identifier itself must read as REGOSST"
            );
            let reader = SsTableReader::open(&path, 1).unwrap();
            assert_eq!(reader.partitioned, partitioned);
            assert!(table_carries_data(&*crate::env::std_env(), &path, None).unwrap());
        }
    }

    /// Every byte of a partitioned table's index leaves and of its
    /// range-tombstone block sits inside a checksummed region too. A leaf
    /// is read lazily, so a flip there surfaces on the read that reaches
    /// it rather than at open; either is loud, and neither serves an
    /// answer the caller would believe.
    #[test]
    fn a_flip_in_an_index_leaf_or_a_range_tombstone_block_is_caught() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("partitioned-rt.sst");
        let mut writer =
            SsTableWriter::new(&path, 64, 10, CompressionType::None, None, true, 96).unwrap();
        for i in 0..40 {
            writer
                .add(&ik(format!("key_{i:04}").as_bytes(), 1), b"value")
                .unwrap();
        }
        writer.add_range_tombstone(b"key_0000", b"key_0005", 3);
        writer.finish().unwrap().unwrap();

        let clean = fs::read(&path).unwrap();
        let f = &clean[clean.len() - FOOTER_SIZE..];
        let word = |i: usize| u64::from_le_bytes(f[i * 8..i * 8 + 8].try_into().unwrap());
        let range_tombstones = word(0)..word(0) + word(1);
        // Everything between the end of the bloom region and the
        // top-level index is index leaves.
        let leaves = word(2) + word(3)..word(4);
        assert!(
            !range_tombstones.is_empty(),
            "the fixture has no range tombstones"
        );
        assert!(
            leaves.end - leaves.start > 1,
            "the fixture did not partition its index"
        );

        for offset in range_tombstones.chain(leaves) {
            for bit in [0u8, 5] {
                let mut damaged = clean.clone();
                damaged[offset as usize] ^= 1 << bit;
                fs::write(&path, &damaged).unwrap();
                let cache = BlockCache::new(1024 * 1024);
                let caught = match SsTableReader::open(&path, 1) {
                    Err(_) => true,
                    Ok(reader) => reader.iter_internal(&cache).is_err(),
                };
                assert!(caught, "byte {offset} bit {bit} was not caught");
            }
        }
    }

    fn expect_reader_open_err(path: &Path) -> io::Error {
        match SsTableReader::open(path, 1) {
            Err(e) => e,
            Ok(_) => panic!("expected invalid SSTable"),
        }
    }

    // ── reader error paths ──────────────────────────────────────

    #[test]
    fn open_rejects_file_smaller_than_footer() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("tiny.sst");
        fs::write(&path, vec![0u8; 10]).unwrap();
        let kind = match SsTableReader::open(&path, 1) {
            Err(e) => e.kind(),
            Ok(_) => panic!("expected InvalidData"),
        };
        assert_eq!(kind, io::ErrorKind::InvalidData);
    }

    #[test]
    fn open_rejects_file_with_bogus_magic() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bogus.sst");
        fs::write(&path, vec![0u8; FOOTER_SIZE]).unwrap();
        let kind = match SsTableReader::open(&path, 1) {
            Err(e) => e.kind(),
            Ok(_) => panic!("expected InvalidData"),
        };
        assert_eq!(kind, io::ErrorKind::InvalidData);
    }

    #[test]
    fn open_rejects_footer_region_past_file_data() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bad-region.sst");
        let footer = Footer {
            range_tombstone_offset: 0,
            range_tombstone_size: 0,
            bloom_offset: 0,
            bloom_size: 8,
            index_offset: 8,
            index_size: 4,
            num_entries: 0,
            magic: MAGIC_V5,
            sealed: None,
        };
        fs::write(&path, footer.encode(None).unwrap()).unwrap();

        let err = expect_reader_open_err(&path);
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn open_missing_file_returns_not_found() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("never.sst");
        let kind = match SsTableReader::open(&path, 1) {
            Err(e) => e.kind(),
            Ok(_) => panic!("expected NotFound"),
        };
        assert_eq!(kind, io::ErrorKind::NotFound);
    }

    #[test]
    fn read_block_rejects_short_frame() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("short-block.bin");
        fs::write(&path, [0u8; 4]).unwrap();
        let reader = SsTableReader {
            file: crate::env::std_env().open_read(&path).unwrap(),
            data_end: 4,
            file_id: 1,
            index: MetaSlot::Pinned(Arc::new(
                IndexBlock::decode(0u32.to_le_bytes().to_vec()).unwrap(),
            )),
            filter: MetaSlot::Cached(BlockHandle { offset: 0, size: 0 }),
            index_fallback: OnceLock::new(),
            filter_fallback: OnceLock::new(),
            range_tombstones: RangeTombstoneSet::default(),
            partitioned: false,
            seal: None,
            global_seq: None,
            me: OnceLock::new(),
            index_leaf_reads: AtomicUsize::new(0),
        };
        let cache = BlockCache::new(1024);

        let err = match reader.read_block(BlockHandle { offset: 0, size: 4 }, &cache) {
            Err(e) => e,
            Ok(_) => panic!("expected invalid block frame"),
        };
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn block_checksum_covers_compression_type() {
        let payload = b"payload";
        let baseline = checksum::sst_block(COMPRESSION_NONE, payload);
        assert_ne!(baseline, checksum::sst_block(COMPRESSION_LZ4, payload));
    }

    #[test]
    fn read_block_rejects_compression_header_flip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("header-flip.sst");
        {
            let mut writer =
                SsTableWriter::new(&path, 4096, 10, CompressionType::None, None, false, 4096)
                    .unwrap();
            writer.add(&ik(b"k", 1), b"v").unwrap();
            writer.finish().unwrap().unwrap();
        }

        let mut bytes = fs::read(&path).unwrap();
        bytes[0] = COMPRESSION_LZ4;
        fs::write(&path, bytes).unwrap();

        let reader = SsTableReader::open(&path, 1).unwrap();
        let cache = BlockCache::new(1024);
        let err = match probe_get(&reader, b"k", u64::MAX, &cache) {
            Err(e) => e,
            Ok(v) => panic!("expected checksum error, got {v:?}"),
        };
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    // ── writer empty / summary ──────────────────────────────────

    #[test]
    fn finish_on_empty_writer_returns_none() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("empty.sst");
        let writer =
            SsTableWriter::new(&path, 4096, 10, CompressionType::None, None, false, 4096).unwrap();
        assert!(writer.finish().unwrap().is_none());
    }

    #[test]
    fn finish_with_only_range_tombstones_returns_summary() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("rt_only_summary.sst");
        let mut writer =
            SsTableWriter::new(&path, 4096, 10, CompressionType::None, None, false, 4096).unwrap();
        writer.add_range_tombstone(b"a", b"z", 7);
        let summary = writer.finish().unwrap().expect("should produce summary");
        assert_eq!(summary.num_entries, 0);
        assert_eq!(summary.smallest_user_key, b"a");
        assert_eq!(summary.largest_user_key, b"z");
    }

    #[test]
    fn summary_records_first_and_last_user_keys() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bounds.sst");
        let mut writer =
            SsTableWriter::new(&path, 4096, 10, CompressionType::None, None, false, 4096).unwrap();
        writer.add(&ik(b"alpha", 1), b"1").unwrap();
        writer.add(&ik(b"bravo", 1), b"2").unwrap();
        writer.add(&ik(b"delta", 1), b"3").unwrap();
        let summary = writer.finish().unwrap().unwrap();
        assert_eq!(summary.smallest_user_key, b"alpha");
        assert_eq!(summary.largest_user_key, b"delta");
        assert_eq!(summary.num_entries, 3);
    }

    #[test]
    fn summary_includes_range_tombstone_bounds_with_point_entries() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("mixed_rt_bounds.sst");
        let mut writer =
            SsTableWriter::new(&path, 4096, 10, CompressionType::None, None, false, 4096).unwrap();
        writer.add(&ik(b"m", 1), b"1").unwrap();
        writer.add(&ik(b"n", 1), b"2").unwrap();
        writer.add_range_tombstone(b"a", b"c", 7);
        writer.add_range_tombstone(b"x", b"z", 8);

        let summary = writer.finish().unwrap().unwrap();

        assert_eq!(summary.smallest_user_key, b"a");
        assert_eq!(summary.largest_user_key, b"z");
        assert_eq!(summary.num_entries, 2);
    }

    // ── multi-block / index ─────────────────────────────────────

    #[test]
    fn multi_block_lookup_walks_index_correctly() {
        // Tiny block size forces each entry into (almost) its own
        // block, giving a tall index and exercising the binary-search
        // path.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("multi.sst");
        let cache = BlockCache::new(1024 * 1024);

        {
            let mut writer =
                SsTableWriter::new(&path, 64, 10, CompressionType::None, None, false, 4096)
                    .unwrap();
            for i in 0..200 {
                let k = format!("k_{:04}", i);
                let v = format!("v_{}", i);
                writer.add(&ik(k.as_bytes(), 1), v.as_bytes()).unwrap();
            }
            writer.finish().unwrap().unwrap();
        }

        let reader = SsTableReader::open(&path, 1).unwrap();
        for i in 0..200 {
            let k = format!("k_{:04}", i);
            let expected = format!("v_{}", i);
            assert_eq!(
                probe_get(&reader, k.as_bytes(), u64::MAX, &cache).unwrap(),
                LookupResult::Found {
                    seq: 1,
                    value: DbSlice::from_vec(expected.into_bytes())
                },
            );
        }
    }

    #[test]
    fn iter_internal_returns_every_entry_in_order() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("iter.sst");
        let cache = BlockCache::new(1024 * 1024);
        let mut writer =
            SsTableWriter::new(&path, 64, 10, CompressionType::None, None, false, 4096).unwrap();
        writer.add(&ik(b"a", 3), b"a3").unwrap();
        writer.add(&ik(b"a", 1), b"a1").unwrap();
        writer.add(&ik(b"b", 2), b"b2").unwrap();
        writer.finish().unwrap().unwrap();

        let reader = SsTableReader::open(&path, 1).unwrap();
        let pairs = reader.iter_internal(&cache).unwrap();
        assert_eq!(pairs.len(), 3);
        // iter_internal preserves raw internal-key order - no dedup,
        // no tombstone hiding.
        let user_keys: Vec<&[u8]> = pairs.iter().map(|(k, _)| user_key_of(k)).collect();
        assert_eq!(user_keys, vec![&b"a"[..], &b"a"[..], &b"b"[..]]);
    }

    #[test]
    fn iter_internal_stream_matches_vec_iterator() {
        for partitioned in [false, true] {
            let dir = TempDir::new().unwrap();
            let path = dir.path().join("stream.sst");
            let cache = BlockCache::new(1024 * 1024);
            let mut writer =
                SsTableWriter::new(&path, 64, 10, CompressionType::None, None, partitioned, 96)
                    .unwrap();
            for i in 0..128 {
                let key = format!("k{i:04}");
                let value = format!("v{i}");
                writer
                    .add(&ik(key.as_bytes(), 1), value.as_bytes())
                    .unwrap();
            }
            writer.finish().unwrap().unwrap();

            let reader = SsTableReader::open(&path, 1).unwrap();
            let expected = reader.iter_internal(&cache).unwrap();
            let mut stream = reader.iter_internal_stream(&cache).unwrap();
            let mut actual = Vec::new();
            while let Some(entry) = stream.next_entry().unwrap() {
                actual.push(entry);
            }
            assert_eq!(actual, expected);
        }
    }

    // ── approximate size ───────────────────────────────────────

    #[test]
    fn approximate_size_empty_range_is_zero() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("approx.sst");
        let mut writer =
            SsTableWriter::new(&path, 4096, 10, CompressionType::None, None, false, 4096).unwrap();
        writer.add(&ik(b"m", 1), b"v").unwrap();
        writer.finish().unwrap().unwrap();

        let reader = SsTableReader::open(&path, 1).unwrap();
        let cache = BlockCache::new(1024 * 1024);
        // start >= end: guaranteed zero.
        assert_eq!(reader.approximate_size_in_range(b"z", b"a", &cache), 0);
        assert_eq!(reader.approximate_size_in_range(b"m", b"m", &cache), 0);
    }

    #[test]
    fn approximate_size_grows_with_range_width() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("approx_grow.sst");
        let mut writer =
            SsTableWriter::new(&path, 128, 10, CompressionType::None, None, false, 4096).unwrap();
        for i in 0..500 {
            writer
                .add(&ik(format!("k_{:04}", i).as_bytes(), 1), b"value")
                .unwrap();
        }
        writer.finish().unwrap().unwrap();

        let reader = SsTableReader::open(&path, 1).unwrap();
        let cache = BlockCache::new(1024 * 1024);
        let narrow = reader.approximate_size_in_range(b"k_0000", b"k_0001", &cache);
        let wide = reader.approximate_size_in_range(b"k_0000", b"k_0499", &cache);
        assert!(
            wide > narrow,
            "wide ({wide}) should exceed narrow ({narrow})"
        );
    }

    // ── compression variants ────────────────────────────────────

    #[test]
    fn lz4_compression_round_trips() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("lz4.sst");
        let cache = BlockCache::new(1024 * 1024);
        {
            let mut writer =
                SsTableWriter::new(&path, 4096, 10, CompressionType::Lz4, None, false, 4096)
                    .unwrap();
            // Repetitive data compresses well - exercises a realistic path.
            for i in 0..50 {
                writer
                    .add(
                        &ik(format!("key_{:03}", i).as_bytes(), 1),
                        b"AAAAAAAAAAAAAAAAAAAAAAAAAA",
                    )
                    .unwrap();
            }
            writer.finish().unwrap().unwrap();
        }
        let reader = SsTableReader::open(&path, 1).unwrap();
        assert_eq!(
            probe_get(&reader, b"key_025", u64::MAX, &cache).unwrap(),
            LookupResult::Found {
                seq: 1,
                value: DbSlice::from_vec(b"AAAAAAAAAAAAAAAAAAAAAAAAAA".to_vec())
            }
        );
    }

    #[test]
    fn snappy_compression_round_trips() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("snappy.sst");
        let cache = BlockCache::new(1024 * 1024);
        {
            let mut writer =
                SsTableWriter::new(&path, 4096, 10, CompressionType::Snappy, None, false, 4096)
                    .unwrap();
            for i in 0..50 {
                writer
                    .add(
                        &ik(format!("key_{:03}", i).as_bytes(), 1),
                        b"BBBBBBBBBBBBBBBBBBBBBBBBBB",
                    )
                    .unwrap();
            }
            writer.finish().unwrap().unwrap();
        }
        let reader = SsTableReader::open(&path, 1).unwrap();
        assert_eq!(
            probe_get(&reader, b"key_007", u64::MAX, &cache).unwrap(),
            LookupResult::Found {
                seq: 1,
                value: DbSlice::from_vec(b"BBBBBBBBBBBBBBBBBBBBBBBBBB".to_vec())
            }
        );
    }

    // ── range tombstone encode/decode ──────────────────────────

    #[test]
    fn range_tombstone_block_round_trip() {
        let input = vec![
            RangeTombstone::new(b"a".to_vec(), b"c".to_vec(), 5),
            RangeTombstone::new(b"f".to_vec(), b"k".to_vec(), 7),
        ];
        let bytes = encode_range_tombstone_block(&input);
        let got = decode_range_tombstone_block(&bytes).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].start, b"a");
        assert_eq!(got[0].end, b"c");
        assert_eq!(got[0].seq, 5);
        assert_eq!(got[1].seq, 7);
    }

    #[test]
    fn range_tombstone_block_empty_input() {
        assert!(decode_range_tombstone_block(&[]).unwrap().is_empty());
    }

    #[test]
    fn range_tombstone_block_rejects_tiny_header() {
        // 1-3 bytes is ambiguous: not empty, not enough for a count.
        let kind = match decode_range_tombstone_block(&[0, 1]) {
            Err(e) => e.kind(),
            Ok(v) => panic!("expected error, got {} tombstones", v.len()),
        };
        assert_eq!(kind, io::ErrorKind::InvalidData);
    }

    #[test]
    fn range_tombstone_block_rejects_truncated_mid_record() {
        // Valid count = 2, but only the first record is complete. The
        // decoder must reject the block rather than silently keeping
        // a prefix of the range-tombstone metadata.
        let mut bytes = vec![];
        bytes.extend_from_slice(&2u32.to_le_bytes());
        // First record: start "aa", end "bb", seq=1 → 4+2+4+2+8 = 20
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(b"aa");
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(b"bb");
        bytes.extend_from_slice(&1u64.to_le_bytes());
        // Second record: deliberately truncated after start_len
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.push(b'c'); // incomplete start

        let err = decode_range_tombstone_block(&bytes).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn range_tombstone_block_rejects_trailing_bytes() {
        let mut bytes =
            encode_range_tombstone_block(&[RangeTombstone::new(b"a".to_vec(), b"b".to_vec(), 1)]);
        bytes.push(0xAA);

        let err = decode_range_tombstone_block(&bytes).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    // ── index block decode ─────────────────────────────────────

    #[test]
    fn index_block_rejects_tiny_header() {
        let err = IndexBlock::decode(vec![1, 2]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn index_block_rejects_truncated_entry() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&4u32.to_le_bytes());
        bytes.extend_from_slice(b"ab");

        let err = IndexBlock::decode(bytes).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn index_block_rejects_trailing_bytes() {
        let mut bytes = encode_index_block(&[]);
        bytes.push(0xAA);

        let err = IndexBlock::decode(bytes).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn reader_with_no_range_tombstones_reports_empty_list() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("no_rt.sst");
        {
            let mut writer =
                SsTableWriter::new(&path, 4096, 10, CompressionType::None, None, false, 4096)
                    .unwrap();
            writer.add(&ik(b"k", 1), b"v").unwrap();
            writer.finish().unwrap().unwrap();
        }
        let reader = SsTableReader::open(&path, 1).unwrap();
        assert!(reader.range_tombstones().is_empty());
        assert_eq!(reader.covering_range_tombstone_seq(b"k", u64::MAX), 0);
    }

    // ── bloom short-circuit ─────────────────────────────────────

    #[test]
    fn negative_lookup_on_many_keys_mostly_short_circuits() {
        // Indirect test: with 10 bpk the bloom rejects >99% of absent
        // keys without touching blocks. We can't directly observe
        // block reads but can verify lookup returns NotInTable fast
        // for all absent keys.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bloomy.sst");
        let cache = BlockCache::new(1024 * 1024);
        {
            let mut writer =
                SsTableWriter::new(&path, 4096, 10, CompressionType::None, None, false, 4096)
                    .unwrap();
            for i in 0..1000 {
                writer
                    .add(&ik(format!("hit_{:04}", i).as_bytes(), 1), b"v")
                    .unwrap();
            }
            writer.finish().unwrap().unwrap();
        }
        let reader = SsTableReader::open(&path, 1).unwrap();
        for i in 0..5000 {
            let absent = format!("miss_{:05}", i);
            assert_eq!(
                probe_get(&reader, absent.as_bytes(), u64::MAX, &cache).unwrap(),
                LookupResult::NotInTable
            );
        }
    }

    // ── partitioned index variant ──────────────────────────────

    fn write_partitioned_fixture(path: &Path) {
        let mut writer =
            SsTableWriter::new(path, 96, 10, CompressionType::None, None, true, 96).unwrap();
        for i in 0..300 {
            writer
                .add(&ik(format!("k_{:04}", i).as_bytes(), 1), b"value")
                .unwrap();
        }
        writer.finish().unwrap().unwrap();
    }

    #[test]
    fn partitioned_index_round_trip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("partitioned.sst");
        let cache = BlockCache::new(1024 * 1024);
        write_partitioned_fixture(&path);
        let reader = SsTableReader::open(&path, 1).unwrap();
        // Spot-check across the range.
        for i in [0usize, 75, 150, 225, 299] {
            let k = format!("k_{:04}", i);
            assert_eq!(
                probe_get(&reader, k.as_bytes(), u64::MAX, &cache).unwrap(),
                LookupResult::Found {
                    seq: 1,
                    value: DbSlice::from_vec(b"value".to_vec()),
                }
            );
        }
    }

    #[test]
    fn partitioned_point_lookup_reads_one_index_leaf() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("partitioned_point.sst");
        let cache = BlockCache::new(1024 * 1024);
        write_partitioned_fixture(&path);

        let reader = SsTableReader::open(&path, 1).unwrap();
        assert!(
            reader.pinned_index_len().unwrap() > 3,
            "fixture must contain several index leaves"
        );

        reader.reset_index_leaf_read_count();
        assert_eq!(
            probe_get(&reader, b"k_0150", u64::MAX, &cache).unwrap(),
            LookupResult::Found {
                seq: 1,
                value: DbSlice::from_vec(b"value".to_vec()),
            }
        );
        assert_eq!(
            reader.index_leaf_read_count(),
            1,
            "point lookup must load only the selected leaf index"
        );
    }

    #[test]
    fn partitioned_internal_stream_starts_without_expanding_all_leaves() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("partitioned_stream.sst");
        let cache = BlockCache::new(1024 * 1024);
        write_partitioned_fixture(&path);

        let reader = SsTableReader::open(&path, 1).unwrap();
        let leaf_count = reader.pinned_index_len().unwrap();
        assert!(leaf_count > 3, "fixture must contain several index leaves");

        reader.reset_index_leaf_read_count();
        let mut stream = reader.iter_internal_stream(&cache).unwrap();
        assert_eq!(
            reader.index_leaf_read_count(),
            1,
            "stream construction should open only the first leaf"
        );

        let first = stream.next_entry().unwrap().expect("fixture has entries");
        assert_eq!(user_key_of(&first.0), b"k_0000");
        assert!(
            reader.index_leaf_read_count() < leaf_count,
            "reading the first entry must not expand every leaf"
        );
    }

    #[test]
    fn partitioned_approximate_size_reads_only_overlapped_leaves() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("partitioned_approx.sst");
        write_partitioned_fixture(&path);

        let reader = SsTableReader::open(&path, 1).unwrap();
        let cache = BlockCache::new(1024 * 1024);
        let leaf_count = reader.pinned_index_len().unwrap();
        assert!(leaf_count > 3, "fixture must contain several index leaves");

        reader.reset_index_leaf_read_count();
        assert!(reader.approximate_size_in_range(b"k_0100", b"k_0101", &cache) > 0);
        assert!(
            reader.index_leaf_read_count() <= 2,
            "narrow range should load at most the boundary leaves"
        );
        assert!(
            reader.index_leaf_read_count() < leaf_count,
            "narrow range must not expand every leaf"
        );
    }

    // -- index and filter blocks through the block cache -------------

    /// A flat-index fixture large enough that its index and filter are
    /// worth measuring.
    fn write_flat_fixture(path: &Path) {
        let mut writer =
            SsTableWriter::new(path, 256, 10, CompressionType::None, None, false, 4096).unwrap();
        for i in 0..500 {
            writer
                .add(&ik(format!("k_{:04}", i).as_bytes(), 1), b"value")
                .unwrap();
        }
        writer.finish().unwrap().unwrap();
    }

    #[test]
    fn partitioned_leaf_is_read_from_disk_once_then_served_from_the_cache() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("leaf_cached.sst");
        let cache = BlockCache::new(1024 * 1024);
        write_partitioned_fixture(&path);

        let reader = SsTableReader::open(&path, 1).unwrap();
        reader.reset_index_leaf_read_count();

        assert!(
            probe_get(&reader, b"k_0150", u64::MAX, &cache).unwrap() != LookupResult::NotInTable
        );
        let after_first = reader.index_leaf_read_count();
        assert_eq!(after_first, 1, "first lookup must read its leaf from disk");

        for _ in 0..8 {
            assert!(
                probe_get(&reader, b"k_0150", u64::MAX, &cache).unwrap()
                    != LookupResult::NotInTable
            );
        }
        assert_eq!(
            reader.index_leaf_read_count(),
            after_first,
            "repeat lookups must be served from the cached leaf"
        );
    }

    #[test]
    fn cached_leaf_survives_a_second_reader_over_the_same_file_id() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("leaf_shared.sst");
        let cache = BlockCache::new(1024 * 1024);
        write_partitioned_fixture(&path);

        let warm = SsTableReader::open(&path, 9).unwrap();
        probe_get(&warm, b"k_0150", u64::MAX, &cache).unwrap();

        let fresh = SsTableReader::open(&path, 9).unwrap();
        fresh.reset_index_leaf_read_count();
        probe_get(&fresh, b"k_0150", u64::MAX, &cache).unwrap();
        assert_eq!(
            fresh.index_leaf_read_count(),
            0,
            "a leaf cached under the same file id is shared across readers"
        );
    }

    #[test]
    fn cached_policy_answers_the_same_as_pinned() {
        let dir = TempDir::new().unwrap();
        for (name, partitioned) in [("cached_flat.sst", false), ("cached_part.sst", true)] {
            let path = dir.path().join(name);
            if partitioned {
                write_partitioned_fixture(&path);
            } else {
                write_flat_fixture(&path);
            }

            let pinned_cache = BlockCache::new(1024 * 1024);
            let cached_cache = BlockCache::new(1024 * 1024);
            let pinned = SsTableReader::open_with(
                &crate::env::std_env(),
                &path,
                1,
                MetadataPolicy::Pinned,
                None,
            )
            .unwrap();
            let cached = SsTableReader::open_with(
                &crate::env::std_env(),
                &path,
                1,
                MetadataPolicy::Cached,
                None,
            )
            .unwrap();

            for probe in ["k_0000", "k_0123", "k_0299", "k_9999"] {
                let a = probe_get(&pinned, probe.as_bytes(), u64::MAX, &pinned_cache).unwrap();
                let b = probe_get(&cached, probe.as_bytes(), u64::MAX, &cached_cache).unwrap();
                assert_eq!(a, b, "{name} disagreed on {probe}");
            }

            let pinned_all = pinned.iter_internal(&pinned_cache).unwrap();
            let cached_all = cached.iter_internal(&cached_cache).unwrap();
            assert_eq!(pinned_all, cached_all, "{name} full scans disagreed");
        }
    }

    #[test]
    fn cached_policy_moves_flat_index_and_filter_into_the_cache() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("charge.sst");
        write_flat_fixture(&path);

        let pinned = SsTableReader::open_with(
            &crate::env::std_env(),
            &path,
            1,
            MetadataPolicy::Pinned,
            None,
        )
        .unwrap();
        let cached = SsTableReader::open_with(
            &crate::env::std_env(),
            &path,
            2,
            MetadataPolicy::Cached,
            None,
        )
        .unwrap();
        assert!(
            pinned.pinned_metadata_bytes() > cached.pinned_metadata_bytes(),
            "the cached reader must hold less outside the budget"
        );

        let cache = BlockCache::new(1024 * 1024);
        assert_eq!(cache.usage(), 0);
        probe_get(&cached, b"k_0100", u64::MAX, &cache).unwrap();
        assert!(
            cache.usage() > 0,
            "index and filter bytes must be charged to the cache"
        );
    }

    #[test]
    fn cached_policy_still_rejects_a_corrupt_index_at_open() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("corrupt_index.sst");
        write_flat_fixture(&path);

        // The footer's last 32 bytes hold index_offset/index_size/
        // num_entries/magic. Shrink index_size so decode sees trailing
        // bytes; the region stays inside the file.
        let mut bytes = fs::read(&path).unwrap();
        let footer_at = bytes.len() - FOOTER_SIZE;
        let index_size =
            u64::from_le_bytes(bytes[footer_at + 40..footer_at + 48].try_into().unwrap());
        bytes[footer_at + 40..footer_at + 48].copy_from_slice(&(index_size - 1).to_le_bytes());
        fs::write(&path, &bytes).unwrap();

        for policy in [MetadataPolicy::Pinned, MetadataPolicy::Cached] {
            let err = match SsTableReader::open_with(&crate::env::std_env(), &path, 1, policy, None)
            {
                Err(e) => e,
                Ok(_) => panic!("a corrupt index must fail the open under either policy"),
            };
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        }
    }

    #[test]
    fn strict_cache_refusal_pins_the_metadata_instead_of_re_reading_it() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("strict.sst");
        write_flat_fixture(&path);

        // One shard, 512 bytes, strict: the flat index and the filter
        // region of a 500-entry file are both larger than that, so both
        // inserts are refused.
        let cache = BlockCache::with_config(512, 0, true);
        let reader = SsTableReader::open_with(
            &crate::env::std_env(),
            &path,
            1,
            MetadataPolicy::Cached,
            None,
        )
        .unwrap();
        assert_eq!(reader.pinned_metadata_bytes(), 0);

        for i in 0..64 {
            let key = format!("k_{:04}", i);
            assert!(
                probe_get(&reader, key.as_bytes(), u64::MAX, &cache).unwrap()
                    != LookupResult::NotInTable,
                "strict-cache reads must still be correct"
            );
        }
        assert!(
            reader.pinned_metadata_bytes() > 0,
            "a refused metadata insert must fall back to pinning"
        );
    }

    #[test]
    fn index_filter_and_data_offsets_do_not_collide() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("offsets.sst");
        write_partitioned_fixture(&path);

        let mut bytes = fs::read(&path).unwrap();
        let magic = u64::from_le_bytes(bytes[bytes.len() - 8..].try_into().unwrap());
        let footer_size = Footer::size_for_magic(magic).unwrap();
        let footer_at = bytes.len() - footer_size;
        let footer = Footer::decode(&bytes[footer_at..]).unwrap();
        bytes.truncate(footer_at);

        let cache = BlockCache::new(1024 * 1024);
        let reader = SsTableReader::open(&path, 1).unwrap();

        // Every data-block offset, every index-leaf offset, the
        // top-level index offset and the filter offset must be
        // distinct: the cache keys them all by (file_id, offset).
        let mut offsets = std::collections::HashSet::new();
        assert!(offsets.insert(footer.bloom_offset), "filter offset");
        assert!(offsets.insert(footer.index_offset), "top index offset");

        let index = reader.index(&cache).unwrap();
        for leaf_idx in 0..index.len() {
            let leaf_handle = index.handle(leaf_idx).unwrap();
            assert!(
                offsets.insert(leaf_handle.offset),
                "leaf {leaf_idx} offset collides"
            );
            let leaf = reader.read_index_leaf(leaf_handle, &cache).unwrap();
            for entry_idx in 0..leaf.len() {
                let handle = leaf.handle(entry_idx).unwrap();
                assert!(
                    offsets.insert(handle.offset),
                    "data block offset {} collides",
                    handle.offset
                );
            }
        }
    }

    #[test]
    fn pinned_metadata_bytes_counts_index_filter_and_tombstones() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("pinned_bytes.sst");
        {
            let mut writer =
                SsTableWriter::new(&path, 4096, 10, CompressionType::None, None, false, 4096)
                    .unwrap();
            writer.add(&ik(b"a", 1), b"v").unwrap();
            writer.add_range_tombstone(b"m", b"z", 5);
            writer.finish().unwrap().unwrap();
        }

        let reader = SsTableReader::open(&path, 1).unwrap();
        let reported = reader.pinned_metadata_bytes();
        assert!(
            reported > std::mem::size_of::<RangeTombstone>(),
            "pinned bytes {reported} must cover more than one tombstone"
        );
    }

    /// Build a one-file SSTable over `entries` at seq 1 and return it
    /// with a cache, for the `get` / `get_size` agreement tests.
    fn reader_over(
        entries: &std::collections::BTreeMap<Vec<u8>, Vec<u8>>,
    ) -> (TempDir, SsTableReader, BlockCache) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("agree.sst");
        let cache = BlockCache::new(4 * 1024 * 1024);
        {
            let mut writer =
                SsTableWriter::new(&path, 512, 10, CompressionType::None, None, false, 512)
                    .expect("writer");
            for (key, value) in entries {
                writer.add(&ik(key, 1), value).expect("add");
            }
            writer.finish().expect("finish").expect("non-empty");
        }
        let reader = SsTableReader::open(&path, 7).expect("open");
        (dir, reader, cache)
    }

    /// `get_size` must agree with `get` on every outcome, differing
    /// only in what a hit carries. If the two ever disagreed, `has` and
    /// `get_size` would answer differently from `get` for the same key.
    fn assert_size_agrees(reader: &SsTableReader, key: &[u8], seq: u64, cache: &BlockCache) {
        let full = probe_get(reader, key, seq, cache).expect("get");
        let sized = probe_get_size(reader, key, seq, cache).expect("get_size");
        match (full, sized) {
            (LookupResult::NotInTable, LookupResult::NotInTable) => {}
            (LookupResult::FoundTombstone { seq: a }, LookupResult::FoundTombstone { seq: b }) => {
                assert_eq!(a, b);
            }
            (LookupResult::Found { seq: a, value }, LookupResult::Found { seq: b, value: len }) => {
                assert_eq!(a, b);
                assert_eq!(value.len(), len);
            }
            (a, b) => panic!("get and get_size disagreed: {a:?} vs {b:?}"),
        }
    }

    #[test]
    fn get_size_agrees_with_get_on_tombstones_and_misses() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("mixed.sst");
        let cache = BlockCache::new(1024 * 1024);
        {
            let mut writer =
                SsTableWriter::new(&path, 512, 10, CompressionType::None, None, false, 512)
                    .unwrap();
            writer.add(&ik(b"alive", 2), b"payload").unwrap();
            writer.add(&tombstone(b"dead", 3), b"").unwrap();
            writer.add(&ik(b"empty", 4), b"").unwrap();
            writer.finish().unwrap().unwrap();
        }
        let reader = SsTableReader::open(&path, 3).unwrap();

        for key in [b"alive".as_ref(), b"dead", b"empty", b"absent", b"zzz"] {
            assert_size_agrees(&reader, key, u64::MAX, &cache);
        }

        assert_eq!(
            probe_get_size(&reader, b"alive", u64::MAX, &cache).unwrap(),
            LookupResult::Found { seq: 2, value: 7 }
        );
        assert_eq!(
            probe_get_size(&reader, b"empty", u64::MAX, &cache).unwrap(),
            LookupResult::Found { seq: 4, value: 0 },
            "a zero-length value is present with length 0, not absent"
        );
        assert_eq!(
            probe_get_size(&reader, b"dead", u64::MAX, &cache).unwrap(),
            LookupResult::FoundTombstone { seq: 3 }
        );
        assert_eq!(
            probe_get_size(&reader, b"absent", u64::MAX, &cache).unwrap(),
            LookupResult::NotInTable
        );
    }

    proptest::proptest! {
        #[test]
        fn get_size_agrees_with_get(
            entries in proptest::collection::btree_map(
                proptest::collection::vec(proptest::prelude::any::<u8>(), 1..12),
                proptest::collection::vec(proptest::prelude::any::<u8>(), 0..40),
                1..40,
            ),
            probes in proptest::collection::vec(
                proptest::collection::vec(proptest::prelude::any::<u8>(), 1..12), 1..12),
        ) {
            let (_dir, reader, cache) = reader_over(&entries);
            for key in entries.keys() {
                assert_size_agrees(&reader, key, u64::MAX, &cache);
            }
            for probe in &probes {
                assert_size_agrees(&reader, probe, u64::MAX, &cache);
            }
        }
    }
}
