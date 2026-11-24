//! A merge operator over values made of three counters, one per part, for the
//! tests of projected reads.
//!
//! A value is three big-endian `u64`s (24 bytes); an absent key reads as
//! zeros. An operand names one part and adds to it. `touches` says an operand
//! touches exactly the part it names, and `partial_merge` folds two operands
//! on one part, so the operator meets the contract `touches` documents.

#![allow(dead_code)]

use regolith::MergeOperator;

/// Parts in a value.
pub const PARTS: usize = 3;

pub struct Parted;

/// The 24-byte value holding `parts`.
pub fn value(parts: [u64; PARTS]) -> Vec<u8> {
    parts.iter().flat_map(|part| part.to_be_bytes()).collect()
}

/// The parts of a value, or `None` when it is not one.
pub fn parts_of(bytes: &[u8]) -> Option<[u64; PARTS]> {
    if bytes.len() != PARTS * 8 {
        return None;
    }
    let (chunks, _) = bytes.as_chunks::<8>();
    let mut parts = [0u64; PARTS];
    for (part, chunk) in parts.iter_mut().zip(chunks) {
        *part = u64::from_be_bytes(*chunk);
    }
    Some(parts)
}

/// An operand adding `delta` to `part`.
pub fn add(part: u32, delta: u64) -> Vec<u8> {
    let mut operand = part.to_be_bytes().to_vec();
    operand.extend_from_slice(&delta.to_be_bytes());
    operand
}

fn operand_of(bytes: &[u8]) -> Option<(u32, u64)> {
    if bytes.len() != 12 {
        return None;
    }
    Some((
        u32::from_be_bytes(bytes[..4].try_into().ok()?),
        u64::from_be_bytes(bytes[4..].try_into().ok()?),
    ))
}

impl MergeOperator for Parted {
    fn name(&self) -> &'static str {
        "parted"
    }

    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut parts = match base {
            Some(bytes) => parts_of(bytes)?,
            None => [0; PARTS],
        };
        for operand in operands {
            let (part, delta) = operand_of(operand)?;
            let slot = parts.get_mut(usize::try_from(part).ok()?)?;
            *slot = slot.wrapping_add(delta);
        }
        Some(value(parts))
    }

    fn partial_merge(&self, _key: &[u8], left: &[u8], right: &[u8]) -> Option<Vec<u8>> {
        let ((lp, ld), (rp, rd)) = (operand_of(left)?, operand_of(right)?);
        (lp == rp).then(|| add(lp, ld.wrapping_add(rd)))
    }

    fn touches(&self, _key: &[u8], operand: &[u8], parts: &[u32]) -> bool {
        operand_of(operand).is_none_or(|(part, _)| parts.contains(&part))
    }
}
