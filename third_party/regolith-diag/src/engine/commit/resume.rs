//! A `commit_nowait` commit after its call returned: left on its
//! committer's queue behind the unit it waits on, and driven again by that
//! queue's owner when the unit lands (D53).

use std::sync::Arc;

use super::super::RegolithEngine;
use super::super::io::job::Delivery;
use super::super::io::scope;
use super::super::io::shared::QueueShared;
use super::descriptor::{ADMITTED, Descriptor, INSTALLED};
use crate::sync::internal::Arc as RingArc;

/// What a `commit_nowait` commit left on its queue, behind the unit it waits
/// on: when that unit lands, the queue's owner drives the commit again, on
/// its own thread, and tells the commit's ticket there once it is published
/// (D53).
struct Nudge {
    engine: std::sync::Weak<RegolithEngine>,
    desc: RingArc<Descriptor>,
    queue: Arc<QueueShared>,
    told: Arc<dyn Delivery>,
}

impl Delivery for Nudge {
    fn deliver(&self) {
        match self.engine.upgrade() {
            Some(engine) => engine.resume_nowait(&self.desc, &self.queue, &self.told),
            // The database is gone, and the commit with it: the ticket
            // learns what the slot holds.
            None => self.told.deliver(),
        }
    }
}

impl RegolithEngine {
    /// Drive a `commit_nowait` commit and, once it is published, discarded
    /// or refused, tell `told`, on this thread. While a unit is still in its
    /// way, or in the way of its taking a slot (a write stall, a full ring),
    /// the commit is left on `queue` behind that unit instead, and this
    /// returns: the queue's owner comes back here when the unit lands. A
    /// queue that is gone cannot be polled again, so the commit is then
    /// brought through here, as a blocking one is.
    ///
    /// Called on the queue owner's thread: by `commit_nowait` itself, and by
    /// the queue's poll through [`Nudge`]. Exactly one call tells `told`.
    pub(super) fn resume_nowait(
        &self,
        desc: &RingArc<Descriptor>,
        queue: &Arc<QueueShared>,
        told: &Arc<dyn Delivery>,
    ) {
        let Some(pipe) = self.pipeline.as_ref() else {
            told.deliver();
            return;
        };
        // A commit made through a `CacheOnly` handle reads no device on
        // this thread, here as in the call that made it.
        let _scope = desc.read_queue().map(scope::cache_only);
        loop {
            if desc.is_refused() || (desc.is(INSTALLED) && pipe.ring.published() >= desc.index()) {
                told.deliver();
                return;
            }
            let Err(job) = self.advance_nowait(pipe, desc) else {
                continue;
            };
            let nudge: Arc<dyn Delivery> = Arc::new(Nudge {
                engine: self.me.clone(),
                desc: RingArc::clone(desc),
                queue: Arc::clone(queue),
                told: Arc::clone(told),
            });
            if self.io().wait_on(queue, job, Some(nudge)) {
                return;
            }
            // The queue is gone and polls no more: the commit reads the
            // device here, as a blocking one does.
            let _device = scope::blocking();
            let index = if desc.is(INSTALLED) {
                desc.index()
            } else {
                // As a blocking commit: a stop is waited out before the slot
                // is taken.
                if !desc.is(ADMITTED) {
                    match self.admit_commit() {
                        Ok(()) => {
                            desc.mark(ADMITTED);
                        }
                        Err(refused) => {
                            desc.refuse(refused.into_io_error());
                            continue;
                        }
                    }
                }
                self.enter(pipe, desc)
            };
            self.drive(pipe, index, desc);
        }
    }
}
