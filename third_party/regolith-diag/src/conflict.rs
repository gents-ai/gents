//! Why a commit lost a race.
//!
//! A [`Conflict`] names what the losing transaction did with a key
//! ([`Access`]), what the newer committed write was ([`WriteKind`]) and the two
//! sequences involved. It is built only when a commit actually conflicts, so a
//! commit that wins pays nothing for it.
//!
//! A key can hold user data, so neither `Display` nor `Debug` of a
//! [`Conflict`] prints its bytes: both print the key's length and a short
//! hash, which is enough to tell whether two conflicts are on the same key.

use std::fmt;

use xxhash_rust::xxh3::xxh3_64;

use crate::column_family::CF_PREFIX_LEN;

/// A key as an error message may show it: its length and a short hash, never
/// its bytes, since a key can hold user data. Equal keys show the same hash,
/// so two messages can be told to name one key.
pub(crate) struct RedactedKey<'a>(pub(crate) &'a [u8]);

impl RedactedKey<'_> {
    /// A 32-bit digest of the key, for telling errors on one key from errors
    /// on another without printing it.
    fn hash(&self) -> u32 {
        xxh3_64(self.0) as u32
    }
}

impl fmt::Display for RedactedKey<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-byte key (hash {:08x})", self.0.len(), self.hash())
    }
}

/// What a transaction did with a key that a newer write then overtook.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Access {
    /// A point read of the key, which found a value or found nothing.
    Read,
    /// A read of selected parts of the key's value (a projected read).
    ReadParts,
    /// A read that found the key and relied on its presence only, because its
    /// class says its bytes never differ.
    ReadPresence,
    /// A read through [`crate::Transaction::get_for_update`].
    ReadForUpdate,
    /// A write to a key that lies inside a stretch this transaction scanned.
    ScannedThenWrote,
    /// A validated scan of a range. The conflict names the key that landed in
    /// the range.
    ScannedRange,
    /// A put of the key.
    Put,
    /// A delete of the key.
    Delete,
    /// A merge into the key.
    Merge,
}

impl Access {
    /// The phrase [`Conflict`]'s message uses for this access.
    fn phrase(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::ReadParts => "partial read",
            Self::ReadPresence => "presence check",
            Self::ReadForUpdate => "read for update",
            Self::ScannedThenWrote => "write after a scan",
            Self::ScannedRange => "range scan",
            Self::Put => "put",
            Self::Delete => "delete",
            Self::Merge => "merge",
        }
    }
}

/// The kind of committed write that won a race.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WriteKind {
    /// A put of a value.
    Put,
    /// A delete of one key.
    Delete,
    /// A delete of a range that covers the key.
    RangeDelete,
    /// A merge operand.
    Merge,
}

impl WriteKind {
    /// Whether the write removed the key.
    pub(crate) fn is_deletion(self) -> bool {
        matches!(self, Self::Delete | Self::RangeDelete)
    }

    /// The phrase [`Conflict`]'s message uses for this write.
    fn phrase(self) -> &'static str {
        match self {
            Self::Put => "put",
            Self::Delete => "delete",
            Self::RangeDelete => "range delete",
            Self::Merge => "merge",
        }
    }
}

/// Why a commit lost a race: the key, what this transaction did with it, and
/// the newer committed write that decided the race.
///
/// Carried by [`crate::TransactionError::Conflict`] and handed to
/// [`crate::EventListener::on_conflict`]. Neither its `Display` nor its
/// `Debug` prints the key; use [`Conflict::key`] when the bytes are wanted.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Conflict {
    key: Vec<u8>,
    mine: Access,
    theirs: WriteKind,
    observed_seq: u64,
    latest_seq: u64,
}

impl Conflict {
    pub(crate) fn new(
        key: Vec<u8>,
        mine: Access,
        theirs: WriteKind,
        observed_seq: u64,
        latest_seq: u64,
    ) -> Self {
        Self {
            key,
            mine,
            theirs,
            observed_seq,
            latest_seq,
        }
    }

    /// Drop the column-family prefix, so the key is the one the caller passed.
    pub(crate) fn strip_cf_prefix(&mut self) {
        if self.key.len() >= CF_PREFIX_LEN {
            self.key.drain(..CF_PREFIX_LEN);
        }
    }

    /// The key, without the column-family prefix.
    pub fn key(&self) -> &[u8] {
        &self.key
    }

    /// What this transaction did with the key.
    pub fn mine(&self) -> Access {
        self.mine
    }

    /// The newer committed write that decided the race.
    pub fn theirs(&self) -> WriteKind {
        self.theirs
    }

    /// The sequence this transaction validated the key against.
    pub fn observed_seq(&self) -> u64 {
        self.observed_seq
    }

    /// The sequence of the newer write that [`Conflict::theirs`] names.
    pub fn latest_seq(&self) -> u64 {
        self.latest_seq
    }
}

impl fmt::Display for Conflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "transaction conflict on a {}: this transaction's {} at seq {} \
             was overtaken by a {} at seq {}; retry the transaction",
            RedactedKey(&self.key),
            self.mine.phrase(),
            self.observed_seq,
            self.theirs.phrase(),
            self.latest_seq,
        )
    }
}

impl fmt::Debug for Conflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Conflict")
            .field("key_len", &self.key.len())
            .field(
                "key_hash",
                &format_args!("{:08x}", RedactedKey(&self.key).hash()),
            )
            .field("mine", &self.mine)
            .field("theirs", &self.theirs)
            .field("observed_seq", &self.observed_seq)
            .field("latest_seq", &self.latest_seq)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conflict(key: &[u8]) -> Conflict {
        Conflict::new(key.to_vec(), Access::Read, WriteKind::Delete, 3, 9)
    }

    #[test]
    fn neither_display_nor_debug_prints_the_key() {
        let c = conflict(b"SECRET-user-key");
        for text in [c.to_string(), format!("{c:?}"), format!("{c:#?}")] {
            assert!(!text.contains("SECRET"), "{text}");
            assert!(!text.contains("user-key"), "{text}");
            assert!(!text.contains("83, 69, 67"), "{text}");
        }
    }

    #[test]
    fn the_message_names_the_length_hash_reason_and_sequences() {
        let text = conflict(b"abcd").to_string();
        assert!(text.contains("4-byte key"), "{text}");
        assert!(
            text.contains(&format!("hash {:08x}", xxh3_64(b"abcd") as u32)),
            "{text}"
        );
        assert!(text.contains("read at seq 3"), "{text}");
        assert!(text.contains("delete at seq 9"), "{text}");
    }

    #[test]
    fn the_hash_tells_keys_apart() {
        let shown = |key: &[u8]| RedactedKey(key).to_string();
        assert_eq!(shown(b"a"), shown(b"a"));
        assert_ne!(shown(b"a"), shown(b"b"));
    }

    #[test]
    fn stripping_the_prefix_leaves_the_callers_key() {
        let mut c = conflict(&[0, 0, 0, 0, b'k']);
        c.strip_cf_prefix();
        assert_eq!(c.key(), b"k");
    }
}
