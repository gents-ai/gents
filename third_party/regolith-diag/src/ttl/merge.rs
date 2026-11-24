//! The caller's merge operator on a TTL database, and how a merged key is
//! stored.
//!
//! Each write expires by its own time: a put and each merge operand live
//! the TTL from when they were written, and a read folds the writes of a
//! key that have not expired. A fold cannot be undone, and one value has one
//! time, so the writes of a merged key are not folded where they are stored:
//! the database's merge keeps them apart, each with its time, in one stored
//! value (a *bundle*), and the caller's operator folds the ones that have
//! not expired when the key is read. Compaction drops a write from a bundle
//! once it has expired, which a read was ignoring already, so what a read
//! returns does not depend on which writes a compaction pass met, nor on
//! when it ran.
//!
//! A merge with no time was written past the wrapper (through
//! [`super::DbWithTtl::inner`]), or by release 0.1.9, which stamped puts but
//! not merge operands. It is folded the way a database without TTL folds
//! it, which is how 0.1.9 read it: the caller's operator is shown the writes
//! before it as they are stored, stamps and all, and what it returns is
//! read as a plain stamped value, which the timed merges after it apply to.
//! A write that has expired is left out of that fold as well, so compaction
//! dropping it changes nothing. A run of writes with no time at the front of
//! a key never expires, so the database's merge folds that run where it is
//! stored, and a key merged past the wrapper stays one value, as on a
//! database without TTL.
//!
//! A bundle, after the writes it holds:
//!
//! ```text
//! [write]... ["RTTL"][version 2][newest time: u64 BE]
//! write = [flags: u8 (1 the put the merges apply to, 2 has a time)]
//!         [length: varint][bytes][time: u64 BE, when it has one]
//! ```

use std::borrow::Cow;
use std::sync::Arc;

use super::{
    TTL_MAGIC, TTL_MAGIC_LEN, TTL_SUFFIX_LEN, TTL_TS_LEN, decode_ttl_suffix, is_live, live_value,
    stamp,
};
use crate::engine::block::{encode_varint, read_varint};
use crate::options::MergeOperator;

/// The version byte of a bundle's suffix (a plain stamped value has 1).
const BUNDLE_VERSION: u8 = 2;

/// The flag of the put a bundle's merges apply to.
const FLAG_BASE: u8 = 1;

/// The flag of a write that carries the time it was written at.
const FLAG_TIMED: u8 = 2;

/// One write a stored value holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Piece<'a> {
    /// The put the merges after it apply to.
    pub(super) base: bool,
    /// What the caller wrote.
    pub(super) value: &'a [u8],
    /// When, or `None` for a write with no time. Such a write never ends in
    /// a stamp: a stamped one has the stamp's time.
    pub(super) at: Option<u64>,
}

/// The writes `pieces`, kept apart in one bundle.
pub(super) fn encode<'a>(pieces: impl IntoIterator<Item = Piece<'a>>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut newest = 0u64;
    for piece in pieces {
        let flags = u8::from(piece.base) * FLAG_BASE + u8::from(piece.at.is_some()) * FLAG_TIMED;
        out.push(flags);
        encode_varint(&mut out, piece.value.len() as u64);
        out.extend_from_slice(piece.value);
        if let Some(at) = piece.at {
            out.extend_from_slice(&at.to_be_bytes());
            newest = newest.max(at);
        }
    }
    out.extend_from_slice(&TTL_MAGIC);
    out.push(BUNDLE_VERSION);
    out.extend_from_slice(&newest.to_be_bytes());
    out
}

/// The writes `stored` holds when it is a bundle, or `None` when it is not
/// one, or is not a whole one.
pub(super) fn decode(stored: &[u8]) -> Option<Vec<Piece<'_>>> {
    let body_len = stored.len().checked_sub(TTL_SUFFIX_LEN)?;
    let (body, suffix) = stored.split_at(body_len);
    if suffix[..TTL_MAGIC_LEN] != TTL_MAGIC || suffix[TTL_MAGIC_LEN] != BUNDLE_VERSION {
        return None;
    }
    let mut pieces = Vec::new();
    let mut offset = 0;
    while let Some(&flags) = body.get(offset) {
        if flags & !(FLAG_BASE | FLAG_TIMED) != 0 {
            return None;
        }
        offset += 1;
        let len = read_varint(body, &mut offset).ok()?;
        let value = body.get(offset..offset.checked_add(len)?)?;
        offset += len;
        let at = if flags & FLAG_TIMED != 0 {
            let (at, _) = body.get(offset..)?.split_first_chunk::<TTL_TS_LEN>()?;
            offset += TTL_TS_LEN;
            Some(u64::from_be_bytes(*at))
        } else {
            None
        };
        pieces.push(Piece {
            base: flags & FLAG_BASE != 0,
            value,
            at,
        });
    }
    Some(pieces)
}

/// The one write a plain value or operand is: one this wrapper stamped, or
/// one with no stamp, written past it or by 0.1.9.
fn plain_piece(stored: &[u8], base: bool) -> Piece<'_> {
    match decode_ttl_suffix(stored) {
        Some(decoded) => Piece {
            base,
            value: &stored[..decoded.value_len],
            at: Some(decoded.timestamp),
        },
        None => Piece {
            base,
            value: stored,
            at: None,
        },
    }
}

/// The writes a stored value or operand holds: a bundle's own, or the one
/// write a plain value or operand is, a put the merges apply to when `base`.
fn pieces_of(stored: &[u8], base: bool) -> Vec<Piece<'_>> {
    decode(stored).unwrap_or_else(|| vec![plain_piece(stored, base)])
}

/// A merge with no time.
fn untimed_merge(piece: &Piece<'_>) -> bool {
    !piece.base && piece.at.is_none()
}

/// The put `pieces` start with, if they do, and the merges after it.
fn split_base<'p, 'a>(pieces: &'p [Piece<'a>]) -> (Option<&'p Piece<'a>>, &'p [Piece<'a>]) {
    match pieces.split_first() {
        Some((first, rest)) if first.base => (Some(first), rest),
        _ => (None, pieces),
    }
}

/// A write as the database stored it: stamped, when it has a time.
fn as_stored<'a>(piece: &Piece<'a>) -> Cow<'a, [u8]> {
    match piece.at {
        Some(at) => Cow::Owned(stamp(piece.value, at)),
        None => Cow::Borrowed(piece.value),
    }
}

/// What the caller's `operator` makes of `pieces` shown as they are stored,
/// as a database without TTL folds them. `None` when it declines.
fn fold_as_stored(
    operator: &dyn MergeOperator,
    key: &[u8],
    pieces: &[Piece<'_>],
) -> Option<Vec<u8>> {
    let (base, operands) = split_base(pieces);
    let base = base.map(as_stored);
    let operands: Vec<Cow<'_, [u8]>> = operands.iter().map(as_stored).collect();
    let operands: Vec<&[u8]> = operands.iter().map(AsRef::as_ref).collect();
    operator.full_merge(key, base.as_deref(), &operands)
}

/// What a read of `key` returns from the writes `pieces` it holds, oldest
/// first, when a write stamped before `horizon` has expired (none has when
/// it is 0): the value, and the time of the newest write in it. `Ok(None)`
/// when the read returns nothing.
pub(super) fn read(
    operator: Option<&dyn MergeOperator>,
    key: &[u8],
    pieces: &[Piece<'_>],
    horizon: u64,
) -> crate::Result<Option<(Vec<u8>, u64)>> {
    let live: Vec<Piece<'_>> = pieces
        .iter()
        .copied()
        .filter(|piece| piece.at.is_none_or(|at| is_live(at, horizon)))
        .collect();
    let operator = || operator.ok_or(crate::Error::NoMergeOperator);
    let failed = || crate::Error::MergeFailed(key.to_vec());
    let (base, operands) = match live.iter().rposition(untimed_merge) {
        Some(last) => {
            let folded = fold_as_stored(operator()?, key, &live[..=last]).ok_or_else(failed)?;
            (live_value(folded, horizon), &live[last + 1..])
        }
        None => {
            // A put with no time has no stamp, so a read does not see it, as
            // for any value written past the wrapper.
            let (base, operands) = split_base(&live);
            let base = base.and_then(|base| Some((base.value.to_vec(), base.at?)));
            (base, operands)
        }
    };
    if operands.is_empty() {
        return Ok(base);
    }
    // Every merge after the last one with no time has a time.
    let newest = operands
        .iter()
        .filter_map(|piece| piece.at)
        .chain(base.as_ref().map(|(_, at)| *at))
        .max()
        .unwrap_or(0);
    let operands: Vec<&[u8]> = operands.iter().map(|piece| piece.value).collect();
    let folded = operator()?
        .full_merge(key, base.as_ref().map(|(value, _)| &value[..]), &operands)
        .ok_or_else(failed)?;
    Ok(Some((folded, newest)))
}

/// The database's merge on a TTL database: it keeps the writes apart, so a
/// read can fold the ones that have not expired.
pub(super) struct TtlMergeOperator {
    pub(super) callers: Arc<dyn MergeOperator>,
}

impl MergeOperator for TtlMergeOperator {
    fn full_merge(&self, key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut pieces = base.map(|base| pieces_of(base, true)).unwrap_or_default();
        pieces.extend(
            operands
                .iter()
                .flat_map(|operand| pieces_of(operand, false)),
        );
        // The writes with no time at the front of the key, up to the last
        // merge among them: none ever expires, so every read folds them the
        // same way, and the fold is made once, here. Its value has no stamp,
        // so a put of it is one with no time; a fold that came out stamped,
        // or that the operator declines, stays apart for the read to fold.
        let run = pieces.iter().take_while(|piece| piece.at.is_none()).count();
        if let Some(last) = pieces[..run].iter().rposition(untimed_merge)
            && let Some(folded) = fold_as_stored(&*self.callers, key, &pieces[..=last])
            && decode_ttl_suffix(&folded).is_none()
        {
            let put = Piece {
                base: true,
                value: &folded,
                at: None,
            };
            return Some(encode(
                std::iter::once(put).chain(pieces[last + 1..].iter().copied()),
            ));
        }
        Some(encode(pieces))
    }

    fn partial_merge(&self, _key: &[u8], left: &[u8], right: &[u8]) -> Option<Vec<u8>> {
        Some(encode(
            pieces_of(left, false)
                .into_iter()
                .chain(pieces_of(right, false)),
        ))
    }

    fn touches(&self, key: &[u8], operand: &[u8], parts: &[u32]) -> bool {
        pieces_of(operand, false)
            .iter()
            .any(|piece| self.callers.touches(key, piece.value, parts))
    }

    fn name(&self) -> &'static str {
        self.callers.name()
    }
}

#[cfg(test)]
mod tests;
