//! Column-family births and deaths, ordered with the writes (plan 4.6).
//!
//! The registry ([`CfRegistry`]) is read without a lock everywhere. What
//! keeps a family's life consistent with the data is *where* it changes and
//! where it is checked: both happen in ring order, in the commit pipeline.
//!
//! - **Every commit is fenced when it is decided, before it takes a
//!   sequence.** Whatever kind of slot it is (a plain write, an optimistic
//!   transaction, a create's or a drop's own commit), deciding it begins
//!   with [`RegolithEngine::cf_fence`]: a commit naming a family that is not
//!   live is refused whole, with nothing logged and no sequence taken. One
//!   place, so no path can miss it. An ingest's install fences the families
//!   its table holds in its own ordered step, before it takes its sequence
//!   (`ingest.rs`).
//! - **A drop is an ordered unit: a commit, then a retire.**
//!   [`RegolithEngine::drop_family`] runs in the ordered step
//!   (`ordered.rs`): with every earlier commit published and no later one
//!   decided, it commits the family's range tombstone and meta delete, and
//!   only once that commit is durable and applied removes the family from
//!   the registry, before any later slot is decided. Every write decided
//!   before it took a lower sequence, so the tombstone deletes it; every
//!   write decided after is fenced against the retired family.
//! - **A create is an ordered unit: a commit, then a publish.**
//!   [`RegolithEngine::create_family`] draws the next id, commits the meta
//!   entry, and publishes the family only once that commit succeeded, so no
//!   handle exists for a family the log does not hold.
//!
//! So a write to a family lands only while the family is live, and a write
//! racing a drop is either deleted by the drop or refused: never committed
//! after the tombstone, where it would be invisible to every handle and never
//! reclaimed. `proofs/tla/CfRegistry.tla` and `proofs/lean/Regolith/CfRegistry.lean`
//! model exactly this order, with the fence moved out of the ordered step, or
//! skipped on one path, as the bugs they rule out.

use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;

use super::super::{DurabilityMode, RegolithEngine, grouped_batch_ops};
use super::WriteRequest;
use crate::WriteBatchOp;
use crate::column_family::{
    CF_PREFIX_LEN, CfRegistry, ColumnFamilyHandle, DEFAULT_CF_ID, META_CF_ID, cf_lower_bound,
    cf_upper_bound, cf_upper_bound_bytes, dropped_family_error, invalid_handle_error, meta,
};

impl RegolithEngine {
    /// Hand the engine the database's column-family registry, so the ordered
    /// step fences writes against it. `Db::open` calls this once, before the
    /// first write; an engine with no registry (the engine's own tests,
    /// whose keys carry no family prefix) fences nothing.
    pub(crate) fn attach_families(&self, families: Arc<CfRegistry>) {
        // A second attach would be a second `Db` over one engine, which
        // `Db::open` never builds; the first registry stays.
        let _ = self.families.set(families);
    }

    fn families(&self) -> io::Result<&CfRegistry> {
        self.families.get().map(Arc::as_ref).ok_or_else(|| {
            io::Error::other("column families need the database's registry, which is not attached")
        })
    }

    /// Refuse `request` when a key in it names a column family that is not
    /// live. A transaction is fenced by the writes it carries; the appends it
    /// numbers later write the default family, which is never dropped.
    pub(super) fn cf_fence(&self, request: &WriteRequest) -> io::Result<()> {
        match request {
            WriteRequest::Put { key, .. } => self.cf_fence_keys(std::iter::once(key.as_slice())),
            WriteRequest::Batch { ops, .. } => self.cf_fence_ops(ops),
            WriteRequest::Txn(txn) => self.cf_fence_ops(&txn.ops),
        }
    }

    /// [`Self::cf_fence`] over a batch's operations.
    pub(super) fn cf_fence_ops(&self, ops: &[WriteBatchOp]) -> io::Result<()> {
        let Some(families) = self.families.get() else {
            return Ok(());
        };
        let mut last = None;
        for op in ops {
            match op {
                WriteBatchOp::Put { key, .. }
                | WriteBatchOp::Delete { key }
                | WriteBatchOp::Merge { key, .. } => fence_key(families, &mut last, key)?,
                WriteBatchOp::DeleteRange { start, end } => {
                    fence_key(families, &mut last, start)?;
                    // A range that ends at the next family's first key (a
                    // whole-family delete) writes nothing in that family.
                    let whole_family = family_of(start)
                        .is_some_and(|id| end.as_slice() == cf_upper_bound_bytes(id));
                    if !whole_family {
                        fence_key(families, &mut last, end)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// [`Self::cf_fence`] over a run of keys.
    pub(super) fn cf_fence_keys<'a>(
        &self,
        keys: impl IntoIterator<Item = &'a [u8]>,
    ) -> io::Result<()> {
        let Some(families) = self.families.get() else {
            return Ok(());
        };
        let mut last = None;
        keys.into_iter()
            .try_for_each(|key| fence_key(families, &mut last, key))
    }

    /// [`Self::cf_fence`] over the families an ingested table holds keys of.
    pub(crate) fn cf_fence_ids(&self, ids: &[u32]) -> io::Result<()> {
        let Some(families) = self.families.get() else {
            return Ok(());
        };
        let mut last = None;
        ids.iter()
            .try_for_each(|id| fence_id(families, &mut last, *id))
    }

    /// Create the column family `name`, or return the live one of that name.
    ///
    /// In the ordered step: draws the next id, commits its meta entry and
    /// the advanced id counter, and publishes the family only once that
    /// commit is durable and applied. Concurrent creates of one name take
    /// their turns through the ring, so the second finds the first's family.
    pub(crate) fn create_family(
        &self,
        name: &str,
        durability: DurabilityMode,
    ) -> io::Result<ColumnFamilyHandle> {
        self.families()?;
        self.ensure_writable()?;
        let name = name.to_string();
        let created = self.ordered(move |engine, ord| {
            let families = engine.families()?;
            if let Some(existing) = families.get(&name) {
                return Ok(existing);
            }
            let Some(id) = families.next_id() else {
                return Err(crate::Error::invalid_argument(
                    "the column-family id space is exhausted",
                )
                .into_io_error());
            };
            let mut point_ops = BTreeMap::new();
            point_ops.insert(meta::name_key(&name), Some(id.to_be_bytes().to_vec()));
            point_ops.insert(meta::next_id_key(), Some((id + 1).to_be_bytes().to_vec()));
            let ops = grouped_batch_ops(point_ops, Vec::new(), Vec::new());
            ord.commit_alone(&ops, durability)?;
            // Published inside the ordered step, before any later commit is
            // decided.
            Ok(families.publish(id, &name))
        });
        // Off the pipeline, as every commit runs the step it left owing.
        self.run_owed_step();
        created
    }

    /// Drop the column family `cf`.
    ///
    /// In the ordered step: commits a range tombstone over the family's keys
    /// and the delete of its meta entry, then retires the family from the
    /// registry before any later commit is decided. A handle that is not
    /// live when the drop's turn comes (another drop won) is refused there.
    pub(crate) fn drop_family(
        &self,
        cf: &ColumnFamilyHandle,
        durability: DurabilityMode,
    ) -> io::Result<()> {
        self.families()?;
        self.ensure_writable()?;
        let cf = cf.clone();
        let dropped = self.ordered(move |engine, ord| {
            let families = engine.families()?;
            if !families.is_live_handle(&cf) {
                return Err(invalid_handle_error(&cf).into_io_error());
            }
            let mut point_ops = BTreeMap::new();
            point_ops.insert(meta::name_key(cf.name()), None);
            let range = (cf_lower_bound(cf.id()), cf_upper_bound(cf.id()));
            let ops = grouped_batch_ops(point_ops, vec![range], Vec::new());
            ord.commit_alone(&ops, durability)?;
            // The tombstone is durable and applied: from this retire on, the
            // fence refuses every write to the family.
            families.retire(cf.id());
            Ok(())
        });
        self.run_owed_step();
        dropped
    }
}

#[cfg(test)]
#[path = "families_tests.rs"]
mod tests;

/// The family a prefixed key belongs to, or `None` for a key too short to
/// carry one.
fn family_of(key: &[u8]) -> Option<u32> {
    key.first_chunk::<CF_PREFIX_LEN>()
        .map(|prefix| u32::from_be_bytes(*prefix))
}

fn fence_key(families: &CfRegistry, last: &mut Option<u32>, key: &[u8]) -> io::Result<()> {
    match family_of(key) {
        Some(id) => fence_id(families, last, id),
        // Every key a `Db` writes carries its family; a shorter one cannot
        // name a family to land in.
        None => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a write key is shorter than its column-family prefix",
        )),
    }
}

/// Refuse `id` unless it is live. `last` remembers the id just found live,
/// so a run of keys in one family costs one lookup.
fn fence_id(families: &CfRegistry, last: &mut Option<u32>, id: u32) -> io::Result<()> {
    // The default family is never dropped, and the meta family is written
    // only by create and drop themselves (a caller's write to it is refused
    // at the API boundary).
    if *last == Some(id) || id == DEFAULT_CF_ID || id == META_CF_ID {
        return Ok(());
    }
    if !families.contains_id(id) {
        return Err(dropped_family_error(id).into_io_error());
    }
    *last = Some(id);
    Ok(())
}
