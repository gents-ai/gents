//! The key layout of a commit-ordered log.
//!
//! A commit-ordered log is a run of entries numbered 1, 2, 3, ... in the order
//! their transactions commit. [`crate::Transaction::append`] adds an entry,
//! and regolith picks its position while the commit is being ordered, so
//! positions follow commit order and have no holes.
//! [`crate::Transaction::truncate_log`] drops a range of entries, and the
//! head keeps counting from where it was. regolith does not know what the
//! keys mean: a [`LogLayout`] tells it where the log keeps them.

/// Where one commit-ordered log keeps its keys. Implement it once per log and
/// share it behind an [`Arc`](std::sync::Arc).
///
/// A log owns three kinds of key, and no key belongs to two logs:
///
/// - the **head key**, which holds the newest position as an 8-byte
///   big-endian `u64`, absent while the log is empty;
/// - one **entry key** per position, which holds the entry's bytes;
/// - optionally one **once key** per `append` call that names one, which holds
///   the position its entry was given as an 8-byte big-endian `u64`.
///
/// regolith writes all three, in the same atomic commit that numbers the
/// entry, and `truncate_log` deletes entry keys. A caller reads them like any
/// other key, and must not write them. A reader that reads the head key in a
/// snapshot and then the entries up to it finds every entry present that no
/// commit at or before the snapshot truncated, because positions are numbered
/// in commit order and become visible with their commit.
///
/// # Contract
///
/// The methods run inside the ordered step of a commit, which holds up every
/// other writer, so each must be quick, pure and deterministic: no I/O, no
/// blocking, no panic, and no call back into the database. The same input
/// always gives the same output, for the life of the database and across
/// reopens, since positions already written are never renumbered.
///
/// - [`head_key`](LogLayout::head_key) never changes and is not an entry key
///   of any position.
/// - [`entry_key`](LogLayout::entry_key) gives a different key for every
///   position, and none is longer than
///   [`max_entry_key_len`](LogLayout::max_entry_key_len).
///
/// Breaking the contract is not detected in full. A layout that gives two
/// positions one key overwrites the earlier entry; a layout whose keys
/// overlap another log's mixes the two logs. regolith checks only what it
/// can cheaply: that no entry key is longer than the declared maximum, and,
/// when a [`crate::KeyClassifier`] is installed, that the keys are classified
/// [`crate::KeyClass::Log`].
///
/// # Example
///
/// A log of rows: the head at `journal-head`, and each entry at `journal/`
/// followed by its position as eight big-endian bytes, so the entries sort
/// in position order and the key is built without allocating.
///
/// ```
/// use std::sync::Arc;
///
/// use regolith::{LogLayout, OptimisticTransactionDb, Options, TxnOptions};
///
/// struct Journal;
///
/// impl LogLayout for Journal {
///     fn head_key(&self) -> &[u8] {
///         b"journal-head"
///     }
///
///     fn entry_key(&self, position: u64, out: &mut Vec<u8>) {
///         out.extend_from_slice(b"journal/");
///         out.extend_from_slice(&position.to_be_bytes());
///     }
///
///     fn max_entry_key_len(&self) -> usize {
///         b"journal/".len() + 8
///     }
/// }
///
/// let db = OptimisticTransactionDb::open("journal", Options::memory())?;
/// let journal: Arc<dyn LogLayout> = Arc::new(Journal);
///
/// let first = db.begin(&TxnOptions::new());
/// first.append(&journal, b"row one", None)?;
/// first.append(&journal, b"row two", None)?;
/// first.commit()?;
/// let second = db.begin(&TxnOptions::new());
/// second.append(&journal, b"row three", None)?;
/// second.commit()?;
///
/// // A reader takes the head in a snapshot, then every entry up to it:
/// // positions are dense from 1, in commit order.
/// let reader = db.db().snapshot();
/// let head = reader.get(Journal.head_key())?.expect("the log has entries");
/// let head = u64::from_be_bytes(head.try_into().expect("eight bytes"));
/// let mut rows = Vec::new();
/// for position in 1..=head {
///     let mut key = Vec::new();
///     Journal.entry_key(position, &mut key);
///     rows.push(reader.get(&key)?.expect("no position is skipped"));
/// }
/// assert_eq!(rows, [&b"row one"[..], b"row two", b"row three"]);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub trait LogLayout: Send + Sync + 'static {
    /// The key holding the newest assigned position as an 8-byte big-endian
    /// `u64`. It is absent while the log is empty, and any other length is
    /// reported as corruption by the commit that reads it.
    fn head_key(&self) -> &[u8];

    /// Appends the key of the entry at `position` to `out`.
    ///
    /// `out` is empty when called. The implementation only extends it, and
    /// leaves no bytes in it that are not part of the key.
    fn entry_key(&self, position: u64, out: &mut Vec<u8>);

    /// An upper bound, in bytes, on the length of every
    /// [`entry_key`](LogLayout::entry_key).
    ///
    /// `append` and `truncate_log` refuse a layout whose key for the largest
    /// position is longer, and a commit refuses to write an entry key that is
    /// longer, so a commit's size is known before it waits for a place in the
    /// log.
    fn max_entry_key_len(&self) -> usize;
}
