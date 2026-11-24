//! What a successful commit hands back.

/// The outcome of a successful [`crate::Transaction::commit`].
///
/// Its sequence orders commits within one database: work that must follow the
/// order the commits became visible in can sort by it, with no per-key lock.
/// The sequence orders work within one database epoch only. `drop_all` resets
/// the engine's sequence, and a new or restored database starts low, so a
/// caller must not keep a sequence across either.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct CommitReceipt {
    seq: u64,
}

impl CommitReceipt {
    pub(crate) fn new(seq: u64) -> Self {
        Self { seq }
    }

    /// The sequence at which the commit's writes became visible. A commit
    /// that wrote nothing reports the sequence of its snapshot, also when a
    /// pessimistic `get_for_update` served one of its reads later than that.
    pub fn seq(&self) -> u64 {
        self.seq
    }
}
