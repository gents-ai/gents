//! Containing a panic in code the caller supplied while regolith runs it on
//! its own account.
//!
//! A commit is the ordered step: the check a slot of the commit ring runs
//! when it is decided, and what the thread that decides it applies and
//! writes. A panic that unwinds out of a caller's implementation there leaves
//! the shared state of that step half done, so [`contain`] catches it, names
//! the trait, and the step's boundary latches the database read-only.
//!
//! A background step, a flush regolith runs off any caller's call (on a
//! worker, or as the step a write owes on a database with no worker, E9), is
//! contained too, since there is no caller for the panic to unwind into, but
//! it latches nothing: it fails that step alone, which reports the failure and
//! is retried, as any failing flush is.
//!
//! Anywhere else nothing is caught: the panic unwinds into the call that ran
//! the code and fails only that call.
//!
//! Containment is a thread-local mode, because a flush is the same code
//! whoever runs it.

use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::Error;

/// How a panic in a caller's code is treated on this thread.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Not caught: it unwinds into the call that ran the code.
    Uncaught,
    /// Caught inside a commit's ordered step, which latches the database.
    Commit,
    /// Caught inside a background step, which fails alone.
    Background,
}

thread_local! {
    static MODE: Cell<Mode> = const { Cell::new(Mode::Uncaught) };
}

/// The calling thread is inside a commit's ordered step until this drops.
pub(crate) struct InCommit {
    outer: Mode,
}

impl InCommit {
    pub(crate) fn enter() -> Self {
        Self {
            outer: MODE.replace(Mode::Commit),
        }
    }
}

impl Drop for InCommit {
    fn drop(&mut self) {
        MODE.set(self.outer);
    }
}

/// The calling thread runs a background step until this drops.
pub(crate) struct InBackground {
    outer: Mode,
}

impl InBackground {
    pub(crate) fn enter() -> Self {
        Self {
            outer: MODE.replace(Mode::Background),
        }
    }

    /// Contain without latching until this drops, where callbacks are
    /// contained at all: for code told of work that already completed,
    /// which a panic cannot leave half done. Outside a commit or a
    /// background step nothing changes, and a panic still unwinds into the
    /// call that ran the code.
    pub(crate) fn where_contained() -> Option<Self> {
        (MODE.get() != Mode::Uncaught).then(Self::enter)
    }
}

impl Drop for InBackground {
    fn drop(&mut self) {
        MODE.set(self.outer);
    }
}

/// Run `f`, code the caller attached to an outcome already decided: a
/// transaction's `on_commit` and `on_abort` callbacks and hooks, a ticket's
/// `on_complete`. A panic cannot change the outcome, so it is caught, reported
/// to the listeners of `engine` (when it is still open to tell), and the
/// outcome stands. The one function every such point calls.
///
/// `engine` is asked for only once `f` has panicked. A callback may run on a
/// worker after the caller it answers was woken, and a worker holding the
/// engine there would keep the database, and its directory lock, alive past
/// the caller's drop of its handle.
pub(crate) fn survive<E: std::ops::Deref<Target = super::RegolithEngine>>(
    engine: impl FnOnce() -> Option<E>,
    callback: &'static str,
    f: impl FnOnce(),
) {
    if catch_unwind(AssertUnwindSafe(f)).is_err() {
        tracing::error!(
            callback,
            "a callback panicked after its outcome was decided"
        );
        if let Some(engine) = engine() {
            engine.notify_callback_panic(callback);
        }
    }
}

/// Run `f`, a call into the caller's implementation of the trait `callback`.
///
/// Inside a commit or a background step a panic is caught and returned as
/// [`Error::CallbackPanicked`]; the caller of `contain` propagates it to the
/// step's boundary, and the error says whether that boundary latches the
/// database (a commit's does, a background step's does not). Elsewhere `f`
/// simply runs. Catching costs nothing when nothing panics, and neither path
/// allocates.
pub(crate) fn contain<T>(callback: &'static str, f: impl FnOnce() -> T) -> Result<T, Error> {
    let mode = MODE.get();
    if mode == Mode::Uncaught {
        return Ok(f());
    }
    catch_unwind(AssertUnwindSafe(f)).map_err(|_| {
        let latched = mode == Mode::Commit;
        if latched {
            tracing::error!(callback, "a callback panicked while committing");
        } else {
            tracing::error!(callback, "a callback panicked in a background flush");
        }
        // Every caller propagates it to the step's boundary: a commit's
        // latches the database, a background step's fails that step.
        Error::CallbackPanicked { callback, latched }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A callback that returns holds no engine: a worker running it after
    /// its caller was woken must not keep the database alive past the
    /// caller's drop of it.
    #[test]
    fn the_engine_is_asked_for_only_when_a_callback_panics() {
        let asked = std::cell::Cell::new(0);
        let engine = || -> Option<&crate::engine::RegolithEngine> {
            asked.set(asked.get() + 1);
            None
        };
        survive(engine, "on_complete", || {});
        assert_eq!(asked.get(), 0);
        survive(engine, "on_complete", || panic!("boom"));
        assert_eq!(asked.get(), 1);
    }

    #[test]
    fn a_panic_inside_a_commit_is_caught_and_named() {
        let _commit = InCommit::enter();
        let caught = contain("KeyClassifier", || -> u8 { panic!("boom") });
        assert!(matches!(
            caught,
            Err(Error::CallbackPanicked {
                callback: "KeyClassifier",
                latched: true
            })
        ));
        assert_eq!(contain("KeyClassifier", || 7).unwrap(), 7);
    }

    #[test]
    fn a_panic_outside_a_commit_unwinds() {
        let unwound = catch_unwind(|| contain("KeyClassifier", || -> u8 { panic!("boom") }));
        assert!(unwound.is_err());
    }

    #[test]
    fn a_panic_in_a_background_step_is_caught_and_latches_nothing() {
        let _background = InBackground::enter();
        let caught = contain("EventListener", || -> u8 { panic!("boom") });
        assert!(matches!(
            caught,
            Err(Error::CallbackPanicked {
                callback: "EventListener",
                latched: false
            })
        ));
        {
            let _commit = InCommit::enter();
            assert!(matches!(
                contain("EventListener", || -> u8 { panic!("boom") }),
                Err(Error::CallbackPanicked { latched: true, .. })
            ));
        }
        assert!(matches!(
            contain("EventListener", || -> u8 { panic!("boom") }),
            Err(Error::CallbackPanicked { latched: false, .. })
        ));
    }

    #[test]
    fn the_mode_ends_with_the_guard_and_nests() {
        {
            let _outer = InCommit::enter();
            {
                let _inner = InCommit::enter();
            }
            assert!(contain("EventListener", || -> u8 { panic!("boom") }).is_err());
        }
        let unwound = catch_unwind(|| contain("EventListener", || -> u8 { panic!("boom") }));
        assert!(unwound.is_err(), "the mode outlived its guard");
    }
}
