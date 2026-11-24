//! Wall-clock TTL wrapper on top of [`crate::Db`].
//!
//! Every value written through [`DbWithTtl`] carries a versioned TTL
//! suffix appended at the end:
//!
//! ```text
//! stored = [user_value]["RTTL"][version: u8][timestamp: u64_be]
//! ```
//!
//! Reads strip the trailing suffix before returning and compare the
//! embedded 64-bit Unix timestamp against `now - ttl`; expired entries
//! read back as `None` even if a compaction has not yet physically
//! removed them.
//!
//! Physical reclamation is driven by a [`TtlCompactionFilter`] installed
//! in [`crate::Options::compaction_filter`] by [`DbWithTtl::open`]: as
//! entries flow through compaction, any whose embedded timestamp is
//! older than the TTL is dropped.
//!
//! A merged key is stored as the writes it holds, each with its time (the
//! `merge` module), so that what a read returns never depends on which
//! writes a compaction met: every write expires by its own time, and a
//! write that has expired is one a read ignores and compaction may drop.

mod merge;

use crate::env::Env;
use std::path::Path;
use std::sync::Arc;

use crate::options::{CompactionDecision, CompactionFilter, MergeOperator};
use crate::{Db, DbSlice, Options, Result, WriteBatch, WriteBatchOp};

/// Identifier stamped into a TTL-carrying value.
const TTL_MAGIC: [u8; 4] = *b"RTTL";
const TTL_MAGIC_LEN: usize = 4;
const TTL_FORMAT_VERSION: u8 = 1;
const TTL_TS_LEN: usize = 8;
const TTL_SUFFIX_LEN: usize = TTL_MAGIC_LEN + 1 + TTL_TS_LEN;

/// A [`Db`] wrapper that attaches a wall-clock TTL to every written
/// value. Entries whose embedded timestamp is older than `ttl_seconds`
/// read as `None` and are physically reclaimed at the next compaction.
///
/// `ttl_seconds == 0` disables expiration entirely - the wrapper still
/// appends the TTL suffix for format consistency, but the filter
/// keeps every entry and reads never treat any value as expired.
///
/// # Merges
///
/// Each write expires by its own time: a put and each merge operand live
/// `ttl_seconds` from when they were written, and a merge does not renew
/// what was written before it. A read folds the writes of a key that have
/// not expired, with the caller's [`crate::MergeOperator`]: the put if it
/// has not expired, and the merges after it that have not. A counter whose
/// put expired reads as the sum of the operands merged in the last
/// `ttl_seconds`; once every write of a key has expired, the key reads as
/// absent.
///
/// So that a read returns the same whatever compaction has done, the
/// writes of a merged key are kept apart, each with its time, until they
/// expire: the caller's operator folds them when the key is read
/// ([`crate::MergeOperator::full_merge`]), and is never asked to fold two
/// operands ahead of a read ([`crate::MergeOperator::partial_merge`]). The
/// cost: a merged key keeps every write of the last `ttl_seconds`, and each
/// read of it folds them all.
///
/// A merge written through [`DbWithTtl::inner`], or by regolith 0.1.9,
/// which did not stamp merges, has no time. It is folded as a database
/// without TTL folds it, which is how 0.1.9 read it: the caller's operator
/// is shown the put and merges before it as they are stored, timestamps
/// and all, and what it returns reads as a stamped value would. Writes that
/// have expired are left out of that fold.
pub struct DbWithTtl {
    inner: Db,
    ttl_seconds: u64,
    env: Arc<dyn Env>,
    /// The caller's operator, which folds a merged key's live writes on a
    /// read.
    merge_operator: Option<Arc<dyn MergeOperator>>,
}

impl std::fmt::Debug for DbWithTtl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DbWithTtl")
            .field("ttl_seconds", &self.ttl_seconds)
            .finish_non_exhaustive()
    }
}

impl DbWithTtl {
    /// Open or create a TTL-enabled database at `path`. The caller's
    /// `opts` are forwarded to [`Db::open`] with one change: a
    /// [`TtlCompactionFilter`] bound to `ttl_seconds` is installed in the
    /// compaction filter slot.
    ///
    /// A [`crate::CompactionFilter`] the caller set in `opts` keeps
    /// running behind it. A value that has expired is removed without
    /// asking it. Every other value is shown to it as the caller wrote it,
    /// without the timestamp the database keeps behind it, and a value it
    /// changes keeps the time it was written at. Its range deletes reach
    /// it unchanged.
    ///
    /// A merged key is shown to it as a read would return it now (see
    /// "Merges" on [`DbWithTtl`]); a value it changes then is stored as one
    /// write, at the time of the newest write it folded.
    ///
    /// A [`crate::MergeOperator`] the caller set is shown the put and the
    /// operands the caller wrote that have not expired, without their
    /// timestamps, when a merged key is read. A merge in
    /// [`DbWithTtl::write`] is a write, stamped like a put.
    pub fn open<P: AsRef<Path>>(path: P, mut opts: Options, ttl_seconds: u64) -> Result<Self> {
        // Every value carries a wall-clock stamp, so an environment
        // without a wall clock cannot serve this wrapper at all. Say
        // so at open rather than stamping every write with the epoch
        // and expiring the whole database on the first compaction.
        let env = Arc::clone(&opts.env);
        if env.unix_secs().is_none() {
            return Err(crate::Error::invalid_argument(
                "a TTL database needs a wall clock, and this Env has none",
            ));
        }
        let merge_operator = opts.merge_operator.take();
        opts.compaction_filter = Some(Arc::new(TtlCompactionFilter {
            ttl_seconds,
            env: Arc::clone(&env),
            callers: opts.compaction_filter.take(),
            merge_operator: merge_operator.clone(),
        }));
        opts.merge_operator = merge_operator.as_ref().map(|callers| {
            Arc::new(merge::TtlMergeOperator {
                callers: Arc::clone(callers),
            }) as Arc<dyn MergeOperator>
        });
        let inner = Db::open(path, opts)?;
        Ok(Self {
            inner,
            ttl_seconds,
            env,
            merge_operator,
        })
    }

    /// Seconds since the Unix epoch, from the environment this
    /// database was opened on.
    ///
    /// [`DbWithTtl::open`] refuses an environment without a wall
    /// clock, so this only reports an error if the host stopped
    /// providing one after open.
    fn now_seconds(&self) -> Result<u64> {
        self.env.unix_secs().ok_or_else(|| {
            crate::Error::invalid_argument("the TTL database's Env stopped reporting a wall clock")
        })
    }

    /// Write `key → value` with the current wall-clock timestamp
    /// appended. The in-memory value is the stamped form until a
    /// matching [`DbWithTtl::get`] strips it off.
    pub fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
        let stamped = stamp(value, self.now_seconds()?);
        self.inner.put(key, &stamped)
    }

    /// Look up `key`. Returns `Ok(None)` if the key is absent or if
    /// its embedded timestamp has aged beyond `ttl_seconds`. The
    /// trailing TTL suffix is always stripped from the returned value.
    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        match self.inner.get(key)? {
            Some(stored) => self.read(key, stored, self.expiry_horizon()?),
            None => Ok(None),
        }
    }

    /// [`DbWithTtl::get`] without copying the value.
    ///
    /// The TTL suffix is dropped by narrowing the view, so the result
    /// still borrows the bytes the database already owns. Same absence
    /// semantics as [`DbWithTtl::get`]: an expired entry, and a value
    /// with no decodable TTL suffix, both read as `Ok(None)`. A merged
    /// key's value is folded on the read, so it is a new buffer.
    pub fn get_slice(&self, key: &[u8]) -> Result<Option<DbSlice>> {
        let Some(stamped) = self.inner.get_slice(key)? else {
            return Ok(None);
        };
        let horizon = self.expiry_horizon()?;
        if merge::decode(stamped.as_slice()).is_some() {
            return Ok(self
                .read(key, stamped.as_slice().to_vec(), horizon)?
                .map(DbSlice::from_vec));
        }
        match decode_ttl_suffix(stamped.as_slice()) {
            Some(decoded) if is_live(decoded.timestamp, horizon) => {
                Ok(stamped.try_subslice(0..decoded.value_len))
            }
            _ => Ok(None),
        }
    }

    /// Delete `key`. Range deletes are delegated to the inner
    /// [`Db::delete`] - tombstones don't carry timestamps.
    pub fn delete(&self, key: &[u8]) -> Result<()> {
        self.inner.delete(key)
    }

    /// Delete every key in `[start, end)`. Same semantics as
    /// [`Db::delete_range`]; TTL has no effect on range tombstones.
    pub fn delete_range(&self, start: &[u8], end: &[u8]) -> Result<()> {
        self.inner.delete_range(start, end)
    }

    /// Scan `[start, end)`, returning `(key, value)` pairs where
    /// every value has its trailing TTL suffix stripped and every
    /// expired entry is omitted. Ordering matches [`Db::scan`].
    pub fn scan(
        &self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let raw = self.inner.scan(start, end)?;
        let horizon = self.expiry_horizon()?;
        let mut out = Vec::with_capacity(raw.len());
        for (k, stored) in raw {
            if let Some(v) = self.read(&k, stored, horizon)? {
                out.push((k, v));
            }
        }
        Ok(out)
    }

    /// Apply a batch of point writes atomically. Each `put` in the
    /// batch gets its value stamped with the current timestamp, and so
    /// does each `merge` operand; each `delete` is passed through
    /// unchanged.
    ///
    /// The input batch's original contents are consumed and the
    /// stamped/delegated version is applied.
    pub fn write(&self, batch: WriteBatch) -> Result<()> {
        let ts = self.now_seconds()?;
        let mut stamped_batch = WriteBatch::new();
        // Source batch keys are already CF-prefixed by the public
        // `put`/`delete`/... methods. Pass them through via raw
        // inserts so we don't double-prefix.
        for op in batch.ops_iter() {
            match op {
                WriteBatchOp::Put { key, value } => {
                    stamped_batch.insert_raw_put(key.clone(), stamp(value, ts));
                }
                WriteBatchOp::Delete { key } => {
                    stamped_batch.insert_raw_delete(key.clone());
                }
                WriteBatchOp::DeleteRange { start, end } => {
                    stamped_batch.insert_raw_range_delete(start.clone(), end.clone());
                }
                // A merge is a write: its operand carries the time, and a
                // fold of it expires with the newest write it holds.
                WriteBatchOp::Merge { key, operand } => {
                    stamped_batch.insert_raw_merge(key.clone(), stamp(operand, ts));
                }
            }
        }
        self.inner.write(stamped_batch)
    }

    /// Borrow the underlying [`Db`] for APIs that [`DbWithTtl`] does
    /// not wrap (streaming iterator, snapshots, compact_range).
    ///
    /// Values read through the inner `Db` still carry their trailing
    /// TTL suffix - callers are responsible for stripping it via
    /// [`strip_timestamp`] if they want the user payload.
    pub fn inner(&self) -> &Db {
        &self.inner
    }

    /// Compact every SSTable overlapping `[start, end)` down the tree, as
    /// a job (see [`Db::compact_range`]). Expired entries are physically
    /// removed via the installed [`TtlCompactionFilter`].
    pub fn compact_range(&self, start: Option<&[u8]>, end: Option<&[u8]>) -> crate::JobTicket {
        self.inner.compact_range(start, end)
    }

    /// Flush to disk and shut down background threads.
    pub fn close(&self) -> Result<()> {
        self.inner.close()
    }

    /// Compute the "entries written before this Unix second have
    /// expired" threshold. Returns `0` when `ttl_seconds == 0` so
    /// every timestamp compares as live.
    fn expiry_horizon(&self) -> Result<u64> {
        if self.ttl_seconds == 0 {
            return Ok(0);
        }
        Ok(horizon_at(self.ttl_seconds, self.now_seconds()?))
    }

    /// What a read of `key` returns from what the database stores for it,
    /// with writes before `horizon` expired: a plain value without its
    /// stamp, or a merged key's live writes folded by the caller's
    /// operator. The one place every read decides.
    fn read(&self, key: &[u8], stored: Vec<u8>, horizon: u64) -> Result<Option<Vec<u8>>> {
        let Some(pieces) = merge::decode(&stored) else {
            return Ok(strip_if_live(stored, horizon));
        };
        Ok(
            merge::read(self.merge_operator.as_deref(), key, &pieces, horizon)?
                .map(|(value, _)| value),
        )
    }
}

/// The second before which a write has expired at `now` under
/// `ttl_seconds`; 0, so that none has, with a TTL of zero. Saturating, so a
/// clock skewed into the past does not expire everything.
fn horizon_at(ttl_seconds: u64, now: u64) -> u64 {
    if ttl_seconds == 0 {
        return 0;
    }
    now.saturating_sub(ttl_seconds)
}

/// Whether a write stamped `at` is live when writes before `horizon` have
/// expired. A read and compaction both decide it here.
fn is_live(at: u64, horizon: u64) -> bool {
    horizon == 0 || at >= horizon
}

/// The value a stamped buffer holds and its time, if it carries a stamp
/// that is live under `horizon`.
fn live_value(stamped: Vec<u8>, horizon: u64) -> Option<(Vec<u8>, u64)> {
    let decoded = decode_ttl_suffix(&stamped)?;
    if !is_live(decoded.timestamp, horizon) {
        return None;
    }
    let mut out = stamped;
    out.truncate(decoded.value_len);
    Some((out, decoded.timestamp))
}

/// Return the raw user value with the trailing TTL suffix removed.
/// Returns `None` if `stamped` does not contain a supported suffix.
/// Callers that hold a `stamped` buffer from a raw read through
/// [`DbWithTtl::inner`] can use this to recover the user payload.
pub fn strip_timestamp(stamped: &[u8]) -> Option<&[u8]> {
    decode_ttl_suffix(stamped).map(|decoded| &stamped[..decoded.value_len])
}

fn strip_if_live(stamped: Vec<u8>, horizon: u64) -> Option<Vec<u8>> {
    live_value(stamped, horizon).map(|(value, _)| value)
}

fn stamp(value: &[u8], ts: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(value.len() + TTL_SUFFIX_LEN);
    out.extend_from_slice(value);
    out.extend_from_slice(&TTL_MAGIC);
    out.push(TTL_FORMAT_VERSION);
    out.extend_from_slice(&ts.to_be_bytes());
    out
}

/// Seconds since the Unix epoch from the standard environment.
#[cfg(test)]
fn now_seconds() -> u64 {
    crate::env::std_env().unix_secs().unwrap_or(0)
}

struct DecodedTtlSuffix {
    value_len: usize,
    timestamp: u64,
}

fn decode_ttl_suffix(stamped: &[u8]) -> Option<DecodedTtlSuffix> {
    if stamped.len() < TTL_SUFFIX_LEN {
        return None;
    }
    let suffix_start = stamped.len() - TTL_SUFFIX_LEN;
    let suffix = &stamped[suffix_start..];
    let magic: [u8; TTL_MAGIC_LEN] = suffix[..TTL_MAGIC_LEN].try_into().unwrap();
    if magic != TTL_MAGIC || suffix[TTL_MAGIC_LEN] != TTL_FORMAT_VERSION {
        return None;
    }
    let ts_start = TTL_MAGIC_LEN + 1;
    let timestamp = u64::from_be_bytes(suffix[ts_start..ts_start + TTL_TS_LEN].try_into().unwrap());
    Some(DecodedTtlSuffix {
        value_len: suffix_start,
        timestamp,
    })
}

// ─── TtlCompactionFilter ────────────────────────────────────────────────────

/// Compaction filter that drops every point entry whose embedded
/// Unix timestamp is older than `ttl_seconds` at compaction time.
///
/// Installed automatically by [`DbWithTtl::open`]. Can be used
/// standalone if a caller prefers to manage the wrapper themselves.
pub struct TtlCompactionFilter {
    ttl_seconds: u64,
    env: Arc<dyn Env>,
    /// The filter the caller set on a [`DbWithTtl`], asked about every
    /// value this one does not expire.
    callers: Option<Arc<dyn CompactionFilter>>,
    /// The caller's merge operator, which folds a merged key's live writes
    /// into what the caller's filter is shown.
    merge_operator: Option<Arc<dyn MergeOperator>>,
}

impl TtlCompactionFilter {
    /// Construct a filter that expires values older than
    /// `ttl_seconds`, reading the wall clock from the standard
    /// environment. A TTL of `0` is a no-op: every entry is kept
    /// regardless of its timestamp.
    pub fn new(ttl_seconds: u64) -> Self {
        Self::with_env(ttl_seconds, crate::env::std_env())
    }

    /// Construct a filter that reads its wall clock from `env`.
    ///
    /// Match this to the [`crate::Options::env`] of the database the
    /// filter is installed on, so compaction and the read path agree
    /// on what "now" means.
    pub fn with_env(ttl_seconds: u64, env: Arc<dyn Env>) -> Self {
        Self {
            ttl_seconds,
            env,
            callers: None,
            merge_operator: None,
        }
    }

    /// A merged key's writes: those that expired are dropped, which a read
    /// was ignoring already, and the caller's filter is shown what a read
    /// returns now. A value it changes is stored as one write, at the time
    /// of the newest write it folded.
    fn filter_merged(
        &self,
        level: usize,
        key: &[u8],
        pieces: Vec<merge::Piece<'_>>,
    ) -> CompactionDecision {
        let horizon = self.horizon();
        let held = pieces.len();
        // A write without a time never expires.
        let kept: Vec<merge::Piece<'_>> = pieces
            .into_iter()
            .filter(|piece| piece.at.is_none_or(|at| is_live(at, horizon)))
            .collect();
        if kept.is_empty() {
            return CompactionDecision::Remove;
        }
        let pruned = || {
            if kept.len() == held {
                CompactionDecision::Keep
            } else {
                CompactionDecision::Change(merge::encode(kept.iter().copied()))
            }
        };
        let Some(callers) = &self.callers else {
            return pruned();
        };
        let Ok(Some((value, newest))) =
            merge::read(self.merge_operator.as_deref(), key, &kept, horizon)
        else {
            // Nothing a read returns, or a fold the operator declines: the
            // caller's filter has no value to be shown.
            return pruned();
        };
        match callers.filter(level, key, &value) {
            CompactionDecision::Keep => pruned(),
            CompactionDecision::Remove => CompactionDecision::Remove,
            CompactionDecision::Change(changed) => {
                CompactionDecision::Change(stamp(&changed, newest))
            }
        }
    }

    /// The second before which a write has expired by the wall clock now,
    /// as a read decides it ([`DbWithTtl::expiry_horizon`]).
    fn horizon(&self) -> u64 {
        // No wall clock means nothing can be shown to have expired, so
        // everything is kept. Dropping data on a guess is the one outcome
        // this filter must never produce.
        self.env
            .unix_secs()
            .map_or(0, |now| horizon_at(self.ttl_seconds, now))
    }
}

impl CompactionFilter for TtlCompactionFilter {
    fn filter(&self, level: usize, key: &[u8], value: &[u8]) -> CompactionDecision {
        if let Some(pieces) = merge::decode(value) {
            return self.filter_merged(level, key, pieces);
        }
        let stamp = decode_ttl_suffix(value);
        if stamp
            .as_ref()
            .is_some_and(|s| !is_live(s.timestamp, self.horizon()))
        {
            return CompactionDecision::Remove;
        }
        let Some(callers) = &self.callers else {
            return CompactionDecision::Keep;
        };
        // The caller's filter is shown the value the caller wrote: what is
        // in front of the stamp, or all of it for a value written past this
        // wrapper, which has none.
        let written = stamp.as_ref().map_or(value.len(), |s| s.value_len);
        match callers.filter(level, key, &value[..written]) {
            // A value the caller's filter rewrites is still this entry: it
            // keeps the stamp it was written with.
            CompactionDecision::Change(mut changed) => {
                changed.extend_from_slice(&value[written..]);
                CompactionDecision::Change(changed)
            }
            decision => decision,
        }
    }

    fn filter_range_delete(&self, level: usize, start: &[u8], end: &[u8]) -> CompactionDecision {
        match &self.callers {
            Some(callers) => callers.filter_range_delete(level, start, end),
            None => CompactionDecision::Keep,
        }
    }

    fn name(&self) -> &'static str {
        "regolith_ttl_filter"
    }
}

// Internal accessors on `WriteBatch` used by `DbWithTtl::write`.
//
// `DbWithTtl::write` needs to rebuild a parallel batch with stamped
// values, so expose a read-only ordered iterator instead of moving
// the storage field to the public API.
impl WriteBatch {
    pub(crate) fn ops_iter(&self) -> impl Iterator<Item = &WriteBatchOp> {
        self.ops.iter()
    }
}

// `Error` is plumbed via `crate::Result`; silence unused-import lint

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tiny_opts() -> Options {
        Options::default().write_buffer_size(4 * 1024)
    }

    fn force_flush(db: &DbWithTtl, tag: &str) {
        let payload = vec![0u8; 512];
        for i in 0..32 {
            let key = format!("__flush_{}_{:04}", tag, i);
            crate::through_stalls(|| db.put(key.as_bytes(), &payload)).unwrap();
        }
    }

    #[test]
    fn test_ttl_basic_put_get() {
        let dir = TempDir::new().unwrap();
        // Ttl is long enough that nothing expires mid-test.
        let db = DbWithTtl::open(dir.path(), Options::default(), 3600).unwrap();
        db.put(b"key1", b"value1").unwrap();
        assert_eq!(db.get(b"key1").unwrap(), Some(b"value1".to_vec()));
        assert_eq!(db.get(b"missing").unwrap(), None);
    }

    #[test]
    fn test_ttl_zero_disables_expiration() {
        let dir = TempDir::new().unwrap();
        let db = DbWithTtl::open(dir.path(), Options::default(), 0).unwrap();
        db.put(b"k", b"v").unwrap();
        // Even after a compaction nothing should be dropped.
        db.compact_range(None, None).wait().unwrap();
        assert_eq!(db.get(b"k").unwrap(), Some(b"v".to_vec()));
    }

    #[test]
    fn test_ttl_expired_read_returns_none() {
        // Simulate expiry by writing a past timestamp directly via
        // the raw inner Db, bypassing `put`'s "stamp with now" path.
        let dir = TempDir::new().unwrap();
        let db = DbWithTtl::open(dir.path(), Options::default(), 10).unwrap();
        // `now - 100s` is well past the 10s TTL horizon.
        let ancient = now_seconds().saturating_sub(100);
        let stamped = stamp(b"stale", ancient);
        db.inner().put(b"old", &stamped).unwrap();
        // Read goes through DbWithTtl and filters the expired entry.
        assert_eq!(db.get(b"old").unwrap(), None);

        // A fresh put with the current timestamp still reads back.
        db.put(b"fresh", b"new").unwrap();
        assert_eq!(db.get(b"fresh").unwrap(), Some(b"new".to_vec()));
    }

    #[test]
    fn test_ttl_compaction_removes_expired() {
        let dir = TempDir::new().unwrap();
        let db = DbWithTtl::open(dir.path(), tiny_opts(), 10).unwrap();
        // Prime with 20 "stale" entries backdated past the horizon.
        let ancient = now_seconds().saturating_sub(100);
        for i in 0..20 {
            let key = format!("old_{i:02}");
            let stamped = stamp(b"payload", ancient);
            crate::through_stalls(|| db.inner().put(key.as_bytes(), &stamped)).unwrap();
        }
        // And 20 fresh entries written normally.
        for i in 0..20 {
            crate::through_stalls(|| db.put(format!("new_{i:02}").as_bytes(), b"payload")).unwrap();
        }

        force_flush(&db, "ttl");
        db.compact_range(None, None).wait().unwrap();

        // Reads via DbWithTtl: stale is gone, fresh survives.
        for i in 0..20 {
            assert_eq!(db.get(format!("old_{i:02}").as_bytes()).unwrap(), None);
            assert_eq!(
                db.get(format!("new_{i:02}").as_bytes()).unwrap(),
                Some(b"payload".to_vec())
            );
        }

        // Persistence check on the raw inner Db: stale entries are
        // physically gone after compaction, not just filtered on read.
        for i in 0..20 {
            assert_eq!(
                db.inner().get(format!("old_{i:02}").as_bytes()).unwrap(),
                None,
                "stale old_{i:02} should be physically removed"
            );
        }
    }

    #[test]
    fn test_ttl_scan_strips_timestamp_and_hides_expired() {
        let dir = TempDir::new().unwrap();
        let db = DbWithTtl::open(dir.path(), Options::default(), 10).unwrap();
        let ancient = now_seconds().saturating_sub(100);
        db.inner()
            .put(b"a_old", &stamp(b"stale_a", ancient))
            .unwrap();
        db.put(b"b_new", b"fresh_b").unwrap();
        db.inner()
            .put(b"c_old", &stamp(b"stale_c", ancient))
            .unwrap();
        db.put(b"d_new", b"fresh_d").unwrap();

        let pairs = db.scan(None, None).unwrap();
        // Only the fresh entries appear; their values are the
        // stripped user payloads.
        assert_eq!(
            pairs,
            vec![
                (b"b_new".to_vec(), b"fresh_b".to_vec()),
                (b"d_new".to_vec(), b"fresh_d".to_vec()),
            ]
        );
    }

    #[test]
    fn test_ttl_write_batch_stamps_every_put() {
        let dir = TempDir::new().unwrap();
        let db = DbWithTtl::open(dir.path(), Options::default(), 3600).unwrap();
        let mut batch = WriteBatch::new();
        batch.put(b"a", b"1");
        batch.put(b"b", b"2");
        batch.delete(b"ghost");
        db.write(batch).unwrap();

        assert_eq!(db.get(b"a").unwrap(), Some(b"1".to_vec()));
        assert_eq!(db.get(b"b").unwrap(), Some(b"2".to_vec()));
        // Raw inner value carries the suffix, proving the stamp was
        // applied.
        let raw = db.inner().get(b"a").unwrap().unwrap();
        assert_eq!(raw.len(), 1 + TTL_SUFFIX_LEN);
        assert_eq!(strip_timestamp(&raw), Some(b"1".as_slice()));
    }

    #[test]
    fn test_ttl_strip_timestamp_helper() {
        let v = stamp(b"hello", 42);
        assert_eq!(strip_timestamp(&v), Some(b"hello".as_slice()));
        assert_eq!(strip_timestamp(b""), None);
        assert_eq!(strip_timestamp(b"abc"), None); // shorter than suffix
    }

    #[test]
    fn test_ttl_timestamp_uses_u64_encoding() {
        let far_future = u32::MAX as u64 + 100;
        let stamped = stamp(b"future", far_future);
        let decoded = decode_ttl_suffix(&stamped).expect("a stamped value");
        assert_eq!(decoded.timestamp, far_future);
        assert_eq!(strip_timestamp(&stamped), Some(b"future".as_slice()));
    }

    #[test]
    fn get_slice_strips_the_suffix_without_copying() {
        let dir = TempDir::new().unwrap();
        let db = DbWithTtl::open(dir.path(), tiny_opts(), 0).unwrap();
        crate::through_stalls(|| db.put(b"k", b"payload")).unwrap();

        let slice = db.get_slice(b"k").unwrap().expect("present");
        assert_eq!(slice.as_slice(), b"payload");
        assert_eq!(Some(slice.to_vec()), db.get(b"k").unwrap());
        assert_eq!(db.get_slice(b"absent").unwrap(), None);
    }

    #[test]
    fn get_slice_hides_an_expired_entry() {
        let dir = TempDir::new().unwrap();
        let db = DbWithTtl::open(dir.path(), tiny_opts(), 1).unwrap();
        // Write straight through the inner `Db` with an ancient stamp so
        // the test does not have to wait a wall-clock second.
        crate::through_stalls(|| {
            db.inner().put(
                b"stale",
                &stamp(b"payload", now_seconds().saturating_sub(1000)),
            )
        })
        .unwrap();
        assert_eq!(db.get_slice(b"stale").unwrap(), None);
        assert_eq!(db.get(b"stale").unwrap(), None);
    }

    #[test]
    fn get_slice_hides_a_value_too_short_to_carry_a_suffix() {
        let dir = TempDir::new().unwrap();
        let db = DbWithTtl::open(dir.path(), tiny_opts(), 0).unwrap();
        crate::through_stalls(|| db.inner().put(b"raw", b"ab")).unwrap();
        assert_eq!(db.get_slice(b"raw").unwrap(), None);
        assert_eq!(db.get(b"raw").unwrap(), None);
    }

    #[test]
    fn test_ttl_filter_standalone_noop_when_zero() {
        let filter = TtlCompactionFilter::new(0);
        let ancient = stamp(b"v", 0);
        assert_eq!(filter.filter(0, b"k", &ancient), CompactionDecision::Keep);
    }

    #[test]
    fn test_ttl_filter_removes_stale() {
        let filter = TtlCompactionFilter::new(10);
        let ancient = stamp(b"v", now_seconds().saturating_sub(100));
        assert_eq!(filter.filter(0, b"k", &ancient), CompactionDecision::Remove);
        let fresh = stamp(b"v", now_seconds());
        assert_eq!(filter.filter(0, b"k", &fresh), CompactionDecision::Keep);
    }
}
