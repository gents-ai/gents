//! Every warning a test thread logs, kept in a list of that thread.
//!
//! One subscriber for the whole test binary, not one per test: the tests
//! run on threads of one process, and a subscriber set for one thread only
//! can lose a warning to another thread that reaches the same log line
//! first while that subscriber is being registered (`tracing` then
//! remembers that nobody listens to it). With one subscriber for every
//! thread there is nothing to race.

#![allow(dead_code)]

use std::cell::RefCell;
use std::sync::Once;

use tracing::field::{Field, Visit};
use tracing::span;
use tracing::{Event, Level, Metadata, Subscriber};

thread_local! {
    /// The warnings this thread has logged since [`warnings_of`] last asked.
    static LOGGED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

struct Warnings;

struct Message<'a>(&'a mut String);

impl Visit for Message<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            *self.0 = format!("{value:?}");
        }
    }
}

impl Subscriber for Warnings {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        *metadata.level() <= Level::WARN
    }

    fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }

    fn record(&self, _: &span::Id, _: &span::Record<'_>) {}

    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut message = String::new();
        event.record(&mut Message(&mut message));
        // A thread being torn down has no list left to keep it in.
        let _ = LOGGED.try_with(|logged| logged.borrow_mut().push(message));
    }

    fn enter(&self, _: &span::Id) {}

    fn exit(&self, _: &span::Id) {}
}

/// Run `open` and return what it returned with every warning this thread
/// logged while it ran.
pub fn warnings_of<T>(open: impl FnOnce() -> T) -> (T, Vec<String>) {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        tracing::subscriber::set_global_default(Warnings)
            .expect("no other subscriber is set in this test binary");
    });
    LOGGED.with(|logged| logged.borrow_mut().clear());
    let out = open();
    (out, LOGGED.with(|logged| logged.take()))
}

/// The lines among `warnings` that contain `needle`.
pub fn lines_with<'w>(warnings: &'w [String], needle: &str) -> Vec<&'w String> {
    warnings
        .iter()
        .filter(|line| line.contains(needle))
        .collect()
}
