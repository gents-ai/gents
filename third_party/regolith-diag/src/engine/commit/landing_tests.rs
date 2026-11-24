//! Landing: whoever waits for a commit to land (a pessimistic commit's key
//! locks, released then) is told once, by the thread that publishes or
//! discards the commit, when the commit is visible and not before, and not
//! by its owner's next call.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use tempfile::TempDir;

use super::super::io::job::Delivery;
use super::hook::Stage;
use super::pipeline_tests::{
    arm_sync_failure, durable_put, eventual_put, install, open_engine, seq_of, stopped_at,
};
use super::*;

/// Counts how often it was told, and keeps the read horizon it found.
struct Told {
    engine: std::sync::Weak<RegolithEngine>,
    times: AtomicUsize,
    horizon: AtomicU64,
}

impl Told {
    fn on(engine: &Arc<RegolithEngine>) -> Arc<Self> {
        Arc::new(Self {
            engine: Arc::downgrade(engine),
            times: AtomicUsize::new(0),
            horizon: AtomicU64::new(0),
        })
    }

    fn times(&self) -> usize {
        self.times.load(Ordering::Acquire)
    }
}

impl Delivery for Told {
    fn deliver(&self) {
        if let Some(engine) = self.engine.upgrade() {
            self.horizon.store(engine.snapshot_seq(), Ordering::Release);
        }
        self.times.fetch_add(1, Ordering::AcqRel);
    }
}

/// A commit whose writer went no further than the ring is published by the
/// next writer, and that writer tells the listener, with the commit visible.
#[test]
fn a_listener_is_told_once_by_whoever_publishes_the_commit_and_finds_it_visible() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let (_, waited) = install(&engine, eventual_put(b"waited", b"v"));
    let told = Told::on(&engine);
    assert!(waited.listen(Arc::clone(&told) as Arc<dyn Delivery>));
    assert_eq!(told.times(), 0, "nothing has landed yet");

    engine
        .submit(eventual_put(b"later", b"v"))
        .expect("the next writer commits");
    assert_eq!(told.times(), 1, "the publisher tells the listener, once");
    let seq = seq_of(&waited).expect("the waited commit landed");
    assert!(
        told.horizon.load(Ordering::Acquire) >= seq,
        "the listener was told before sequence {seq} was visible"
    );

    let late = Told::on(&engine);
    assert!(
        !waited.listen(Arc::clone(&late) as Arc<dyn Delivery>),
        "a commit that landed takes no more listeners"
    );
    assert_eq!(late.times(), 0);
}

/// A commit a failed sync discards has landed too: its listener is told, by
/// the thread that rolls the log back.
#[test]
fn a_discarded_commit_tells_its_listener() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let (_, waited) = install(&engine, durable_put(b"waited", b"v"));
    let told = Told::on(&engine);
    assert!(waited.listen(Arc::clone(&told) as Arc<dyn Delivery>));

    let _fault = arm_sync_failure(&dir);
    engine
        .submit(durable_put(b"later", b"v"))
        .expect_err("the injected sync failure fails the commits it covers");
    assert!(seq_of(&waited).is_err(), "the waited commit was discarded");
    assert_eq!(told.times(), 1, "the rollback tells the listener, once");
}

/// A thread stopped as it is about to publish a commit keeps nobody waiting
/// for the commit to land: the next thread to reach the slot tells the
/// listener, and the stopped one tells nobody a second time.
#[test]
fn a_publisher_stopped_at_a_commit_leaves_the_telling_to_the_next() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let (_, waited) = install(&engine, eventual_put(b"waited", b"v"));
    let told = Told::on(&engine);
    assert!(waited.listen(Arc::clone(&told) as Arc<dyn Delivery>));

    stopped_at(
        Stage::Publish,
        || {
            engine
                .submit(eventual_put(b"stopped", b"v"))
                .expect("the stopped writer commits once it goes on")
        },
        || {
            assert_eq!(told.times(), 0, "the stopped thread published nothing");
            engine
                .submit(eventual_put(b"next", b"v"))
                .expect("the next writer commits past the stopped one");
            assert_eq!(told.times(), 1, "the next writer told the listener");
        },
    );
    assert_eq!(told.times(), 1, "the listener is told exactly once");
}
