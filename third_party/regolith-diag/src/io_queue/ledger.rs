//! What landed for one queue, as its owner keeps it, and when it lets each
//! landing go.
//!
//! A read run again after a wait finds its block in the block cache or in
//! what landed for its queue (`QueueShared::landed`). That table is bounded
//! by the queue's bytes: past the bound the oldest landings go. A call that
//! needs several blocks at once (a `multi_get`, a collecting scan, a read
//! merging operands from several tables) starts over each time it is run,
//! so evicting the blocks it found on its earlier runs could keep it from
//! ever finishing: two blocks that each take most of the bound would evict
//! each other on every run.
//!
//! So a landing a call still needs is held, and only the others count
//! against the bound:
//!
//! - a wait carries the landed blocks its call read before it
//!   ([`WaitSlot::holds`]): they are held while the wait is pending;
//! - once the wait completes it is *kept*: its holds, and the block it waited
//!   for, stay held until the call is seen again: a later wait whose holds
//!   cover them (the call was run again and waits for one more block), or a
//!   call that read all of them and finished ([`Message::Finished`]);
//! - abandoned waits expire after [`KEEP_ROUNDS`] landing polls; a live
//!   caller retains progress for at most [`LIVE_KEEP_ROUNDS`] landing polls.
//!   A forgotten handle cannot pin landings forever.
//!
//! The landings no call holds go oldest first, at the end of a poll, until
//! total bytes fit the bound or only held landings remain. Total bytes
//! can exceed the bound by the
//! union of blocks held by pending and retained calls; this is not a hard
//! total-memory bound. Completed live waits have a finite age bound, not a
//! separate byte cap. A caller resuming within that window finds its progress;
//! after expiry it may need to reread. IoProgress reports completed live
//! retention bytes, with a warning once per episode above half the bound.
//! `proofs/tla/LandingHold.tla` checks that such a call finishes, and that
//! the eviction this replaces (oldest first, holding nothing) never lets it.
//!
//! [`WaitSlot::holds`]: crate::engine::io::shared::WaitSlot::holds
//! [`Message::Finished`]: crate::engine::io::shared::Message::Finished

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use crate::engine::io::shared::{Landing, QueueShared, WaitSlot};
use crate::engine::io::unit::UnitKey;

/// Landing polls an abandoned completed wait is kept before reclaiming it.
pub(super) const KEEP_ROUNDS: u64 = 4;

/// A live handle may delay reclamation for sixteen abandoned-wait windows.
pub(super) const LIVE_KEEP_ROUNDS: u64 = 16 * KEEP_ROUNDS;

/// The owner's account of what landed for its queue. Only the owner touches
/// it, at its polls.
#[derive(Default)]
pub(super) struct Ledger {
    /// Each landing, oldest first, with what it costs. A block landed twice
    /// is here twice; the table keeps the newer. This is the one strong
    /// handle the queue keeps on each block: the table's is weak, so a
    /// landing dropped here frees its block now, not when kovan reclaims
    /// the table's removed entry.
    order: VecDeque<(UnitKey, usize, Landing)>,
    /// Occurrences in order; only the last eviction forgets a landing.
    occurrences: HashMap<UnitKey, usize>,
    /// Scratch keys reused across polls that observe live completed waits.
    live_keys: HashSet<UnitKey>,
    /// What `order` costs.
    bytes: usize,
    /// How many pending or kept waits hold each block. A block held by none
    /// has no entry.
    held: HashMap<UnitKey, u32>,
    /// Completed waits whose call has not been seen again, oldest first.
    kept: VecDeque<Kept>,
    /// Polls that landed something: what a kept wait is aged by.
    round: u64,
    /// Something landed since the last [`Ledger::settle`].
    landed: bool,
    /// Union of landed bytes protected by completed waits with live callers.
    live_bytes: usize,
    /// A warning has already been emitted for the current retention episode.
    retention_warning: bool,
}

/// A completed wait, kept for its call's next run.
struct Kept {
    /// The block it waited for; `None` for a wait for room.
    awaited: Option<UnitKey>,
    /// The wait itself, which carries what its call read before it.
    slot: Arc<WaitSlot>,
    /// The landing poll it completed in.
    round: u64,
}

impl Ledger {
    /// `key` landed as `landing`, costing `charge` bytes.
    pub(super) fn land(&mut self, key: UnitKey, charge: usize, landing: Landing) {
        self.order.push_back((key, charge, landing));
        *self.occurrences.entry(key).or_default() += 1;
        self.bytes = self.bytes.saturating_add(charge);
        self.landed = true;
    }

    /// A wait recorded on the queue: hold what its call read.
    pub(super) fn hold(&mut self, slot: &WaitSlot) {
        for key in slot.holds() {
            self.pin(*key);
        }
    }

    /// A wait held by [`Ledger::hold`] completed, for the block `awaited`
    /// (`None` for room): keep it, and what it holds, for its call's next
    /// run. A wait that holds nothing and waited for no block keeps nothing.
    pub(super) fn completed(&mut self, awaited: Option<UnitKey>, slot: Arc<WaitSlot>) {
        if awaited.is_none() && slot.holds().is_empty() {
            return;
        }
        if let Some(key) = awaited {
            self.pin(key);
        }
        self.kept.push_back(Kept {
            awaited,
            slot,
            round: self.round,
        });
    }

    /// A call read `read` (sorted). Release covered waits only after their
    /// callers relinquish their handles: another call can read the same
    /// blocks without consuming the delayed caller's progress.
    pub(super) fn seen(&mut self, read: &[UnitKey]) {
        if read.is_empty() || self.kept.is_empty() {
            return;
        }
        let covered = |kept: &Kept| {
            // A completed slot is never reissued. Existing callers may clone,
            // but after only Ledger owns it the count cannot increase again.
            // The relaxed count is only an ownership test: release reads no
            // caller-written fields and needs no final-drop acquire here.
            Arc::strong_count(&kept.slot) == 1
                && kept
                    .awaited
                    .is_none_or(|key| read.binary_search(&key).is_ok())
                && kept
                    .slot
                    .holds()
                    .iter()
                    .all(|key| read.binary_search(key).is_ok())
        };
        let mut i = 0;
        while i < self.kept.len() {
            if covered(&self.kept[i]) {
                if let Some(kept) = self.kept.remove(i) {
                    self.release(&kept);
                }
            } else {
                i += 1;
            }
        }
    }

    /// The end of a poll: when something landed, a round passed, and a kept
    /// wait expires after its abandoned or live window. Unheld landings
    /// then go oldest first while total bytes exceed the queue bound.
    pub(super) fn settle(&mut self, queue: &QueueShared) {
        if !std::mem::take(&mut self.landed) {
            return;
        }
        self.round += 1;
        let mut kept = std::mem::take(&mut self.kept);
        let mut live_keys = std::mem::take(&mut self.live_keys);
        live_keys.clear();
        kept.retain(|wait| {
            let age = self.round - wait.round;
            // A completed slot is never reissued: existing callers can clone,
            // but a count of one cannot later increase. The relaxed load only
            // tests ownership; release reads no caller-written slot fields.
            let live = Arc::strong_count(&wait.slot) > 1;
            if age <= LIVE_KEEP_ROUNDS && (age <= KEEP_ROUNDS || live) {
                if live {
                    live_keys.extend(wait.slot.holds().iter().copied());
                    live_keys.extend(wait.awaited);
                }
                true
            } else {
                self.release(wait);
                false
            }
        });
        self.kept = kept;
        self.live_bytes = 0;
        for (key, charge, _) in self.order.iter().rev() {
            if live_keys.is_empty() {
                break;
            }
            if live_keys.remove(key) {
                self.live_bytes += charge;
            }
        }
        self.live_keys = live_keys;
        let over_half = self.live_bytes > queue.bound() / 2;
        if over_half && !self.retention_warning {
            tracing::warn!(
                retained_live_bytes = self.live_bytes,
                queue_bound_bytes = queue.bound(),
                "live completed reads retain more than half the I/O queue byte bound"
            );
        }
        self.retention_warning = over_half;
        if self.bytes <= queue.bound() {
            return;
        }
        let mut i = 0;
        while self.bytes > queue.bound() && i < self.order.len() {
            let (key, charge, _) = self.order[i];
            if self.held.contains_key(&key) {
                i += 1;
                continue;
            }
            self.order.remove(i);
            self.bytes -= charge;
            let remaining = self.occurrences.get_mut(&key).expect("counted landing");
            *remaining -= 1;
            if *remaining == 0 {
                self.occurrences.remove(&key);
                queue.forget(&key);
            }
        }
    }

    /// Let go of every landing: the queue is being dropped.
    pub(super) fn clear(&mut self, queue: &QueueShared) {
        for (key, _, _) in self.order.drain(..) {
            queue.forget(&key);
        }
        self.occurrences.clear();
        self.live_keys.clear();
        self.bytes = 0;
        self.live_bytes = 0;
        self.retention_warning = false;
        self.held.clear();
        self.kept.clear();
    }

    fn pin(&mut self, key: UnitKey) {
        *self.held.entry(key).or_insert(0) += 1;
    }

    fn unpin(&mut self, key: UnitKey) {
        if let Some(count) = self.held.get_mut(&key) {
            *count -= 1;
            if *count == 0 {
                self.held.remove(&key);
            }
        }
    }

    fn release(&mut self, kept: &Kept) {
        if let Some(key) = kept.awaited {
            self.unpin(key);
        }
        for key in kept.slot.holds() {
            self.unpin(*key);
        }
    }

    /// Bytes retained by completed waits with live callers at the last settle.
    pub(super) fn live_bytes(&self) -> usize {
        self.live_bytes
    }

    /// How many completed waits are kept.
    pub(super) fn kept(&self) -> usize {
        self.kept.len()
    }

    /// Bytes of what landed and is still kept.
    pub(super) fn bytes(&self) -> usize {
        self.bytes
    }

    /// Whether `key` is held by a pending or kept wait.
    #[cfg(test)]
    pub(super) fn is_held(&self, key: &UnitKey) -> bool {
        self.held.contains_key(key)
    }

    /// The blocks still landed, oldest first.
    #[cfg(test)]
    pub(super) fn keys(&self) -> Vec<UnitKey> {
        self.order.iter().map(|(key, _, _)| *key).collect()
    }
}

#[cfg(all(test, not(loom)))]
#[path = "ledger_tests.rs"]
mod tests;
