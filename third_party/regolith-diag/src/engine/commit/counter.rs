//! Reading a position or a counter in the ordered step.
//!
//! A log head, a once key and an allocation counter all hold a `u64` as eight
//! big-endian bytes, and the ordered step reads each from the view it is
//! committing against. There is no cache in front of the read: a group that
//! fails leaves nothing behind that a later group could trust.

use std::io;

use super::super::lookup_key::LookupKey;
use super::super::sstable::{Materialize, PointValue};
use super::super::{ReadView, RegolithEngine};

impl RegolithEngine {
    /// The `u64` that `key` (column-family prefixed) holds in `view`, with
    /// nothing hidden by a sequence bound: the newest committed value.
    /// `None` when the key is absent.
    ///
    /// The caller is deciding a slot, in ring order, so the view is
    /// complete: every earlier commit is applied to it. A value of any length but eight is
    /// refused with `InvalidInput`, which surfaces as
    /// [`crate::Error::InvalidArgument`], and names no key bytes.
    pub(super) fn read_u64_in_view(&self, key: &[u8], view: &ReadView) -> io::Result<Option<u64>> {
        let snapshot_seq = u64::MAX;
        let lk = LookupKey::from_prefixed(key, snapshot_seq);
        match self.lookup_in_view(key, snapshot_seq, &lk, Materialize::Value, view)? {
            None => Ok(None),
            Some(PointValue::Value(value)) => {
                let bytes = <[u8; 8]>::try_from(value.as_slice()).map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "a log head, once key or allocation counter holds {} bytes where \
                             an 8-byte big-endian number is required; something else wrote it",
                            value.len()
                        ),
                    )
                })?;
                Ok(Some(u64::from_be_bytes(bytes)))
            }
            Some(PointValue::Length(_)) => Err(io::Error::other(
                "point lookup produced a length where a value was requested",
            )),
        }
    }
}
