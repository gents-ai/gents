//! How a commit judges one read against the versions committed after it.
//!
//! The transaction layer decides the rule when it builds the validation set;
//! the engine applies it when the commit's slot is decided, only to a key whose newest
//! version is newer than the read. A key nothing touched since the read costs
//! the one lookup of its newest version whatever the rule.

use std::sync::Arc;

/// The rule that decides whether a newer version of a key makes a read stale.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReadRule {
    /// Any newer version is a change: the rule of every level but DefraLevel.
    Seq,
    /// DefraLevel's rule. A newer version is a change unless the key's newest
    /// version is a put (or a delete, for a read that found nothing) that
    /// leaves exactly the bytes the read returned, so an identical rewrite is
    /// not one. An operand on top is always one.
    Value,
    /// A projected read. A newer version is a change only if it is a put,
    /// delete or covering range delete that does not leave the bytes read, or
    /// an operand the merge operator says touches one of these parts (sorted,
    /// never empty). A read that found nothing is judged by `Value`, since
    /// existence is part of every decision on parts.
    Parts(Arc<[u32]>),
}
