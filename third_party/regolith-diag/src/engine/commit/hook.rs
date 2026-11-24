//! Test hooks: a point in each stage of the commit pipeline where a test can
//! stop a thread, to show that the pipeline goes on without it. Outside a
//! test build [`at`] is empty.

/// The stage a thread is about to work in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    /// After the reservation, before the install.
    Reserve,
    /// Before a slot's decision is computed.
    Decide,
    /// Before a transaction's slot is checked against the view, above the
    /// horizon of its early check. A slot nothing was decided above skips
    /// it.
    Validate,
    /// Before a decided slot is applied.
    Apply,
    /// Inside the encoding of a slot's record into its descriptor, the
    /// encoding claimed (`FILLING`) and the record not filled yet.
    Fill,
    /// A blocking call far from its turn found the next slot decided by
    /// another thread and is about to park until its turn nears
    /// (`pipeline.rs`, P6).
    TurnWait,
    /// Before a slot's record is written: the write unit claimed and its
    /// run gathered, or a copy about to count in.
    Write,
    /// Inside a record's write: counted in at the log's gate, the record
    /// not written by anyone yet, its bytes not yet with the host.
    InWrite,
    /// A blocking call found the record it needs in another thread's hands
    /// (the write unit claimed, or the record not filled) and is about to
    /// park on it (`write_unit.rs`).
    WriteWait,
    /// Before a sync round is claimed.
    Sync,
    /// A sync about to wait for writes in flight below the prefix it syncs
    /// (`wal_gate.rs`, WG4).
    SyncWait,
    /// Before a slot is published.
    Publish,
    /// A `commit_nowait` commit found writes stopped and is about to take
    /// the stall's unit.
    Stall,
    /// A commit that writes nothing, with something to check, is about to
    /// take the points it is checked at (`write_free.rs`).
    WriteFreePoints,
    /// A commit that writes nothing took the points it is checked at (the
    /// published horizon and the decided frontier, `write_free.rs`) and is
    /// about to load the view it checks in.
    WriteFree,
}

#[cfg(test)]
type Hook = Box<dyn FnMut(Stage)>;

#[cfg(test)]
std::thread_local! {
    static HOOK: std::cell::RefCell<Option<Hook>> = const { std::cell::RefCell::new(None) };
}

/// Run this thread's hook, if a test set one.
#[inline]
pub(crate) fn at(stage: Stage) {
    #[cfg(test)]
    {
        let hook = HOOK.with(|slot| slot.borrow_mut().take());
        if let Some(mut hook) = hook {
            hook(stage);
            HOOK.with(|slot| {
                let mut slot = slot.borrow_mut();
                if slot.is_none() {
                    *slot = Some(hook);
                }
            });
        }
    }
    #[cfg(not(test))]
    let _ = stage;
}

/// Test-only: call `hook` at every stage this thread enters, until
/// [`clear`].
#[cfg(test)]
pub(crate) fn set(hook: impl FnMut(Stage) + 'static) {
    HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}

/// Test-only: remove this thread's hook.
#[cfg(test)]
pub(crate) fn clear() {
    HOOK.with(|slot| *slot.borrow_mut() = None);
}
