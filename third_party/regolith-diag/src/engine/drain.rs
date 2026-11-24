//! A drain: memtables seen through to tables for a caller that needs them
//! there, as work that can stop where it would wait (plan 4.10).
//!
//! `compact_range` flushes the active memtable before it reads L0, a
//! checkpoint and a close flush every memtable, and so does an explicit
//! flush. Each is the same two steps. An ordered unit of the commit pipeline
//! seals what has to be sealed and names the memtables to flush, the
//! *targets*. Then the oldest frozen memtable is flushed, again and again,
//! until every target has left the frozen list.
//!
//! A call that blocks runs the two in one go. A job a queue's poll runs may
//! not wait for another thread (`commit::park`), and both steps would: the
//! unit for a round or a unit ahead of it, each flush for the flush
//! exclusion and for the version store's step. So a drain is a value that
//! remembers where it is ([`Drain`]), and [`RegolithEngine::drain_advance`]
//! takes it as far as it goes: to its end for a caller that waits, or to
//! the first thing another thread holds for a poll, which parks there and
//! goes on from the same place when it is woken.
//!
//! # Invariants
//!
//! - **DR1, the targets are fixed by the unit and never extended.** A drain
//!   that looked at the view again each pass would chase the memtables
//!   writers seal after it began, and under a steady write load never
//!   finish: a checkpoint taken while anything writes would hang.
//! - **DR2, a parked drain holds nothing.** Between two calls it holds no
//!   exclusion and no step. Its unit, once in the ring, is run by whichever
//!   thread reaches its slot.
//! - **DR3, flushes go oldest first, whoever asked** (`LsmOrder.tla`): each
//!   pass flushes the oldest frozen memtable, so a target is reached only
//!   after every memtable sealed before it. L0 files carry no sequence of
//!   their own and readers take the most recently added as the newest, so
//!   flushing out of order would publish an older file over a newer one.

use std::io;
use std::sync::Arc;

use super::commit::{Help, Needs, OrderedTicket};
use super::memtable::MemTable;
use super::{RegolithEngine, memtable_needs_flush};

/// How a drain seals the active memtable, when it holds anything.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActiveFlush {
    /// In place, with no fresh log and without the writable check: what a
    /// close needs. The log stays the newest until the flush makes it
    /// redundant, so it is never synced (`Ordered::seal_in_place`).
    ForClose,
    /// Behind a fresh log: what a checkpoint and an explicit flush need. A
    /// capture names tables and copies no log, so a write still in a
    /// memtable would be absent from it.
    Always,
}

/// A drain under way. See the module docs.
pub(crate) struct Drain {
    stage: Stage,
}

enum Stage {
    /// The unit that seals and names the targets is not seen through yet.
    Sealing(OrderedTicket<Vec<Arc<MemTable>>>),
    /// Flushing until `targets[next..]` have left the frozen list (DR1).
    Flushing {
        targets: Vec<Arc<MemTable>>,
        next: usize,
    },
}

impl Drain {
    /// Whether the unit named a memtable to flush. `false` while the unit
    /// is not seen through, and when it found nothing to drain.
    pub(crate) fn has_targets(&self) -> bool {
        matches!(&self.stage, Stage::Flushing { targets, .. } if !targets.is_empty())
    }
}

impl RegolithEngine {
    /// A drain of every frozen memtable, and of the active one as `active`
    /// says. Nothing is in the ring until it is advanced.
    pub(crate) fn drain_all(&self, active: ActiveFlush) -> Drain {
        let ticket = self.ordered_unit(Needs::VERSIONS, move |engine, ord| {
            let view = engine.view.load();
            let seal_active = memtable_needs_flush(&view.active);
            let mut targets = view.frozen.clone();
            drop(view);
            if seal_active {
                targets.push(match active {
                    // A drain for close runs with the database already
                    // closing, and seals the last memtable without the
                    // writable check.
                    ActiveFlush::ForClose => ord.seal_in_place()?,
                    ActiveFlush::Always => ord.seal(false)?,
                });
            }
            Ok(targets)
        });
        Drain {
            stage: Stage::Sealing(ticket),
        }
    }

    /// A drain of the active memtable alone, and of the frozen ones before
    /// it, if `needed` says so when the unit's turn comes: what
    /// `compact_range` needs before it reads L0. Asked in the ordered step,
    /// so a memtable another call sealed meanwhile is left alone.
    pub(crate) fn drain_active_if(
        &self,
        needed: impl Fn(&RegolithEngine, &MemTable) -> bool + Send + 'static,
    ) -> Drain {
        let ticket = self.ordered_unit(Needs::VERSIONS, move |engine, ord| {
            Ok(if needed(engine, ord.active()) {
                vec![ord.seal(true)?]
            } else {
                Vec::new()
            })
        });
        Drain {
            stage: Stage::Sealing(ticket),
        }
    }

    /// Take `drain` as far as it goes. `Ok(true)` when every target has
    /// left the frozen list. `Ok(false)` when a queue's poll parked: its
    /// waker is registered where the wait ends, the drain holds nothing
    /// (DR2), and the next call goes on from where this one stopped. A call
    /// that waits (`Help::Wait`) never gets `Ok(false)`.
    pub(crate) fn drain_advance(&self, drain: &mut Drain, help: Help<'_>) -> io::Result<bool> {
        loop {
            match &mut drain.stage {
                Stage::Sealing(ticket) => {
                    let Some(targets) = self.ordered_advance(ticket, help) else {
                        return Ok(false);
                    };
                    drain.stage = Stage::Flushing {
                        targets: targets?,
                        next: 0,
                    };
                }
                Stage::Flushing { targets, next } => {
                    // Oldest first, the order `flush_until_retired_with`
                    // writes them in, so a target an earlier pass already
                    // flushed costs one list scan and no work.
                    while let Some(target) = targets.get(*next) {
                        if !self.flush_until_retired_with(target, help)? {
                            return Ok(false);
                        }
                        *next += 1;
                    }
                    return Ok(true);
                }
            }
        }
    }

    /// Flush frozen memtables until none of those there now remain, and the
    /// active one according to `active`, waiting on this thread.
    pub(super) fn drain_memtables(&self, active: ActiveFlush) -> io::Result<()> {
        let mut drain = self.drain_all(active);
        self.drain_advance(&mut drain, Help::Wait).map(|_| ())
    }

    /// Flush frozen memtables, oldest first, until `target` is no longer in
    /// the frozen list. `Ok(false)` when a queue's poll parked before a
    /// pass, holding nothing.
    ///
    /// The caller names the memtable it needs durable, not a count of
    /// passes, so a concurrent rotation cannot extend the work and a
    /// concurrent flush that already retired the target ends it early
    /// (DR3 says why each pass still takes the oldest).
    ///
    /// Each pass takes the flush exclusion and releases it before the
    /// next. A poll takes it, and the version store's step the flush hands
    /// sections to, before the pass begins, or parks; a call that waits
    /// takes the exclusion on this thread and lets the flush wait for the
    /// step. Inside an ordered unit a poll runs, both are held already.
    pub(crate) fn flush_until_retired_with(
        &self,
        target: &Arc<MemTable>,
        help: Help<'_>,
    ) -> io::Result<bool> {
        while self
            .view
            .load()
            .frozen
            .iter()
            .any(|mt| Arc::ptr_eq(mt, target))
        {
            let Some(kept) = self.keep(Needs::FLUSH, help) else {
                return Ok(false);
            };
            let flushed = self
                .flusher
                .flushing()
                .and_then(|flushing| self.flush_oldest_frozen(&flushing));
            self.unkeep(kept);
            if !flushed? {
                break;
            }
        }
        Ok(true)
    }

    /// [`Self::flush_until_retired_with`] for a caller that waits, or that
    /// runs in an ordered unit, which holds what a flush needs.
    pub(super) fn flush_until_retired(&self, target: &Arc<MemTable>) -> io::Result<()> {
        self.flush_until_retired_with(target, Help::Wait)
            .map(|_| ())
    }
}
