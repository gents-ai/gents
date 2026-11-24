//! What the parking tests share: a waker that counts its wakes, and a way
//! to have another thread hold something while this one polls.
//!
//! Nothing here is timed. A test polls where a poll must return without
//! what the other thread has, and a poll that waited for it would never
//! return, since it is released only afterwards.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::task::{Wake, Waker};
use std::thread::{Scope, ScopedJoinHandle};
use std::time::Duration;

/// Counts its wakes.
pub(crate) struct Wakes(AtomicUsize);

impl Wakes {
    pub(crate) fn count(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

impl Wake for Wakes {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

pub(crate) fn counting() -> (Arc<Wakes>, Waker) {
    let wakes = Arc::new(Wakes(AtomicUsize::new(0)));
    (Arc::clone(&wakes), Waker::from(Arc::clone(&wakes)))
}

/// Another thread that took something and keeps it until it is told to let
/// go, or until this value is dropped: a failed assertion on the test's
/// thread must not leave it holding.
pub(crate) struct Held<'scope> {
    release: mpsc::Sender<()>,
    holder: ScopedJoinHandle<'scope, ()>,
}

impl Held<'_> {
    /// Have the other thread let go, and wait until it has.
    pub(crate) fn release(self) {
        drop(self.release);
        self.holder.join().unwrap();
    }
}

/// Have a thread of `scope` take what `take` takes, and return once it has.
pub(crate) fn hold<'scope, G>(
    scope: &'scope Scope<'scope, '_>,
    take: impl FnOnce() -> G + Send + 'scope,
) -> Held<'scope> {
    let (taken, is_taken) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let holder = scope.spawn(move || {
        let guard = take();
        taken.send(()).unwrap();
        let _ = released.recv();
        drop(guard);
    });
    is_taken
        .recv_timeout(Duration::from_secs(30))
        .expect("the other thread never took it");
    Held { release, holder }
}

/// Have another thread take what `take` takes and keep it while `held` runs
/// on this thread; then let it go, wait for that thread, and run `released`
/// with what `held` returned.
pub(crate) fn while_another_thread_holds<G, T, R>(
    take: impl FnOnce() -> G + Send,
    held: impl FnOnce() -> T,
    released: impl FnOnce(T) -> R,
) -> R {
    std::thread::scope(|scope| {
        let other = hold(scope, take);
        let out = held();
        other.release();
        released(out)
    })
}
