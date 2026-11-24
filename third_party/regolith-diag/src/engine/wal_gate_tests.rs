use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering as StdOrdering};
use std::task::Wake;

struct Count(AtomicUsize);

impl Wake for Count {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, StdOrdering::SeqCst);
    }
}

fn counting() -> (Arc<Count>, Waker) {
    let count = Arc::new(Count(AtomicUsize::new(0)));
    (Arc::clone(&count), Waker::from(count))
}

fn woken(count: &Count) -> usize {
    count.0.load(StdOrdering::SeqCst)
}

#[test]
fn a_write_of_the_current_epoch_is_let_in_and_one_of_an_older_epoch_is_not() {
    let gate = Gate::new();
    assert_eq!(gate.epoch(), 0);
    let entry = gate.enter(0).expect("a write of the current epoch");
    assert!(!gate.is_clear());
    gate.leave(entry);
    assert!(gate.is_clear());
    gate.turn(1);
    assert_eq!(gate.epoch(), 1);
    assert!(
        gate.enter(0).is_none(),
        "a write decided before the gate moved"
    );
    assert!(gate.is_clear(), "a write turned away is never counted");
    gate.leave(gate.enter(1).expect("a write of the new epoch"));
}

#[test]
fn the_epoch_moves_with_writes_in_flight_and_keeps_them_counted() {
    let gate = Gate::new();
    let first = gate.enter(0).unwrap();
    let second = gate.enter(0).unwrap();
    gate.turn(7);
    assert_eq!(gate.epoch(), 7);
    assert!(!gate.is_clear());
    gate.leave(first);
    assert!(!gate.is_clear());
    gate.leave(second);
    assert!(gate.is_clear());
}

#[test]
fn a_parked_rollback_is_woken_by_the_last_write_to_leave() {
    let gate = Gate::new();
    let first = gate.enter(0).unwrap();
    let second = gate.enter(0).unwrap();
    gate.turn(1);
    let (count, waker) = counting();
    assert!(!gate.clear_or_park(&waker), "two writes are in flight");
    gate.leave(first);
    assert_eq!(woken(&count), 0, "one write is left");
    gate.leave(second);
    assert_eq!(woken(&count), 1);
    assert!(gate.clear_or_park(&waker));
    assert_eq!(woken(&count), 1, "nothing to wait for");
}

#[test]
fn a_write_that_leaves_under_its_own_epoch_wakes_nobody() {
    let gate = Gate::new();
    let (count, waker) = counting();
    let entry = gate.enter(0).unwrap();
    // Nothing moved the gate and no sync turned it: nobody that would park
    // has begun, so no waker is taken.
    gate.cleared.register(&waker);
    gate.leave(entry);
    assert_eq!(woken(&count), 0);
}

#[test]
fn a_sync_with_no_write_in_flight_covers_its_prefix_at_once() {
    let gate = Gate::new();
    let (count, waker) = counting();
    assert_eq!(gate.settle(100, &waker), Poll::Ready(100));
    gate.leave(gate.enter(0).unwrap());
    assert_eq!(gate.settle(200, &waker), Poll::Ready(200));
    assert_eq!(woken(&count), 0);
}

#[test]
fn a_sync_waits_for_the_writes_in_flight_when_it_began_and_not_for_later_ones() {
    let gate = Gate::new();
    let (count, waker) = counting();
    let before = gate.enter(0).unwrap();
    assert_eq!(
        gate.settle(100, &waker),
        Poll::Pending,
        "a write is in flight"
    );
    // Let in after the turn: on the other side, so the sync does not wait
    // for it, and it leaves whenever it likes.
    let after = gate.enter(0).unwrap();
    assert_eq!(gate.settle(100, &waker), Poll::Pending);
    assert_eq!(woken(&count), 0);
    gate.leave(before);
    assert_eq!(
        woken(&count),
        1,
        "the last write the turn waits for wakes the sync"
    );
    assert_eq!(
        gate.settle(100, &waker),
        Poll::Ready(100),
        "the write let in after the turn is still in flight"
    );
    gate.leave(after);
    assert_eq!(
        woken(&count),
        1,
        "nobody waits for the write let in after the turn"
    );
}

#[test]
fn a_turn_a_parked_sync_made_is_finished_for_its_own_prefix() {
    let gate = Gate::new();
    let (_, waker) = counting();
    let slow = gate.enter(0).unwrap();
    assert_eq!(gate.settle(100, &waker), Poll::Pending);
    // Records up to 300 got written meanwhile, by writes let in after the
    // turn: one of them still in flight.
    let late = gate.enter(0).unwrap();
    gate.leave(slow);
    assert_eq!(
        gate.settle(300, &waker),
        Poll::Ready(100),
        "the turn was made for 100, and a write let in after it may still land below 300"
    );
    // The next sync turns again, and waits for the late write.
    assert_eq!(gate.settle(300, &waker), Poll::Pending);
    gate.leave(late);
    assert_eq!(gate.settle(300, &waker), Poll::Ready(300));
}

#[test]
fn a_rollback_forgets_the_turn_of_a_prefix_it_cut() {
    let gate = Gate::new();
    let (_, waker) = counting();
    let slow = gate.enter(0).unwrap();
    assert_eq!(gate.settle(100, &waker), Poll::Pending);
    gate.turn(1);
    gate.leave(slow);
    assert!(gate.is_clear());
    gate.forget_turn();
    assert_eq!(
        gate.settle(80, &waker),
        Poll::Ready(80),
        "the forgotten turn no longer bounds the sync"
    );
}
