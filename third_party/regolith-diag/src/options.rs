use std::sync::Arc;

use crate::engine::internal_key::INTERNAL_KEY_SUFFIX_LEN;

mod builder;
mod getters;

/// Default maximum user-key length accepted by write APIs: 8 MiB.
pub const DEFAULT_MAX_KEY_SIZE: usize = 8 * 1024 * 1024;

/// Default [`Options::transaction_keys_inline`].
///
/// A transaction that touches more keys than this indexes its buffer.
/// Chosen so an ordinary transaction, which touches a handful of keys,
/// never builds a table it will not use.
pub const DEFAULT_TRANSACTION_KEYS_INLINE: usize = 32;

/// Default maximum value / merge-operand length accepted by write APIs: 64 MiB.
pub const DEFAULT_MAX_VALUE_SIZE: usize = 64 * 1024 * 1024;

/// Highest supported block-cache shard exponent.
pub const MAX_BLOCK_CACHE_SHARD_BITS: u32 = 8;

/// Highest supported Bloom-filter density. Larger values waste space
/// because the hash count is already capped internally.
pub const MAX_BLOOM_BITS_PER_KEY: usize = 64;

/// Default value of [`Options::max_background_compactions`]: one
/// background worker, or none on a target that has no threads.
///
/// No wasm build can carry a background compaction worker, so this is
/// `0` on every wasm target rather than only on the single-threaded
/// ones. Two independent reasons, either of which alone is decisive:
/// without the `atomics` target feature the module has exactly one
/// thread and `std::thread::spawn` reports
/// [`std::io::ErrorKind::Unsupported`]; and the worker's blocking wait
/// is built on thread-parking primitives that do not exist for
/// `wasm32` at all. `0` runs compaction on the calling thread instead.
///
/// This is a compile-time property of the target, not a runtime
/// fallback: the build cannot grow a second thread later.
#[cfg(target_family = "wasm")]
pub const DEFAULT_MAX_BACKGROUND_COMPACTIONS: usize = 0;

/// Default value of [`Options::max_background_compactions`]: one
/// background worker, or none on a target that has no threads.
#[cfg(not(target_family = "wasm"))]
pub const DEFAULT_MAX_BACKGROUND_COMPACTIONS: usize = 1;

/// Decision returned by a [`CompactionFilter`] for each entry it sees.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CompactionDecision {
    /// Leave the entry untouched.
    Keep,
    /// Drop the entry. Compaction replaces it with a tombstone at the
    /// same sequence number so lower levels cannot resurrect the
    /// original value.
    Remove,
    /// Replace the entry's value with a new byte string. The key and
    /// sequence number are preserved.
    Change(Vec<u8>),
}

/// A user-supplied hook that runs during compaction and can drop or
/// rewrite entries in place. Typical uses: TTL expiration, application-
/// level GC, schema migrations.
///
/// # Determinism
///
/// Implementations must be deterministic functions of `(level, key,
/// value)` - the same input should always yield the same decision.
/// They must not read from the database (would deadlock) and should
/// avoid blocking.
///
/// # What the filter is shown
///
/// The filter is handed the key, and the bounds of a range delete, as
/// the caller wrote them: the bytes passed to `put` or `delete_range`,
/// with nothing in front of them. The column family is not part of the
/// key, so one key in two column families reaches the filter as the same
/// bytes.
///
/// It is shown the caller's entries and nothing else. The database keeps
/// entries of its own beside them (the list of column families) and
/// writes range deletes of its own (dropping a column family deletes its
/// whole range): the filter is never asked about either, so a filter
/// that removes everything it is shown removes the caller's data only.
///
/// # How often it runs
///
/// The filter is asked about an entry once for each compaction pass that
/// rewrites it. That is no fixed number: an entry no pass has rewritten
/// yet has not been shown at all, a manual [`crate::Db::compact_range`]
/// carries its tables down one level at a time and asks once a level,
/// and every later pass over the entry asks again. So a decision must
/// stand however often it is taken: asked again about a value it left,
/// the filter must leave that value as it is. Count on being asked
/// eventually, never on being asked once.
///
/// # Snapshot isolation
///
/// Filters run whether or not a [`crate::Snapshot`] is live, so a filter
/// that removes or changes a value an open snapshot can see changes what
/// that snapshot reads from then on. A filter that only hides values
/// readers already treat as absent is unaffected:
/// [`crate::TtlCompactionFilter`] removes only expired values, and an
/// expired value already reads as absent.
///
/// # Example
///
/// A filter that drops rows marked as drafts. It decides from the value
/// alone, so the same entry gets the same answer at every level.
///
/// ```
/// use std::sync::Arc;
///
/// use regolith::{CompactionDecision, CompactionFilter, Db, Options};
///
/// struct DropDrafts;
///
/// impl CompactionFilter for DropDrafts {
///     fn name(&self) -> &'static str {
///         "drop-drafts"
///     }
///
///     fn filter(&self, _level: usize, _key: &[u8], value: &[u8]) -> CompactionDecision {
///         if value.starts_with(b"draft:") {
///             CompactionDecision::Remove
///         } else {
///             CompactionDecision::Keep
///         }
///     }
/// }
///
/// let options = Options::memory().compaction_filter(Some(Arc::new(DropDrafts)));
/// let db = Db::open("rows", options)?;
/// db.put(b"row/1", b"draft:one")?;
/// db.put(b"row/2", b"final:two")?;
///
/// // The filter runs when compaction rewrites an entry, not when it is
/// // written: until then a read still finds the draft.
/// assert!(db.get(b"row/1")?.is_some());
/// let before = db.snapshot();
///
/// db.compact_range(None, None).wait()?;
/// assert_eq!(db.get(b"row/1")?, None);
/// assert_eq!(db.get(b"row/2")?.as_deref(), Some(&b"final:two"[..]));
/// // A snapshot taken before does not shield the draft from the filter.
/// assert_eq!(before.get(b"row/1")?, None);
/// # Ok::<(), regolith::Error>(())
/// ```
pub trait CompactionFilter: Send + Sync + 'static {
    /// Inspect the value of one of the caller's entries: `key` is the key
    /// as the caller wrote it. Called for each value a compaction pass
    /// keeps; versions no reader can see are dropped first and never
    /// reach the filter. `level` is the level the pass writes.
    fn filter(&self, level: usize, key: &[u8], value: &[u8]) -> CompactionDecision;

    /// Inspect a range delete the caller made, by the bounds the caller
    /// passed. Default implementation keeps every range delete.
    /// `CompactionDecision::Change` is treated as `Keep` for range
    /// deletes (there's no "value" to rewrite). Removing one brings back
    /// every value it covered that compaction has not dropped yet.
    fn filter_range_delete(&self, level: usize, start: &[u8], end: &[u8]) -> CompactionDecision {
        let _ = (level, start, end);
        CompactionDecision::Keep
    }

    /// A stable, human-readable identifier for this filter. Used by
    /// tracing and diagnostics.
    fn name(&self) -> &'static str;
}

/// A user-supplied associative merge operator.
///
/// A merge operator lets callers emit `Merge(key, operand)` records
/// cheaply instead of doing a read-modify-write. Readers and
/// compaction collapse a chain of merge operands by calling
/// [`MergeOperator::full_merge`] (combine a base value with an
/// ordered list of operands) or [`MergeOperator::partial_merge`]
/// (fold two adjacent operands, without a base).
///
/// # Determinism and safety
///
/// Implementations must be deterministic functions of their inputs.
/// They MUST NOT read from the database - doing so will deadlock -
/// and they should not block. Returning `None` from either method is
/// interpreted as a merge failure: readers surface it as
/// [`crate::Error::MergeFailed`], while compaction treats it
/// conservatively (keeps the raw chain intact rather than losing data).
///
/// # Operand order
///
/// `operands` in [`MergeOperator::full_merge`] is ordered oldest
/// first: `operands[0]` was written before `operands[1]`, which was
/// written before `operands[2]`, etc. The base value (if any) was
/// written before every operand.
///
/// # The key
///
/// Every method is handed the key as the caller wrote it: the bytes
/// passed to `merge`, with nothing in front of them. That holds for
/// every call the database makes: a point read, a scan, a transaction's
/// read of its own merges, a commit's checks, and compaction. It is the
/// key [`crate::KeyClassifier`] and [`crate::Conflict::key`] see.
///
/// A database has one operator, so a caller whose values have several
/// shapes dispatches on the key inside it, by a prefix of its own. The
/// column family is not part of the key: one key in two column families
/// reaches the operator as the same bytes, so the shape of a value must
/// follow from the key alone.
///
/// # Example
///
/// A counter: the value is a big-endian `u64`, and an operand is an amount
/// to add. [`MergeOperator::partial_merge`] has an append-only list, and
/// [`MergeOperator::touches`] a value with parts.
///
/// ```
/// use std::sync::Arc;
///
/// use regolith::{Db, Error, MergeOperator, Options};
///
/// struct SumCounter;
///
/// fn number(bytes: &[u8]) -> Option<u64> {
///     Some(u64::from_be_bytes(bytes.try_into().ok()?))
/// }
///
/// impl MergeOperator for SumCounter {
///     fn name(&self) -> &'static str {
///         "sum-counter"
///     }
///
///     // An absent key counts from zero. Bytes that are no number are
///     // declined, never guessed at.
///     fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
///         let mut total = base.map_or(Some(0), number)?;
///         for operand in operands {
///             total = total.wrapping_add(number(operand)?);
///         }
///         Some(total.to_be_bytes().to_vec())
///     }
///
///     // Wrapping addition is exactly associative, so two amounts fold into
///     // one before any base is known.
///     fn partial_merge(&self, _key: &[u8], left: &[u8], right: &[u8]) -> Option<Vec<u8>> {
///         Some(number(left)?.wrapping_add(number(right)?).to_be_bytes().to_vec())
///     }
/// }
///
/// let options = Options::memory().merge_operator(Some(Arc::new(SumCounter)));
/// let db = Db::open("counters", options)?;
///
/// // Each write is one operand: no read, no read-modify-write.
/// db.merge(b"visits", &2u64.to_be_bytes())?;
/// db.merge(b"visits", &3u64.to_be_bytes())?;
/// assert_eq!(db.get(b"visits")?, Some(5u64.to_be_bytes().to_vec()));
///
/// // Compaction folds the operands away; a reader sees the same value.
/// db.compact_range(None, None).wait()?;
/// assert_eq!(db.get(b"visits")?, Some(5u64.to_be_bytes().to_vec()));
///
/// // A value the operator declines fails the read that needs it.
/// db.put(b"visits", b"not a number")?;
/// db.merge(b"visits", &1u64.to_be_bytes())?;
/// assert!(matches!(db.get(b"visits"), Err(Error::MergeFailed { .. })));
/// # Ok::<(), regolith::Error>(())
/// ```
pub trait MergeOperator: Send + Sync + 'static {
    /// Combine a `base` value (possibly `None` if the key didn't
    /// exist) with the ordered list of merge `operands`, returning
    /// the final value. Returning `None` signals merge failure.
    ///
    /// `key` is the key as the caller wrote it (see the trait's notes on
    /// the key).
    fn full_merge(&self, key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>>;

    /// Optionally fold two adjacent operands `left` (older) and
    /// `right` (newer) into a single equivalent operand without a
    /// base value. Transaction commits also use this when the combined
    /// operand saves payload bytes and fits the value limit. Compaction
    /// calls it to shrink a key's run of operands when it has no value
    /// to fold them onto, which includes
    /// every run a live snapshot separates from its base. It folds a
    /// run pairwise from oldest to newest, so the fold must be
    /// associative: any run, in any grouping, has to give the value
    /// [`MergeOperator::full_merge`] gives over the unfolded operands.
    /// An operator whose fold is not exactly associative (floating-point
    /// addition, for one) should not implement this; `full_merge` onto a
    /// base value still folds. The default implementation returns
    /// `None`, which leaves such runs as written.
    ///
    /// A [`crate::DbWithTtl`] never calls it: it keeps a merged key's
    /// writes apart until a read, so that each expires by its own time,
    /// and folds them then with [`MergeOperator::full_merge`].
    ///
    /// # Example
    ///
    /// An append-only list: an operand is one item behind a one-byte length,
    /// and a value is items end to end, so two operands fold by joining them.
    ///
    /// ```
    /// use std::sync::Arc;
    ///
    /// use regolith::{Db, MergeOperator, Options};
    ///
    /// struct Append;
    ///
    /// fn item(bytes: &[u8]) -> Vec<u8> {
    ///     let len = u8::try_from(bytes.len()).expect("an item of at most 255 bytes");
    ///     [&[len][..], bytes].concat()
    /// }
    ///
    /// impl MergeOperator for Append {
    ///     fn name(&self) -> &'static str {
    ///         "append"
    ///     }
    ///
    ///     fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
    ///         let mut list = base.unwrap_or_default().to_vec();
    ///         for operand in operands {
    ///             list.extend_from_slice(operand);
    ///         }
    ///         Some(list)
    ///     }
    ///
    ///     fn partial_merge(&self, _key: &[u8], left: &[u8], right: &[u8]) -> Option<Vec<u8>> {
    ///         Some([left, right].concat())
    ///     }
    /// }
    ///
    /// // The law: however a run is grouped, it reads as the unfolded run does.
    /// let (a, b, c) = (item(b"a"), item(b"b"), item(b"c"));
    /// let unfolded = Append.full_merge(b"list", None, &[&a, &b, &c]);
    /// assert_eq!(unfolded.as_deref(), Some(&b"\x01a\x01b\x01c"[..]));
    /// let ab = Append.partial_merge(b"list", &a, &b).unwrap();
    /// let bc = Append.partial_merge(b"list", &b, &c).unwrap();
    /// assert_eq!(Append.full_merge(b"list", None, &[&ab, &c]), unfolded);
    /// assert_eq!(Append.full_merge(b"list", None, &[&a, &bc]), unfolded);
    ///
    /// let options = Options::memory().merge_operator(Some(Arc::new(Append)));
    /// let db = Db::open("lists", options)?;
    /// db.merge(b"list", &a)?;
    /// // The snapshot stands between the first item and the two after it, so
    /// // compaction cannot fold those onto it and joins the two instead.
    /// let pinned = db.snapshot();
    /// db.merge(b"list", &b)?;
    /// db.merge(b"list", &c)?;
    /// db.compact_range(None, None).wait()?;
    ///
    /// assert_eq!(db.get(b"list")?, unfolded);
    /// assert_eq!(pinned.get(b"list")?, Some(a));
    /// # Ok::<(), regolith::Error>(())
    /// ```
    fn partial_merge(&self, key: &[u8], left: &[u8], right: &[u8]) -> Option<Vec<u8>> {
        let _ = (key, left, right);
        None
    }

    /// Whether `operand` changes any of the value parts named by `parts`,
    /// sorted ids the caller's own scheme defines. A transaction that read
    /// a key with [`crate::Transaction::get_parts`] is refused at commit
    /// only by an operand this says touches the parts it named, so an
    /// operand on another part does not abort it.
    ///
    /// `key` is the key exactly as [`MergeOperator::full_merge`] receives
    /// it. Return `true` when unsure: the default, `true`, makes every
    /// operand change every part, so a read by parts is validated as a read
    /// of the whole value is.
    ///
    /// Contract: a part this says an operand leaves alone is left alone by
    /// the value [`MergeOperator::full_merge`] produces, and the answer
    /// survives folding: `touches(partial_merge(a, b), P)` equals
    /// `touches(a, P) || touches(b, P)`. Breaking it lets a transaction
    /// commit on a part that changed. `regolith::testing::check_touches`, behind
    /// the `testing` feature, checks the second half.
    ///
    /// Pure and fast. It runs when the commit of a transaction that read the
    /// key by parts is decided, once for each operand committed to the key
    /// since that transaction began, newest first, down to the first that
    /// touches a named part or to a put or delete. The thread that decides
    /// the commit runs it, which can be another committing thread, and the
    /// commits behind it in the pipeline are decided only after it returns.
    /// A panic here fails that commit with [`crate::Error::CallbackPanicked`]
    /// and latches the database read-only until it is reopened.
    ///
    /// # Example
    ///
    /// A value with two parts, each one byte: a stock level and a price. An
    /// operand names one part and adds to it, and so touches that part only.
    ///
    /// ```
    /// use std::sync::Arc;
    ///
    /// use regolith::{
    ///     Access, IsolationLevel, MergeOperator, OptimisticTransactionDb, Options, TransactionError,
    ///     TxnOptions, WriteKind,
    /// };
    ///
    /// const STOCK: u8 = 0;
    /// const PRICE: u8 = 1;
    ///
    /// struct Fields;
    ///
    /// impl MergeOperator for Fields {
    ///     fn name(&self) -> &'static str {
    ///         "fields"
    ///     }
    ///
    ///     fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
    ///         let mut fields: [u8; 2] = base.map_or(Some([0; 2]), |base| base.try_into().ok())?;
    ///         for &operand in operands {
    ///             let &[part, delta] = operand else { return None };
    ///             let field = fields.get_mut(usize::from(part))?;
    ///             *field = field.wrapping_add(delta);
    ///         }
    ///         Some(fields.to_vec())
    ///     }
    ///
    ///     // Only two operands on one part fold, into one on that part, so the
    ///     // fold touches exactly what the two touched.
    ///     fn partial_merge(&self, _key: &[u8], left: &[u8], right: &[u8]) -> Option<Vec<u8>> {
    ///         let (&[part, a], &[other, b]) = (left, right) else { return None };
    ///         (part == other).then(|| vec![part, a.wrapping_add(b)])
    ///     }
    ///
    ///     // An operand touches the part it names. An empty one names none:
    ///     // unsure, so `true`.
    ///     fn touches(&self, _key: &[u8], operand: &[u8], parts: &[u32]) -> bool {
    ///         operand.first().is_none_or(|part| parts.contains(&u32::from(*part)))
    ///     }
    /// }
    ///
    /// // The law, for one pair that folds.
    /// let folded = Fields.partial_merge(b"item", &[STOCK, 1], &[STOCK, 2]).unwrap();
    /// assert!(Fields.touches(b"item", &folded, &[u32::from(STOCK)]));
    /// assert!(!Fields.touches(b"item", &folded, &[u32::from(PRICE)]));
    ///
    /// let options = Options::memory().merge_operator(Some(Arc::new(Fields)));
    /// let db = OptimisticTransactionDb::open("items", options)?
    ///     .with_isolation(IsolationLevel::DefraLevel);
    /// db.db().put(b"item", &[10, 50])?;
    ///
    /// // A quote decides on the price alone. Stock arriving meanwhile is an
    /// // operand on the other part, so the quote commits.
    /// let quote = db.begin(&TxnOptions::new());
    /// let seen = quote.get_parts(b"item", &[u32::from(PRICE)])?.expect("the item exists");
    /// db.db().merge(b"item", &[STOCK, 5])?;
    /// quote.put(b"quote", &seen[1..])?;
    /// quote.commit()?;
    ///
    /// // A price change touches the part the next quote named: it loses.
    /// let quote = db.begin(&TxnOptions::new());
    /// let seen = quote.get_parts(b"item", &[u32::from(PRICE)])?.expect("the item exists");
    /// db.db().merge(b"item", &[PRICE, 5])?;
    /// quote.put(b"quote", &seen[1..])?;
    /// let Err(TransactionError::Conflict(lost)) = quote.commit() else {
    ///     panic!("the price changed under the quote");
    /// };
    /// assert_eq!((lost.mine(), lost.theirs()), (Access::ReadParts, WriteKind::Merge));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    fn touches(&self, key: &[u8], operand: &[u8], parts: &[u32]) -> bool {
        let _ = (key, operand, parts);
        true
    }

    /// A stable, human-readable identifier for this operator. Used
    /// by tracing and diagnostics.
    fn name(&self) -> &'static str;
}

/// Per-call knobs for point and batch writes. Overrides the database-
/// global [`Options::durability`] on a single operation so callers can
/// opt a critical write into synchronous fsync, or opt a bulk-load
/// phase out of the WAL, without flipping the whole database.
///
/// All fields default to `false`. The ergonomic builders below cover
/// the two knobs currently implemented.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WriteOptions {
    /// Fsync the WAL before returning. Equivalent to
    /// [`DurabilityMode::Immediate`] for this single call, regardless
    /// of the database-global default.
    pub sync: bool,
    /// Skip the WAL append entirely. The caller accepts that a crash
    /// before the next memtable flush loses the write. Used by
    /// bulk-load phases that will be ingested via SST file anyway.
    pub disable_wal: bool,
    /// Reserved for future use by a cooperative write-lock priority
    /// queue. Currently accepted but ignored.
    pub low_pri: bool,
    /// Fail the write under any write stall, a slowdown included.
    ///
    /// No write waits on a stall: without this flag a write under a stop
    /// returns [`crate::Error::WouldBlock`] with a [`crate::StallWait`], and a
    /// write under a slowdown goes ahead. With it, a write under either
    /// returns [`crate::Error::Busy`] straight away, carrying the reason,
    /// and leaves no wait behind. When the engine is not stalling this has
    /// no effect. A transactional commit takes no write options and is
    /// admitted as a write without this flag.
    pub no_slowdown: bool,
}

impl WriteOptions {
    /// Construct a `WriteOptions` with the default values (all
    /// fields `false`).
    pub fn new() -> Self {
        Self::default()
    }

    /// Shortcut for a `WriteOptions` with `sync: true`.
    pub fn sync() -> Self {
        Self {
            sync: true,
            ..Self::default()
        }
    }

    /// Shortcut for a `WriteOptions` with `disable_wal: true`.
    pub fn disable_wal() -> Self {
        Self {
            disable_wal: true,
            ..Self::default()
        }
    }
}

/// Carves a prefix out of a user key so the SSTable bloom filter can
/// answer "does this file contain any key with prefix P?" queries.
///
/// A configured extractor is consulted when building an SSTable: in
/// addition to the user-key bloom filter (which powers point lookups),
/// the writer builds a second, prefix-keyed bloom filter. Prefix-bounded
/// range scans (`Iter::seek_prefix`) consult the prefix bloom and skip
/// SSTables that demonstrably cannot contain the requested prefix.
///
/// Point lookups are unaffected - they always consult the user-key
/// bloom filter and do not call the extractor.
///
/// # The key
///
/// `extract` is handed the key as the caller wrote it, and
/// `extract_query` the prefix as the caller passed it to `seek_prefix`,
/// with nothing in front of them, in every column family. The entries the
/// database keeps for itself are not shown to the extractor. The filter
/// still answers for one column family at a time: a prefix one family
/// holds does not make a table look as if another family held it.
///
/// Changing the extractor is allowed: each table records the name of the
/// extractor that built it, and a prefix seek reads a table built under
/// another name, or before names were recorded, without its prefix filter
/// until a compaction rewrites it.
///
/// A panic in this trait's code while a commit's memtable rotation runs it
/// fails that commit with [`crate::Error::CallbackPanicked`] and latches the
/// database read-only until it is reopened.
pub trait PrefixExtractor: Send + Sync + 'static {
    /// Return the portion of `key` that the prefix bloom should index,
    /// or `None` if `key` cannot produce a prefix (e.g., it's shorter
    /// than a fixed-length extractor's width). Keys that return `None`
    /// are simply absent from the prefix bloom. `key` is the key as the
    /// caller wrote it.
    fn extract<'a>(&self, key: &'a [u8]) -> Option<&'a [u8]>;

    /// Return the bloom key that is safe to probe for a caller's
    /// prefix-bounded scan, or `None` to rule no table out. `prefix` is
    /// the prefix as the caller passed it. The default implementation
    /// only permits exact extracted-prefix queries, preserving
    /// correctness for custom extractors whose output may depend on more
    /// than a fixed byte width. Extractors with stronger prefix-stability
    /// guarantees can override this to enable broader SSTable skipping.
    fn extract_query<'a>(&self, prefix: &'a [u8]) -> Option<&'a [u8]> {
        let extracted = self.extract(prefix)?;
        (extracted == prefix).then_some(extracted)
    }

    /// A stable, human-readable identifier for this extractor, recorded in
    /// every table it builds. Two extractors that carve out different
    /// prefixes must have different names: a table's prefix filter is used
    /// only under the name it records. Also used by tracing and
    /// diagnostics.
    fn name(&self) -> &'static str;
}

/// A [`PrefixExtractor`] that takes the first `N` bytes of every key, as
/// the caller wrote it. Keys shorter than `N` contribute no prefix.
#[derive(Debug, Clone, Copy)]
pub struct FixedLengthPrefix(pub usize);

impl PrefixExtractor for FixedLengthPrefix {
    fn extract<'a>(&self, key: &'a [u8]) -> Option<&'a [u8]> {
        if key.len() >= self.0 {
            Some(&key[..self.0])
        } else {
            None
        }
    }

    fn extract_query<'a>(&self, prefix: &'a [u8]) -> Option<&'a [u8]> {
        self.extract(prefix)
    }

    /// `FixedLengthPrefix(N)`: the width is part of what it extracts, so it
    /// is part of its name.
    fn name(&self) -> &'static str {
        fixed_length_name(self.0)
    }
}

/// The name of [`FixedLengthPrefix`] of `width`, made once per width and
/// kept for the life of the process. Asking for it takes no lock: a width up
/// to 64 has a cell of its own, and a wider one is kept in a lock-free map.
fn fixed_length_name(width: usize) -> &'static str {
    use std::sync::OnceLock;

    use kovan_map::HopscotchMap;

    static SMALL: [OnceLock<&'static str>; 65] = [const { OnceLock::new() }; 65];
    static LARGE: OnceLock<HopscotchMap<usize, &'static str>> = OnceLock::new();
    let make = || &*Box::leak(format!("FixedLengthPrefix({width})").into_boxed_str());
    if let Some(name) = SMALL.get(width) {
        return name.get_or_init(make);
    }
    let large = LARGE.get_or_init(HopscotchMap::new);
    // Threads that miss the same width at once each make a name and the map
    // keeps one: the others stay allocated, one per racing thread at most.
    large
        .get(&width)
        .unwrap_or_else(|| large.get_or_insert(width, make()))
}

/// Compaction strategy used by the background compaction thread.
///
/// # Deleted ranges
///
/// Under [`Level`](Self::Level) and [`Universal`](Self::Universal) a key
/// range that was written and then deleted is compacted without a call
/// from you. Besides the size triggers, a compaction runs when more than
/// half of the entries it would read are deletions and the entries they
/// delete, so that it discards more than it copies. It is an ordinary
/// compaction, counted in
/// [`Ticker::CompactionDeletionTriggered`](crate::Ticker::CompactionDeletionTriggered).
/// Size-triggered work goes first, but passes such a compaction over at
/// most [`sync::MAX_BYPASS`](crate::sync::MAX_BYPASS) times, unless L0 is
/// within one table of a threshold that slows or stops writes.
///
/// Three things limit it:
///
/// - A live [`Snapshot`](crate::Snapshot), or an open transaction, older
///   than the newest deletion in a table defers the compaction of that
///   table for its deletions until the snapshot is dropped or the
///   transaction ends: a deletion a snapshot still needs cannot be
///   discarded. Size-triggered compaction goes on meanwhile.
/// - A table written by an earlier version of regolith, or restored from a
///   backup, carries no count of its deletions. Each such table is read
///   once in the background, a bounded step at a time, and only then
///   weighed. The property `regolith.num-uncounted-tables`
///   ([`Db::get_int_property`](crate::Db::get_int_property)) says how many
///   are left.
/// - A deleted range that is a small part of the tables it lives in is not
///   compacted for its deletions, since rewriting those tables would copy
///   more than it discards. It goes when a size-triggered compaction
///   reaches it, or when [`Db::compact_range`](crate::Db::compact_range)
///   is called on it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CompactionStyle {
    /// Standard leveled compaction: L0 collects flushes, then files
    /// are pushed down through L1..L6 with each level ~10× larger
    /// than the previous one. Best fit for mixed read/write
    /// workloads where space amplification matters more than
    /// write amplification. This is the default.
    #[default]
    Level,
    /// FIFO: regolith never merges files. Once the total size of all
    /// SSTables exceeds [`FifoCompactionOptions::max_table_files_size`],
    /// the oldest SSTable is unlinked. Best fit for time-series and
    /// append-only log workloads where the oldest data is also the
    /// least valuable. Reads still consult every L0 file (there is
    /// no L1+) so read amplification grows with the number of
    /// retained files; this is the trade-off for ~zero write
    /// amplification.
    ///
    /// Because nothing merges L0, the L0 *count* triggers
    /// ([`Options::level0_slowdown_writes_trigger`] and
    /// [`Options::level0_stop_writes_trigger`]) cannot be relieved by
    /// compaction under this style. Set them to `0` unless the byte
    /// cap is guaranteed to bite first.
    ///
    /// For the same reason a deleted key and its deletion both stay until
    /// their table is dropped: no compaction runs for deletions here.
    Fifo,
    /// Universal (size-tiered): all files live at L0 and are
    /// merged into progressively larger runs based on size ratios
    /// instead of fixed level targets. Each merge produces one
    /// larger L0 file; an over-sized database periodically
    /// triggers a full compaction that folds every existing file
    /// into a single run. Write amplification is roughly
    /// `log_ratio(total_size / memtable_size)`, much lower than
    /// leveled at the cost of higher space amplification during
    /// the merge and more read amplification than leveled. Best
    /// fit for write-heavy workloads where disk is cheap and read
    /// latency is tolerant.
    ///
    /// Merging is driven by [`UniversalCompactionOptions`], not by the
    /// L0 *count* triggers: a well-formed size tier legitimately holds
    /// many L0 files without merging any of them. Set
    /// [`Options::level0_stop_writes_trigger`] and
    /// [`Options::level0_slowdown_writes_trigger`] to `0` under this
    /// style, or raise them well past the tier depth the workload
    /// produces.
    Universal,
}

/// Tunables for [`CompactionStyle::Fifo`]. Ignored when
/// [`Options::compaction_style`] is [`CompactionStyle::Level`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FifoCompactionOptions {
    /// Soft cap on the total bytes held by SSTables. After every
    /// flush, if the sum of file sizes exceeds this number, the
    /// background compaction thread unlinks the oldest SSTables
    /// (oldest first, as L0 lists them) until the cap is satisfied or
    /// only one file remains. Must be greater than zero when FIFO
    /// compaction is selected. Default: 1 GiB.
    pub max_table_files_size: u64,
}

impl Default for FifoCompactionOptions {
    fn default() -> Self {
        Self {
            max_table_files_size: 1024 * 1024 * 1024,
        }
    }
}

/// Tunables for [`CompactionStyle::Universal`]. Ignored when the
/// style is not [`CompactionStyle::Universal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UniversalCompactionOptions {
    /// Percent tolerance used by the size-ratio merge rule. Walk
    /// the L0 files newest-first; accumulate files while the
    /// running total is within `size_ratio` percent of the next
    /// candidate's size. When the accumulator hits
    /// [`UniversalCompactionOptions::min_merge_width`] files, the
    /// picker merges them into one new L0 file. A larger
    /// `size_ratio` groups more files per merge (lower write
    /// amplification, larger output); smaller is pickier. Must be
    /// greater than zero when universal compaction is selected.
    /// Default: 1 (percent).
    pub size_ratio: u32,
    /// Smallest number of files the size-ratio rule is willing to
    /// merge. Must be at least `2` when universal compaction is
    /// selected. Pass `2` for the most aggressive grouping.
    /// Default: 2.
    pub min_merge_width: u32,
    /// Cap on the number of files a single size-ratio merge can
    /// consume. Must be >= [`UniversalCompactionOptions::min_merge_width`]
    /// when universal compaction is selected. `u32::MAX` disables
    /// the cap. Default: `u32::MAX`.
    pub max_merge_width: u32,
    /// Size-amplification trigger, as a percent. When
    /// `total_size_of_all_older_files * 100 / size_of_oldest_file`
    /// exceeds this value, the picker forces a full merge of
    /// every L0 file into one run. Default: 200 (i.e., full merge
    /// fires once the accumulated non-oldest content is ~2× the
    /// oldest run). Must be greater than zero when universal
    /// compaction is selected.
    pub max_size_amplification_percent: u32,
}

impl Default for UniversalCompactionOptions {
    fn default() -> Self {
        Self {
            size_ratio: 1,
            min_merge_width: 2,
            max_merge_width: u32::MAX,
            max_size_amplification_percent: 200,
        }
    }
}

/// Controls when data is flushed to disk after a write.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DurabilityMode {
    /// Flush to disk on every write. Safe against process and OS crashes.
    Immediate,
    /// Rely on the OS to flush eventually (default). Process crash is still
    /// safe due to WAL.
    #[default]
    Eventual,
}

/// Block compression codec applied to SSTable data blocks.
///
/// Each codec is identified by a 1-byte discriminator stored in the
/// block frame, so a single SSTable file can read blocks compressed
/// with different codecs (and a database can mix codecs across levels).
///
/// All variants are pure-Rust implementations - no C/C++ toolchain
/// or system libraries required.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CompressionType {
    /// No compression. Smallest CPU cost; largest on-disk footprint.
    None,
    /// Snappy. Fast, modest compression ratio. Pure-Rust via the
    /// `snap` crate.
    Snappy,
    /// LZ4. Slightly faster decompression than Snappy, comparable ratio.
    /// Pure-Rust via `lz4_flex`. This is the default.
    #[default]
    Lz4,
}

pub use crate::engine::arena::ArenaProfile;

/// Configuration options for a regolith database.
///
/// Start from [`Options::default`] or one of the presets
/// ([`Options::embedded`], [`Options::wasm`], [`Options::memory`]) and chain
/// one builder method per option. A method is named after the option it
/// sets, and a new option is a new method, so an existing chain keeps
/// compiling.
///
/// ```
/// use regolith::{DurabilityMode, Options};
///
/// let options = Options::default()
///     .write_buffer_size(8 * 1024 * 1024)
///     .durability(DurabilityMode::Immediate);
/// ```
#[derive(Clone)]
pub struct Options {
    pub(crate) write_buffer_size: usize,
    pub(crate) arena_profile: ArenaProfile,
    pub(crate) block_size: usize,
    pub(crate) block_cache_size: usize,
    pub(crate) block_cache_num_shard_bits: u32,
    pub(crate) strict_capacity_limit: bool,
    pub(crate) bloom_bits_per_key: usize,
    pub(crate) compression: CompressionType,
    pub(crate) compression_per_level: Option<Vec<CompressionType>>,
    pub(crate) l0_compaction_trigger: usize,
    pub(crate) level_base_bytes: u64,
    pub(crate) level_size_multiplier: u64,
    pub(crate) target_file_size: u64,
    pub(crate) durability: DurabilityMode,
    pub(crate) compaction_filter: Option<Arc<dyn CompactionFilter>>,
    pub(crate) prefix_extractor: Option<Arc<dyn PrefixExtractor>>,
    pub(crate) merge_operator: Option<Arc<dyn MergeOperator>>,
    pub(crate) atomic_flush: bool,
    pub(crate) listeners: Vec<Arc<dyn crate::EventListener>>,
    pub(crate) transaction_hooks: Option<Arc<dyn crate::TransactionHooks>>,
    pub(crate) key_provider: Option<Arc<dyn crate::KeyProvider>>,
    pub(crate) statistics: Option<Arc<crate::Statistics>>,
    pub(crate) rate_limiter: Option<Arc<dyn crate::RateLimiter>>,
    pub(crate) level0_slowdown_writes_trigger: usize,
    pub(crate) level0_stop_writes_trigger: usize,
    pub(crate) soft_pending_compaction_bytes_limit: u64,
    pub(crate) hard_pending_compaction_bytes_limit: u64,
    pub(crate) max_write_buffer_number: usize,
    pub(crate) compaction_style: CompactionStyle,
    pub(crate) fifo_compaction_options: FifoCompactionOptions,
    pub(crate) universal_compaction_options: UniversalCompactionOptions,
    pub(crate) max_background_compactions: usize,
    pub(crate) log_writeback: bool,
    /// `None` until [`Options::inline_compaction`] is called: resolved by
    /// [`Options::inline_compaction_resolved`].
    pub(crate) inline_compaction: Option<bool>,
    pub(crate) max_subcompactions: usize,
    pub(crate) evict_compaction_data_from_page_cache: bool,
    pub(crate) partitioned_index: bool,
    pub(crate) cache_index_and_filter_blocks: bool,
    pub(crate) metadata_block_size: usize,
    pub(crate) read_only: bool,
    pub(crate) max_key_size: usize,
    pub(crate) max_value_size: usize,
    pub(crate) transaction_keys_inline: usize,
    pub(crate) env: Arc<dyn crate::env::Env>,
    pub(crate) max_open_files: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            write_buffer_size: 64 * 1024 * 1024,
            arena_profile: ArenaProfile::SERVER,
            block_size: 16 * 1024,
            block_cache_size: 512 * 1024 * 1024,
            block_cache_num_shard_bits: 6,
            strict_capacity_limit: false,
            bloom_bits_per_key: 10,
            compression: CompressionType::Lz4,
            compression_per_level: None,
            l0_compaction_trigger: 4,
            level_base_bytes: 256 * 1024 * 1024,
            level_size_multiplier: 10,
            target_file_size: 64 * 1024 * 1024,
            durability: DurabilityMode::Eventual,
            compaction_filter: None,
            prefix_extractor: None,
            merge_operator: None,
            atomic_flush: false,
            listeners: Vec::new(),
            transaction_hooks: None,
            key_provider: None,
            statistics: None,
            rate_limiter: None,
            level0_slowdown_writes_trigger: 20,
            level0_stop_writes_trigger: 36,
            soft_pending_compaction_bytes_limit: 64 * 1024 * 1024 * 1024,
            hard_pending_compaction_bytes_limit: 256 * 1024 * 1024 * 1024,
            max_write_buffer_number: 2,
            compaction_style: CompactionStyle::Level,
            fifo_compaction_options: FifoCompactionOptions::default(),
            universal_compaction_options: UniversalCompactionOptions::default(),
            evict_compaction_data_from_page_cache: false,
            max_background_compactions: DEFAULT_MAX_BACKGROUND_COMPACTIONS,
            log_writeback: false,
            inline_compaction: None,
            max_subcompactions: 1,
            partitioned_index: false,
            cache_index_and_filter_blocks: false,
            metadata_block_size: 4096,
            read_only: false,
            max_key_size: DEFAULT_MAX_KEY_SIZE,
            max_value_size: DEFAULT_MAX_VALUE_SIZE,
            transaction_keys_inline: DEFAULT_TRANSACTION_KEYS_INLINE,
            env: crate::env::std_env(),
            max_open_files: 0,
        }
    }
}

impl std::fmt::Debug for Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("write_buffer_size", &self.write_buffer_size)
            .field("arena_profile", &self.arena_profile)
            .field("block_size", &self.block_size)
            .field("block_cache_size", &self.block_cache_size)
            .field(
                "block_cache_num_shard_bits",
                &self.block_cache_num_shard_bits,
            )
            .field("strict_capacity_limit", &self.strict_capacity_limit)
            .field("bloom_bits_per_key", &self.bloom_bits_per_key)
            .field("compression", &self.compression)
            .field("compression_per_level", &self.compression_per_level)
            .field("l0_compaction_trigger", &self.l0_compaction_trigger)
            .field("level_base_bytes", &self.level_base_bytes)
            .field("level_size_multiplier", &self.level_size_multiplier)
            .field("target_file_size", &self.target_file_size)
            .field("durability", &self.durability)
            .field(
                "compaction_filter",
                &self.compaction_filter.as_ref().map(|f| f.name()),
            )
            .field(
                "prefix_extractor",
                &self.prefix_extractor.as_ref().map(|p| p.name()),
            )
            .field(
                "merge_operator",
                &self.merge_operator.as_ref().map(|m| m.name()),
            )
            .field("atomic_flush", &self.atomic_flush)
            .field("listeners", &self.listeners.len())
            .field("transaction_hooks", &self.transaction_hooks.is_some())
            .field("key_provider", &self.key_provider.is_some())
            .field("statistics", &self.statistics.is_some())
            .field("rate_limiter", &self.rate_limiter.is_some())
            .field(
                "level0_slowdown_writes_trigger",
                &self.level0_slowdown_writes_trigger,
            )
            .field(
                "level0_stop_writes_trigger",
                &self.level0_stop_writes_trigger,
            )
            .field(
                "soft_pending_compaction_bytes_limit",
                &self.soft_pending_compaction_bytes_limit,
            )
            .field(
                "hard_pending_compaction_bytes_limit",
                &self.hard_pending_compaction_bytes_limit,
            )
            .field("max_write_buffer_number", &self.max_write_buffer_number)
            .field("compaction_style", &self.compaction_style)
            .field("fifo_compaction_options", &self.fifo_compaction_options)
            .field(
                "universal_compaction_options",
                &self.universal_compaction_options,
            )
            .field(
                "evict_compaction_data_from_page_cache",
                &self.evict_compaction_data_from_page_cache,
            )
            .field(
                "max_background_compactions",
                &self.max_background_compactions,
            )
            .field("log_writeback", &self.log_writeback)
            .field("inline_compaction", &self.inline_compaction_resolved())
            .field("max_subcompactions", &self.max_subcompactions)
            .field("partitioned_index", &self.partitioned_index)
            .field(
                "cache_index_and_filter_blocks",
                &self.cache_index_and_filter_blocks,
            )
            .field("metadata_block_size", &self.metadata_block_size)
            .field("read_only", &self.read_only)
            .field("max_key_size", &self.max_key_size)
            .field("max_value_size", &self.max_value_size)
            .field("env", &self.env)
            .finish()
    }
}

impl Options {
    /// [`Options::inline_compaction`] as the engine takes it: what was set,
    /// or, when nothing was, on for a database with no compaction worker and
    /// off for one that has a worker.
    pub(crate) fn inline_compaction_resolved(&self) -> bool {
        self.inline_compaction
            .unwrap_or(self.max_background_compactions == 0)
    }

    /// Tuning for a device or module whose whole working set has to
    /// fit in roughly 1-4 MiB: a Linux-class embedded board (Cortex-A,
    /// or an ESP32-S3 under esp-idf), or a wasm module, where every
    /// 64 KiB page linear memory ever touches stays committed for the
    /// life of the instance.
    ///
    /// [`Options::default`] is tuned for a server and reserves far
    /// more than such a host has: a 64 MiB write buffer, a 512 MiB
    /// block cache, and a 64 MiB target file size. This profile
    /// replaces every one of those with a value that is bounded by the
    /// budget rather than by throughput.
    ///
    /// # The budget, term by term
    ///
    /// Steady-state resident cost, which is what the 1-4 MiB figure
    /// has to cover:
    ///
    /// - **Memtables, ~0.5 MiB of entry bytes.**
    ///   [`Options::write_buffer_size`] is 256 KiB and
    ///   [`Options::max_write_buffer_number`] is 2, so at most one
    ///   active plus one frozen memtable is resident. Skip-list node
    ///   overhead sits on top of the entry bytes, so budget more than
    ///   512 KiB for this term, not exactly 512 KiB.
    /// - **Block cache, exactly 0 bytes.**
    ///   [`Options::block_cache_size`] of 0 allocates no shard at all,
    ///   retains no block, and leaves the cache tickers at zero. A
    ///   host with an OS page cache absorbs the resulting re-reads;
    ///   a wasm module serves them out of bytes it already holds.
    ///   Raise it to 256 KiB if the budget is nearer 4 MiB than 1 MiB.
    /// - **Per-SSTable metadata, linear in the number of open files.**
    ///   Each open SSTable holds its index block and Bloom filter
    ///   resident. With a 256 KiB [`Options::target_file_size`], a
    ///   4 KiB [`Options::block_size`], and 10
    ///   [`Options::bloom_bits_per_key`], that is roughly 4-6 KiB per
    ///   file, so about 0.25 MiB for a 16 MiB database. This term is
    ///   the one that grows without bound as the database grows; see
    ///   the note on [`Options::partitioned_index`] below.
    /// - **Transient flush and compaction buffers**, bounded by
    ///   [`Options::target_file_size`] and [`Options::block_size`]
    ///   rather than by the size of the level being merged.
    ///
    /// # Every value, and what it buys
    ///
    /// - [`Options::write_buffer_size`] `256 KiB`: the largest
    ///   steady-state allocation, and the bound on how much WAL
    ///   accumulates between rotations.
    /// - [`Options::max_write_buffer_number`] `2`: caps in-memory
    ///   entry bytes at two write buffers.
    /// - [`Options::block_size`] `4 KiB`: matches LittleFS and SPIFFS
    ///   page granularity, and caps the transient decompression buffer
    ///   for an uncached read at 4 KiB instead of the default 16 KiB.
    ///   That transient matters more here precisely because there is
    ///   no block cache to amortize it.
    /// - [`Options::block_cache_size`] `0` and
    ///   [`Options::block_cache_num_shard_bits`] `0`: no cache, and
    ///   the shard count pinned so that raising the budget later does
    ///   not silently allocate 64 shards.
    /// - [`Options::target_file_size`] `256 KiB`: bounds one
    ///   compaction output file, and keeps compaction holding a block
    ///   at a time rather than a file at a time.
    /// - [`Options::level_base_bytes`] `1 MiB` with
    ///   [`Options::level_size_multiplier`] `10`: L1 1 MiB, L2 10 MiB,
    ///   L3 100 MiB - a shape suited to flash measured in tens of MiB.
    /// - [`Options::l0_compaction_trigger`] `2`,
    ///   [`Options::level0_slowdown_writes_trigger`] `4`,
    ///   [`Options::level0_stop_writes_trigger`] `8`: compact early
    ///   and often. A writer that compacts on its own thread should
    ///   pay in small installments rather than take one long stall,
    ///   and a low L0 count also caps how many files a point lookup
    ///   has to consult.
    /// - [`Options::soft_pending_compaction_bytes_limit`] `4 MiB` and
    ///   [`Options::hard_pending_compaction_bytes_limit`] `16 MiB`:
    ///   the byte-denominated back-pressure, scaled to flash rather
    ///   than to the default's 64 GiB / 256 GiB.
    /// - [`Options::max_background_compactions`] `0`: compaction runs
    ///   on the calling thread. Required on a single-threaded host;
    ///   on a board with a spare core, set this to `1`.
    /// - [`Options::inline_compaction`] `true`: with no worker, a write
    ///   a stall stops leaves the step that relieves it on its thread's
    ///   queue, or runs it inline with no queue, so a stall clears without
    ///   a caller compacting by hand.
    /// - [`Options::evict_compaction_data_from_page_cache`] `true`: on
    ///   a device whose page cache is a few MiB, compaction streaming
    ///   through it would evict every hot foreground page. The hint is
    ///   `posix_fadvise(DONTNEED)` on Linux and a no-op elsewhere.
    /// - [`Options::max_key_size`] `16 KiB` and
    ///   [`Options::max_value_size`] `256 KiB`: the defaults accept an
    ///   8 MiB key and a 64 MiB value, either of which exceeds the
    ///   whole budget on its own. Capping a value at one write buffer
    ///   keeps a single accepted write from being one the memtable
    ///   cannot hold.
    /// - [`Options::compression`] stays [`CompressionType::Lz4`],
    ///   inherited from [`Options::default`]. It is pure Rust, its
    ///   block format needs no allocator tricks, and it reduces flash
    ///   writes, which is the resource that wears out.
    /// - [`Options::bloom_bits_per_key`] stays `10`: about a 1% false
    ///   positive rate for 1.25 bytes per key. Cutting it saves little
    ///   memory and costs real reads.
    /// - [`Options::partitioned_index`] stays `false`, so each open
    ///   SSTable's index is resident. That is the cheaper choice while
    ///   the per-file metadata term above stays small. Past roughly
    ///   100 MiB of database - about 400 files at this file size, or
    ///   ~1 MiB of resident index - set it to `true` to keep only a
    ///   top-level index in memory, at the cost of one extra read per
    ///   point lookup. [`Options::metadata_block_size`] is already set
    ///   to 1 KiB here so that switch does not also need retuning.
    ///
    /// # Not a promise about your workload
    ///
    /// These values bound what regolith reserves. They do not bound what
    /// your keys and values cost, nor what the allocator does with
    /// fragmentation. Measure on the target.
    ///
    /// ```
    /// use regolith::{Db, Options};
    ///
    /// let dir = tempfile::TempDir::new()?;
    /// let db = Db::open(dir.path(), Options::embedded())?;
    /// db.put(b"k", b"v")?;
    /// assert_eq!(db.get(b"k")?.as_deref(), Some(&b"v"[..]));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Why the memtable term stays flat
    ///
    /// Memtable-attributable memory is bounded by
    /// `2 * M * (W + c) + M * W` where `W = write_buffer_size`,
    /// `c = arena_profile.max_chunk_size` and
    /// `M = max_write_buffer_number`: at most `2 * M` memtables are in
    /// memory at once (the write stall stops writers there), each arena
    /// holds at most `W + c`, and the recycling pool parks at most
    /// `M * W`.
    ///
    /// That flatness is the point on wasm32, where linear memory grows
    /// in 64 KiB pages and never shrinks, so every peak is permanent.
    /// [`ArenaProfile::EMBEDDED`] caps a chunk at exactly one page and
    /// the pool hands the same pages back to the next memtable, so
    /// `memory.grow` runs once per size class rather than once per
    /// flush-and-refill cycle.
    pub fn embedded() -> Self {
        Self {
            write_buffer_size: 256 * 1024,
            arena_profile: ArenaProfile::EMBEDDED,
            block_size: 4 * 1024,
            block_cache_size: 0,
            block_cache_num_shard_bits: 0,
            l0_compaction_trigger: 2,
            level_base_bytes: 1024 * 1024,
            target_file_size: 256 * 1024,
            level0_slowdown_writes_trigger: 4,
            level0_stop_writes_trigger: 8,
            soft_pending_compaction_bytes_limit: 4 * 1024 * 1024,
            hard_pending_compaction_bytes_limit: 16 * 1024 * 1024,
            max_write_buffer_number: 2,
            max_background_compactions: 0,
            inline_compaction: Some(true),
            evict_compaction_data_from_page_cache: true,
            metadata_block_size: 1024,
            max_key_size: 16 * 1024,
            max_value_size: 256 * 1024,
            // An index over the buffer is a table plus an entry per key,
            // which is real memory on a budget this size. A transaction
            // here is expected to be small, so walk further before
            // spending it.
            transaction_keys_inline: 16,
            ..Self::default()
        }
    }

    /// Tuning for a wasm module: `wasm32-unknown-unknown` in a
    /// browser, or `wasm32-wasip1` under a wasi host.
    ///
    /// wasm is not just a small machine, and that is why this is a
    /// separate profile from [`Options::embedded`] rather than an
    /// alias for it. Three properties of the platform drive every
    /// value that differs.
    ///
    /// # What makes wasm different
    ///
    /// - **There are no threads, and there is no way to wait for
    ///   one.** [`Options::max_background_compactions`] is `0` and on
    ///   a wasm target [`Options::validate`] rejects anything else,
    ///   so the failure arrives at the option rather than out of the
    ///   middle of [`crate::Db::open`]. Compaction runs on whichever
    ///   thread asks for it. Every other value here is chosen so that
    ///   the writer paying that cost pays it in small installments.
    /// - **There is no OS page cache.** This is the sharpest split
    ///   from [`Options::embedded`], which sets
    ///   [`Options::block_cache_size`] to `0` precisely because a
    ///   Linux-class board has a page cache to absorb the re-reads. A
    ///   wasm module has none, so a cache miss costs a host call and,
    ///   because [`Options::compression`] defaults to
    ///   [`CompressionType::Lz4`], a fresh decompression every time.
    ///   The cache here holds decompressed blocks, so it buys back
    ///   both. It is 1 MiB, and one shard, because a single-threaded
    ///   host never contends on it.
    /// - **Linear memory grows in 64 KiB pages and never shrinks.**
    ///   Every peak is permanent for the life of the instance, so the
    ///   figure to keep down is the high-water mark, not the average.
    ///   [`ArenaProfile::EMBEDDED`] caps a chunk at exactly one page
    ///   and the memtable pool hands the same pages to the next
    ///   memtable, so `memory.grow` runs once per size class rather
    ///   than once per flush-and-refill cycle.
    ///
    /// # Where the budget goes
    ///
    /// Up to 2 MiB of memtable entry bytes (a 1 MiB write buffer, at
    /// most one active plus one frozen), 1 MiB of block cache, and
    /// per-SSTable index and Bloom residency that grows with the file
    /// count. Skip-list node overhead sits on top of the entry bytes.
    /// Past roughly 100 MiB of database, set
    /// [`Options::partitioned_index`] to `true` to stop the per-file
    /// index term from growing without bound;
    /// [`Options::metadata_block_size`] is already 1 KiB here so that
    /// switch needs no further retuning.
    ///
    /// Measured, not estimated. The `embedded_profile` example run on
    /// `wasm32-wasip1` under wasmtime (open, 20000 x 128 B puts,
    /// sampled reads, page scan, compact, close, reopen, read back)
    /// reaches **4480 KiB** of linear memory, of which 3392 KiB is
    /// over the pre-open baseline, against **1600 KiB** for
    /// [`Options::embedded`] and **11456 KiB** for
    /// [`Options::default`] on the same run. Most of the gap over
    /// `embedded` is the block cache this profile buys and `embedded`
    /// declines, plus the larger write buffer. Linear memory only
    /// grows, so for a wasm module the high-water mark *is* the
    /// footprint. Reproduce with `just wasm-budget`.
    ///
    /// [`Options::evict_compaction_data_from_page_cache`] is `false`,
    /// unlike [`Options::embedded`]: the hint is `posix_fadvise` on
    /// Linux and a no-op everywhere else, so asking for it on wasm
    /// would imply an effect the target cannot deliver.
    ///
    /// [`Options::max_value_size`] is 1 MiB, exactly one write
    /// buffer, which is the same rule [`Options::embedded`] follows:
    /// a write the API accepts must fit in a memtable that can hold
    /// it. Raising it above [`Options::write_buffer_size`] is
    /// supported and costs a flush per outsized value, which on wasm
    /// is also paid in pages that never come back.
    ///
    /// # Not a promise about your workload
    ///
    /// These values bound what regolith reserves. They do not bound what
    /// your keys and values cost, nor what the allocator does with
    /// fragmentation. Measure in the target runtime.
    ///
    /// # It does not choose a filesystem for you
    ///
    /// On `wasm32-wasip1` the default [`Options::env`] works: wasi has
    /// a real `std::fs` behind a preopened directory. On
    /// `wasm32-unknown-unknown` it does not, because that target has no
    /// filesystem at all and every `std::fs` call reports
    /// [`std::io::ErrorKind::Unsupported`]. In a browser, mount OPFS
    /// and set the env yourself:
    ///
    /// ```ignore
    /// let opfs = OpfsEnv::mount("my-db", OpfsOptions::default()).await?;
    /// let options = Options::wasm().env(opfs.as_env());
    /// let db = Db::open(opfs.db_path(), options)?;
    /// ```
    ///
    /// Mounting is async, which is why the profile cannot do it.
    ///
    /// The profile is a plain value with no wasm-only fields, so it
    /// builds and opens on the host too, which is what lets it be
    /// exercised by the normal test suite:
    ///
    /// ```
    /// use regolith::{Db, Options};
    ///
    /// let dir = tempfile::TempDir::new()?;
    /// let db = Db::open(dir.path(), Options::wasm())?;
    /// db.put(b"k", b"v")?;
    /// assert_eq!(db.get(b"k")?.as_deref(), Some(&b"v"[..]));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn wasm() -> Self {
        Self {
            write_buffer_size: 1024 * 1024,
            arena_profile: ArenaProfile::EMBEDDED,
            block_size: 4 * 1024,
            block_cache_size: 1024 * 1024,
            block_cache_num_shard_bits: 0,
            l0_compaction_trigger: 4,
            level_base_bytes: 8 * 1024 * 1024,
            target_file_size: 1024 * 1024,
            level0_slowdown_writes_trigger: 8,
            level0_stop_writes_trigger: 12,
            soft_pending_compaction_bytes_limit: 32 * 1024 * 1024,
            hard_pending_compaction_bytes_limit: 128 * 1024 * 1024,
            max_write_buffer_number: 2,
            max_background_compactions: 0,
            inline_compaction: Some(true),
            evict_compaction_data_from_page_cache: false,
            metadata_block_size: 1024,
            max_key_size: 16 * 1024,
            max_value_size: 1024 * 1024,
            // A browser tab's heap is not the server's, and a transaction
            // here is a page interaction rather than a bulk job.
            transaction_keys_inline: 16,
            ..Self::default()
        }
    }

    /// A database that lives only in memory. Nothing reaches a filesystem and
    /// nothing outlives the handle.
    ///
    /// The reason to reach for this rather than assembling it by hand is one
    /// non-obvious constraint: [`MemEnv`](crate::MemEnv) starts no threads, so
    /// a background compaction worker cannot exist on it and asking for one
    /// fails the open. Compaction runs on the calling thread instead. Building
    /// the options by hand means discovering that from an open error that does
    /// not mention the environment.
    ///
    /// Sized from [`Options::embedded`], because a process that opens a dozen
    /// of these for tests should not reserve a server's write buffer for each.
    /// Durability is [`DurabilityMode::Eventual`]: there is nothing to make
    /// durable, so an fsync per commit would be pure cost.
    ///
    /// ```
    /// use regolith::{Db, Options};
    ///
    /// let db = Db::open("in-memory", Options::memory())?;
    /// db.put(b"k", b"v")?;
    /// assert_eq!(db.get(b"k")?.as_deref(), Some(&b"v"[..]));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn memory() -> Self {
        Self {
            env: Arc::new(crate::env::MemEnv::new()),
            // Not a tuning choice: the environment cannot spawn.
            max_background_compactions: 0,
            durability: DurabilityMode::Eventual,
            ..Self::embedded()
        }
    }

    /// Validate option invariants before the database uses them.
    ///
    /// Public open paths call this automatically. Callers that build
    /// option sets dynamically can also call it directly to fail fast
    /// before touching the filesystem.
    pub fn validate(&self) -> crate::Result<()> {
        require_nonzero_usize("write_buffer_size", self.write_buffer_size)?;
        require_nonzero_usize("block_size", self.block_size)?;
        require_nonzero_usize("l0_compaction_trigger", self.l0_compaction_trigger)?;
        require_nonzero_u64("level_base_bytes", self.level_base_bytes)?;
        require_nonzero_u64("level_size_multiplier", self.level_size_multiplier)?;
        require_nonzero_u64("target_file_size", self.target_file_size)?;
        require_nonzero_usize("metadata_block_size", self.metadata_block_size)?;

        // A background compaction worker needs a thread, and not every
        // environment has one: wasm cannot spawn, and neither can `MemEnv`.
        // Reject it here so the reason arrives with the option that caused it,
        // naming the environment, rather than as an io error from the middle
        // of `open`.
        if self.max_background_compactions > 0 && !self.env.capabilities().threads {
            return invalid_option(
                "max_background_compactions",
                "must be 0 for an environment that cannot spawn threads, such as MemEnv \
                 or wasm; compaction then runs on the calling thread. Options::memory() \
                 and Options::wasm() set this along with the rest of their profile",
            );
        }

        if !self.arena_profile.is_valid() {
            return invalid_option(
                "arena_profile",
                "initial_chunk_size and max_chunk_size must be powers of two with initial <= max",
            );
        }

        // The memtable stores each entry's key and value length in a
        // `u32` inside its arena node, so an entry that could not be
        // encoded must be rejected at the boundary rather than at the
        // write. What the node holds is the *internal* key, which is the
        // caller's key plus the sequence and value-type suffix, so the
        // key ceiling is that much lower: allowing a key of exactly
        // `u32::MAX` would let a write pass validation, reach the WAL and
        // take a sequence number, and then be dropped by the memtable.
        const MAX_ENCODABLE_KEY: usize = u32::MAX as usize - INTERNAL_KEY_SUFFIX_LEN;
        if self.max_key_size > MAX_ENCODABLE_KEY {
            return invalid_option(
                "max_key_size",
                "must be <= u32::MAX minus the 9-byte internal-key suffix",
            );
        }
        if self.max_value_size > u32::MAX as usize {
            return invalid_option("max_value_size", "must be <= u32::MAX");
        }

        if self.block_cache_num_shard_bits > MAX_BLOCK_CACHE_SHARD_BITS {
            return invalid_option(
                "block_cache_num_shard_bits",
                format!("must be <= {MAX_BLOCK_CACHE_SHARD_BITS}"),
            );
        }

        if !(1..=MAX_BLOOM_BITS_PER_KEY).contains(&self.bloom_bits_per_key) {
            return invalid_option(
                "bloom_bits_per_key",
                format!("must be in 1..={MAX_BLOOM_BITS_PER_KEY}"),
            );
        }

        if self.level0_slowdown_writes_trigger > 0
            && self.level0_stop_writes_trigger > 0
            && self.level0_slowdown_writes_trigger > self.level0_stop_writes_trigger
        {
            return invalid_option(
                "level0_slowdown_writes_trigger",
                "must be <= level0_stop_writes_trigger when both triggers are nonzero",
            );
        }

        if self.soft_pending_compaction_bytes_limit > 0
            && self.hard_pending_compaction_bytes_limit > 0
            && self.soft_pending_compaction_bytes_limit > self.hard_pending_compaction_bytes_limit
        {
            return invalid_option(
                "soft_pending_compaction_bytes_limit",
                "must be <= hard_pending_compaction_bytes_limit when both limits are nonzero",
            );
        }

        // Fail loud rather than accepting a durability the platform
        // cannot keep: an env without `durable_sync` returns `Ok(())`
        // from `sync_all` without making anything durable, so honouring
        // `Immediate` there would report a guarantee that does not
        // exist. OPFS in mirror mode is the case this catches.
        if matches!(self.durability, DurabilityMode::Immediate)
            && !self.env.capabilities().durable_sync
        {
            return invalid_option(
                "durability",
                "DurabilityMode::Immediate needs an Env whose \
                 Capabilities::durable_sync is true; this one reports false, \
                 so a synced write would not be durable",
            );
        }

        match self.compaction_style {
            CompactionStyle::Level => {}
            CompactionStyle::Fifo => {
                require_nonzero_u64(
                    "fifo_compaction_options.max_table_files_size",
                    self.fifo_compaction_options.max_table_files_size,
                )?;
            }
            CompactionStyle::Universal => {
                let universal = self.universal_compaction_options;
                if universal.size_ratio == 0 {
                    return invalid_option(
                        "universal_compaction_options.size_ratio",
                        "must be greater than 0",
                    );
                }
                if universal.min_merge_width < 2 {
                    return invalid_option(
                        "universal_compaction_options.min_merge_width",
                        "must be at least 2",
                    );
                }
                if universal.max_merge_width < universal.min_merge_width {
                    return invalid_option(
                        "universal_compaction_options.max_merge_width",
                        "must be >= universal_compaction_options.min_merge_width",
                    );
                }
                if universal.max_size_amplification_percent == 0 {
                    return invalid_option(
                        "universal_compaction_options.max_size_amplification_percent",
                        "must be greater than 0",
                    );
                }
            }
        }

        Ok(())
    }

    pub(crate) fn to_engine_options(&self) -> crate::engine::EngineOptions {
        crate::engine::EngineOptions {
            write_buffer_size: self.write_buffer_size,
            arena_profile: self.arena_profile,
            block_size: self.block_size,
            block_cache_size: self.block_cache_size,
            block_cache_num_shard_bits: self.block_cache_num_shard_bits,
            strict_capacity_limit: self.strict_capacity_limit,
            bloom_bits_per_key: self.bloom_bits_per_key,
            compression: self.compression,
            compression_per_level: self.compression_per_level.clone(),
            l0_compaction_trigger: self.l0_compaction_trigger,
            level_base_bytes: self.level_base_bytes,
            level_size_multiplier: self.level_size_multiplier,
            target_file_size: self.target_file_size,
            compaction_filter: self.compaction_filter.clone(),
            prefix_extractor: self.prefix_extractor.clone(),
            merge_operator: self.merge_operator.clone(),
            listeners: self.listeners.clone(),
            transaction_hooks: self.transaction_hooks.clone(),
            keyring: self.keyring(),
            statistics: self.statistics.clone(),
            rate_limiter: self.rate_limiter.clone(),
            level0_slowdown_writes_trigger: self.level0_slowdown_writes_trigger,
            level0_stop_writes_trigger: self.level0_stop_writes_trigger,
            soft_pending_compaction_bytes_limit: self.soft_pending_compaction_bytes_limit,
            hard_pending_compaction_bytes_limit: self.hard_pending_compaction_bytes_limit,
            max_write_buffer_number: self.max_write_buffer_number,
            compaction_style: self.compaction_style,
            fifo_compaction_options: self.fifo_compaction_options,
            universal_compaction_options: self.universal_compaction_options,
            evict_compaction_data_from_page_cache: self.evict_compaction_data_from_page_cache,
            max_background_compactions: self.max_background_compactions,
            log_writeback: self.log_writeback,
            inline_compaction: self.inline_compaction_resolved(),
            partitioned_index: self.partitioned_index,
            cache_index_and_filter_blocks: self.cache_index_and_filter_blocks,
            metadata_block_size: self.metadata_block_size,
            read_only: self.read_only,
            max_key_size: self.max_key_size,
            max_value_size: self.max_value_size,
            env: if self.max_open_files > 0 {
                Arc::new(crate::env::OpenFileLimit::new(
                    Arc::clone(&self.env),
                    self.max_open_files,
                ))
            } else {
                Arc::clone(&self.env)
            },
        }
    }

    /// The keyring over [`Options::key_provider`], when one is set.
    pub(crate) fn keyring(&self) -> Option<Arc<crate::engine::seal::Keyring>> {
        self.key_provider
            .as_ref()
            .map(|p| Arc::new(crate::engine::seal::Keyring::new(Arc::clone(p))))
    }
}

fn require_nonzero_usize(name: &'static str, value: usize) -> crate::Result<()> {
    if value == 0 {
        invalid_option(name, "must be greater than 0")
    } else {
        Ok(())
    }
}

fn require_nonzero_u64(name: &'static str, value: u64) -> crate::Result<()> {
    if value == 0 {
        invalid_option(name, "must be greater than 0")
    } else {
        Ok(())
    }
}

fn invalid_option(name: &'static str, requirement: impl Into<String>) -> crate::Result<()> {
    Err(crate::Error::invalid_argument(format!(
        "invalid option `{name}`: {}",
        requirement.into()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_invalid_option(opts: Options, expected: &str) {
        match opts.validate().unwrap_err() {
            crate::Error::InvalidArgument(message) => {
                assert!(
                    message.contains(expected),
                    "expected invalid option message to contain {expected:?}, got {message:?}"
                );
            }
            other => panic!("expected invalid argument, got {other:?}"),
        }
    }

    #[test]
    fn fixed_length_prefix_extract() {
        let ex = FixedLengthPrefix(4);
        assert_eq!(ex.extract(b"tenant_001"), Some(&b"tena"[..]));
        assert_eq!(ex.extract(b"abcd"), Some(&b"abcd"[..]));
        assert_eq!(ex.extract(b"abc"), None);
        assert_eq!(ex.extract(b""), None);
        assert_eq!(ex.name(), "FixedLengthPrefix(4)");
        // Each width its own name, the same every time it is asked.
        assert_eq!(FixedLengthPrefix(4).name(), ex.name());
        assert_eq!(FixedLengthPrefix(65).name(), "FixedLengthPrefix(65)");
        assert_eq!(FixedLengthPrefix(65).name(), FixedLengthPrefix(65).name());
        assert_ne!(FixedLengthPrefix(3).name(), ex.name());
    }

    #[test]
    fn fixed_length_prefix_zero_accepts_any_key() {
        let ex = FixedLengthPrefix(0);
        assert_eq!(ex.extract(b"anything"), Some(&b""[..]));
        assert_eq!(ex.extract(b""), Some(&b""[..]));
    }

    #[test]
    fn compaction_decision_equality_and_clone() {
        assert_eq!(CompactionDecision::Keep, CompactionDecision::Keep);
        assert_ne!(CompactionDecision::Keep, CompactionDecision::Remove);
        let c = CompactionDecision::Change(b"new".to_vec());
        assert_eq!(c.clone(), c);
        assert_ne!(c, CompactionDecision::Change(b"other".to_vec()));
    }

    #[test]
    fn write_options_defaults_are_all_false() {
        let wo = WriteOptions::new();
        assert!(!wo.sync);
        assert!(!wo.disable_wal);
        assert!(!wo.low_pri);
        assert!(!wo.no_slowdown);
        assert_eq!(wo, WriteOptions::default());
    }

    #[test]
    fn write_options_sync_constructor_sets_only_sync() {
        let wo = WriteOptions::sync();
        assert!(wo.sync);
        assert!(!wo.disable_wal);
    }

    #[test]
    fn write_options_disable_wal_constructor_sets_only_disable_wal() {
        let wo = WriteOptions::disable_wal();
        assert!(wo.disable_wal);
        assert!(!wo.sync);
    }

    #[test]
    fn compaction_style_default_is_level() {
        assert_eq!(CompactionStyle::default(), CompactionStyle::Level);
    }

    #[test]
    fn fifo_compaction_options_default_is_one_gib() {
        let f = FifoCompactionOptions::default();
        assert_eq!(f.max_table_files_size, 1024 * 1024 * 1024);
    }

    #[test]
    fn options_validate_accepts_defaults_and_disabled_stall_triggers() {
        Options::default().validate().unwrap();

        let opts = Options::default()
            .level0_slowdown_writes_trigger(0)
            .level0_stop_writes_trigger(0)
            .soft_pending_compaction_bytes_limit(0)
            .hard_pending_compaction_bytes_limit(0)
            .max_write_buffer_number(0);
        opts.validate().unwrap();
    }

    #[test]
    fn options_validate_accepts_zero_block_cache_size() {
        Options::default().block_cache_size(0).validate().unwrap();
    }

    #[test]
    fn zero_block_cache_db_opens_and_reads_correctly() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = crate::Db::open(
            dir.path(),
            Options::default()
                .block_cache_size(0)
                .write_buffer_size(4 * 1024),
        )
        .unwrap();
        for i in 0..500u32 {
            crate::through_stalls(|| {
                db.put(
                    format!("key{i:04}").as_bytes(),
                    format!("value{i}").as_bytes(),
                )
            })
            .unwrap();
        }
        // Small write buffer forces flushes, so most of these reads
        // come from SSTables and exercise the uncached block path.
        for i in 0..500u32 {
            assert_eq!(
                db.get(format!("key{i:04}").as_bytes()).unwrap(),
                Some(format!("value{i}").into_bytes())
            );
        }
        assert!(db.get(b"absent").unwrap().is_none());
    }

    #[test]
    fn options_validate_rejects_zero_core_sizes() {
        assert_invalid_option(Options::default().write_buffer_size(0), "write_buffer_size");
        assert_invalid_option(Options::default().block_size(0), "block_size");
        assert_invalid_option(
            Options::default().l0_compaction_trigger(0),
            "l0_compaction_trigger",
        );
        assert_invalid_option(Options::default().level_base_bytes(0), "level_base_bytes");
        assert_invalid_option(
            Options::default().level_size_multiplier(0),
            "level_size_multiplier",
        );
        assert_invalid_option(Options::default().target_file_size(0), "target_file_size");
        assert_invalid_option(
            Options::default().metadata_block_size(0),
            "metadata_block_size",
        );
    }

    #[test]
    fn options_validate_rejects_unsupported_ranges() {
        assert_invalid_option(
            Options::default().block_cache_num_shard_bits(MAX_BLOCK_CACHE_SHARD_BITS + 1),
            "block_cache_num_shard_bits",
        );
        assert_invalid_option(
            Options::default().bloom_bits_per_key(0),
            "bloom_bits_per_key",
        );
        assert_invalid_option(
            Options::default().bloom_bits_per_key(MAX_BLOOM_BITS_PER_KEY + 1),
            "bloom_bits_per_key",
        );
    }

    #[test]
    fn zero_background_compactions_is_a_supported_configuration() {
        Options::default()
            .max_background_compactions(0)
            .validate()
            .expect("zero background workers selects foreground compaction, not an error");
    }

    #[test]
    fn embedded_profile_validates() {
        Options::embedded()
            .validate()
            .expect("the shipped embedded profile must satisfy its own invariants");
    }

    #[test]
    fn embedded_profile_bounds_every_resident_term() {
        let o = Options::embedded();

        // Entry bytes held in memory at once: two write buffers.
        assert_eq!(o.write_buffer_size, 256 * 1024);
        assert_eq!(o.max_write_buffer_number, 2);

        // The cache must allocate nothing at all, not merely a little.
        assert_eq!(o.block_cache_size, 0);
        assert_eq!(o.block_cache_num_shard_bits, 0);

        // Per-file metadata and transient buffers.
        assert_eq!(o.block_size, 4 * 1024);
        assert_eq!(o.target_file_size, 256 * 1024);
        assert_eq!(o.metadata_block_size, 1024);

        // A single accepted write must not exceed one write buffer,
        // or the memtable cannot hold what the API just admitted.
        assert!(o.max_value_size <= o.write_buffer_size);
        assert!(o.max_key_size < o.write_buffer_size);

        // Compaction runs on the calling thread.
        assert_eq!(o.max_background_compactions, 0);
    }

    #[test]
    fn wasm_profile_validates() {
        Options::wasm()
            .validate()
            .expect("the shipped wasm profile must satisfy its own invariants");
    }

    #[test]
    fn wasm_profile_bounds_every_resident_term() {
        let o = Options::wasm();

        // Entry bytes held in memory at once: two write buffers.
        assert_eq!(o.write_buffer_size, 1024 * 1024);
        assert_eq!(o.max_write_buffer_number, 2);

        // A chunk is exactly one wasm page, so a chunk never straddles
        // more pages than its contents need.
        assert_eq!(o.arena_profile.max_chunk_size, 64 * 1024);

        // Unlike `embedded`, the cache is real: wasm has no OS page
        // cache behind it. One shard, because nothing contends.
        assert_eq!(o.block_cache_size, 1024 * 1024);
        assert_eq!(o.block_cache_num_shard_bits, 0);

        // The same admission rule `embedded` holds: a write the API
        // accepts must fit in a memtable that can hold it.
        assert!(o.max_value_size <= o.write_buffer_size);
        assert!(o.max_key_size < o.write_buffer_size);

        // Compaction runs on the calling thread. This is not a tuning
        // choice on wasm; `validate` rejects anything else there.
        assert_eq!(o.max_background_compactions, 0);

        // The page-cache hint is a no-op on wasm, so the profile does
        // not ask for an effect the target cannot deliver.
        assert!(!o.evict_compaction_data_from_page_cache);

        // Already set for the `partitioned_index` switch the doc
        // points at, so flipping it later needs no further retuning.
        assert_eq!(o.metadata_block_size, 1024);
    }

    #[test]
    fn wasm_profile_is_smaller_than_the_default_but_roomier_than_embedded() {
        let d = Options::default();
        let w = Options::wasm();
        let e = Options::embedded();

        assert!(w.write_buffer_size < d.write_buffer_size);
        assert!(w.block_cache_size < d.block_cache_size);
        assert!(w.max_value_size < d.max_value_size);
        assert!(w.hard_pending_compaction_bytes_limit < d.hard_pending_compaction_bytes_limit);

        // Roomier than `embedded` everywhere it differs, because a
        // browser tab or a wasi host is not an ESP32.
        assert!(w.write_buffer_size > e.write_buffer_size);
        assert!(w.target_file_size > e.target_file_size);
        assert!(w.level_base_bytes > e.level_base_bytes);
        assert!(w.max_value_size > e.max_value_size);

        // The one term that goes the other way, and the reason this is
        // a separate profile rather than an alias for `embedded`.
        assert_eq!(e.block_cache_size, 0);
        assert!(w.block_cache_size > e.block_cache_size);
    }

    #[test]
    fn default_profile_opens_on_every_target_it_is_compiled_for() {
        // `Db::open` validates, so a default that a target rejects
        // would make the zero-configuration path unopenable there.
        Options::default()
            .validate()
            .expect("Options::default must be valid on the target it was built for");
        #[cfg(target_family = "wasm")]
        assert_eq!(Options::default().max_background_compactions, 0);
    }

    #[test]
    fn embedded_profile_is_strictly_smaller_than_the_default() {
        let d = Options::default();
        let e = Options::embedded();
        assert!(e.write_buffer_size < d.write_buffer_size);
        assert!(e.block_cache_size < d.block_cache_size);
        assert!(e.block_size < d.block_size);
        assert!(e.target_file_size < d.target_file_size);
        assert!(e.level_base_bytes < d.level_base_bytes);
        assert!(e.max_key_size < d.max_key_size);
        assert!(e.max_value_size < d.max_value_size);
        assert!(e.level0_stop_writes_trigger < d.level0_stop_writes_trigger);
        assert!(e.hard_pending_compaction_bytes_limit < d.hard_pending_compaction_bytes_limit);
    }

    #[test]
    fn options_validate_rejects_inconsistent_stall_thresholds() {
        assert_invalid_option(
            Options::default()
                .level0_slowdown_writes_trigger(10)
                .level0_stop_writes_trigger(5),
            "level0_slowdown_writes_trigger",
        );
        assert_invalid_option(
            Options::default()
                .soft_pending_compaction_bytes_limit(10)
                .hard_pending_compaction_bytes_limit(5),
            "soft_pending_compaction_bytes_limit",
        );
    }

    #[test]
    fn options_validate_checks_selected_compaction_style_options() {
        Options::default()
            .fifo_compaction_options(FifoCompactionOptions {
                max_table_files_size: 0,
            })
            .validate()
            .unwrap();

        assert_invalid_option(
            Options::default()
                .compaction_style(CompactionStyle::Fifo)
                .fifo_compaction_options(FifoCompactionOptions {
                    max_table_files_size: 0,
                }),
            "fifo_compaction_options.max_table_files_size",
        );
        assert_invalid_option(
            Options::default()
                .compaction_style(CompactionStyle::Universal)
                .universal_compaction_options(UniversalCompactionOptions {
                    size_ratio: 0,
                    ..UniversalCompactionOptions::default()
                }),
            "universal_compaction_options.size_ratio",
        );
        assert_invalid_option(
            Options::default()
                .compaction_style(CompactionStyle::Universal)
                .universal_compaction_options(UniversalCompactionOptions {
                    min_merge_width: 1,
                    ..UniversalCompactionOptions::default()
                }),
            "universal_compaction_options.min_merge_width",
        );
        assert_invalid_option(
            Options::default()
                .compaction_style(CompactionStyle::Universal)
                .universal_compaction_options(UniversalCompactionOptions {
                    min_merge_width: 4,
                    max_merge_width: 3,
                    ..UniversalCompactionOptions::default()
                }),
            "universal_compaction_options.max_merge_width",
        );
        assert_invalid_option(
            Options::default()
                .compaction_style(CompactionStyle::Universal)
                .universal_compaction_options(UniversalCompactionOptions {
                    max_size_amplification_percent: 0,
                    ..UniversalCompactionOptions::default()
                }),
            "universal_compaction_options.max_size_amplification_percent",
        );
    }

    #[test]
    fn embedded_preset_is_valid_and_small() {
        let opts = Options::embedded();
        opts.validate()
            .expect("the preset must be a valid option set");
        assert_eq!(opts.arena_profile, ArenaProfile::EMBEDDED);
        assert_eq!(
            opts.to_engine_options().arena_profile,
            ArenaProfile::EMBEDDED
        );
        // The documented bound: 2 * M * (W + c) + M * W.
        let w = opts.write_buffer_size;
        let c = opts.arena_profile.max_chunk_size;
        let m = opts.max_write_buffer_number;
        assert_eq!(2 * m * (w + c) + m * w, 1792 * 1024);
    }

    #[test]
    fn arena_profile_validation_rejects_bad_shapes() {
        let bad = [
            ArenaProfile {
                initial_chunk_size: 3000,
                max_chunk_size: 64 * 1024,
            },
            ArenaProfile {
                initial_chunk_size: 4096,
                max_chunk_size: 3000,
            },
            ArenaProfile {
                initial_chunk_size: 128 * 1024,
                max_chunk_size: 64 * 1024,
            },
            ArenaProfile {
                initial_chunk_size: 0,
                max_chunk_size: 64 * 1024,
            },
        ];
        for profile in bad {
            assert!(!profile.is_valid(), "{profile:?} must be rejected");
            let opts = Options::default().arena_profile(profile);
            match opts.validate() {
                Err(crate::Error::InvalidArgument(message)) => {
                    assert!(message.contains("arena_profile"), "{message}")
                }
                other => panic!("expected an arena_profile error, got {other:?}"),
            }
        }
        assert!(ArenaProfile::SERVER.is_valid());
        assert!(ArenaProfile::EMBEDDED.is_valid());
        assert_eq!(ArenaProfile::default(), ArenaProfile::SERVER);
    }

    #[test]
    fn key_and_value_size_caps_are_encodable() {
        // The memtable stores both lengths in a `u32` inside its arena
        // node, so an unencodable limit has to fail at the boundary.
        for (name, opts) in [
            (
                "max_key_size",
                Options::default().max_key_size(u32::MAX as usize + 1),
            ),
            (
                "max_value_size",
                Options::default().max_value_size(u32::MAX as usize + 1),
            ),
        ] {
            match opts.validate() {
                Err(crate::Error::InvalidArgument(message)) => {
                    assert!(message.contains(name), "{message}")
                }
                other => panic!("expected a {name} error, got {other:?}"),
            }
        }
    }

    #[test]
    fn options_to_engine_options_preserves_engine_fields() {
        // Defaults consumed by the engine must survive the translation
        // unchanged; compatibility-only public knobs are intentionally
        // not present in EngineOptions.
        let opts = Options::default();
        let eo = opts.to_engine_options();
        assert_eq!(eo.write_buffer_size, opts.write_buffer_size);
        assert_eq!(eo.block_size, opts.block_size);
        assert_eq!(eo.block_cache_size, opts.block_cache_size);
        assert_eq!(eo.bloom_bits_per_key, opts.bloom_bits_per_key);
        assert_eq!(eo.l0_compaction_trigger, opts.l0_compaction_trigger);
        assert_eq!(eo.level_base_bytes, opts.level_base_bytes);
        assert_eq!(eo.level_size_multiplier, opts.level_size_multiplier);
        assert_eq!(eo.target_file_size, opts.target_file_size);
        assert_eq!(eo.compaction_style, opts.compaction_style);
        assert_eq!(eo.read_only, opts.read_only);
        assert_eq!(eo.max_key_size, opts.max_key_size);
        assert_eq!(eo.max_value_size, opts.max_value_size);
        assert_eq!(
            eo.max_background_compactions,
            opts.max_background_compactions
        );
        assert_eq!(
            eo.evict_compaction_data_from_page_cache,
            opts.evict_compaction_data_from_page_cache
        );
        assert_eq!(eo.partitioned_index, opts.partitioned_index);
        assert_eq!(
            eo.cache_index_and_filter_blocks,
            opts.cache_index_and_filter_blocks
        );
        assert!(
            !opts.log_writeback,
            "the log writeback lane is off by default"
        );
        assert!(
            Options::default()
                .log_writeback(true)
                .to_engine_options()
                .log_writeback
        );
    }

    /// Custom `PrefixExtractor` that returns everything before the
    /// first `:` - proves the trait is user-implementable.
    #[test]
    fn custom_prefix_extractor_can_be_plugged_in() {
        struct UntilColon;
        impl PrefixExtractor for UntilColon {
            fn extract<'a>(&self, key: &'a [u8]) -> Option<&'a [u8]> {
                key.iter().position(|&b| b == b':').map(|i| &key[..i])
            }
            fn name(&self) -> &'static str {
                "UntilColon"
            }
        }
        let ex: Arc<dyn PrefixExtractor> = Arc::new(UntilColon);
        assert_eq!(ex.extract(b"tenant:key"), Some(&b"tenant"[..]));
        assert_eq!(ex.extract(b"nocolon"), None);
        assert_eq!(ex.name(), "UntilColon");
    }
}
