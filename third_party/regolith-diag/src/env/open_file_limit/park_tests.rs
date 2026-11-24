//! A reopen that may not wait (D60): it parks when every slot is in use, the
//! step that frees a slot it marked wakes it, and the mark survives every
//! step that keeps the slot busy (a load, a drain).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as StdOrdering};
use std::time::{Duration, Instant};

use super::slots::{Parking, SlotTable, SlotWaiter};
use super::tests::Counted;
use super::*;
use crate::engine::io::scope;
use crate::engine::io::shared::{Message, QueueShared, test_id};
use crate::env::MemEnv;

/// A descriptor whose every byte is its table's number.
struct Table(u8);

impl ReadFile for Table {
    fn read_exact_at(&self, _offset: u64, buf: &mut [u8]) -> io::Result<()> {
        buf.fill(self.0);
        Ok(())
    }

    fn len(&self) -> io::Result<u64> {
        Ok(1)
    }
}

fn table(n: u8) -> io::Result<Arc<dyn ReadFile>> {
    Ok(Arc::new(Table(n)))
}

fn byte(held: &slots::Held<'_>) -> u8 {
    let mut buf = [0u8; 1];
    held.file().read_exact_at(0, &mut buf).unwrap();
    buf[0]
}

/// Counts how often it is told a slot freed.
#[derive(Default)]
struct Waiter(AtomicUsize);

impl SlotWaiter for Waiter {
    fn slot_freed(&self) {
        self.0.fetch_add(1, StdOrdering::SeqCst);
    }
}

impl Waiter {
    fn told(&self) -> usize {
        self.0.load(StdOrdering::SeqCst)
    }
}

/// Wait for `done`, with a backoff that starts at a microsecond, and fail
/// naming `what` once `limit` passes.
fn wait_for(what: &str, limit: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + limit;
    let mut pause = Duration::from_micros(1);
    while !done() {
        assert!(
            Instant::now() < deadline,
            "{what} did not happen in {limit:?}"
        );
        std::thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_millis(1));
    }
}

#[test]
fn a_parked_reopen_opens_nothing_and_the_leave_that_frees_its_slot_wakes_it() {
    let slots = SlotTable::new(1);
    let held = slots.load(1, || table(1)).unwrap();
    let waiter = Arc::new(Waiter::default());
    let opened = AtomicBool::new(false);
    let parked = slots
        .load_or_park(2, waiter.clone(), || {
            opened.store(true, StdOrdering::SeqCst);
            table(2)
        })
        .unwrap();
    assert!(parked.is_none(), "every slot is in use: the reopen parks");
    assert!(
        !opened.load(StdOrdering::SeqCst),
        "a parked reopen opens nothing"
    );
    assert!(slots.wanted(0));
    assert_eq!(waiter.told(), 0);

    // A second reader joins the busy slot; the first leaves. Still busy.
    let again = slots.join(held.index(), 1).unwrap();
    drop(held);
    assert_eq!(waiter.told(), 0, "the slot still has a reader");
    drop(again);
    assert_eq!(waiter.told(), 1, "the last reader's leave wakes the parked");
    assert!(!slots.wanted(0), "the wake clears the mark");

    let held = slots
        .load_or_park(2, waiter.clone(), || table(2))
        .unwrap()
        .expect("the freed slot is claimed");
    assert_eq!(byte(&held), 2);
    drop(held);
    assert_eq!(waiter.told(), 1, "an unmarked slot's leave wakes nobody");
    assert_eq!(slots.open_count(), 1);
}

#[test]
fn a_park_marks_a_slot_being_loaded_and_the_mark_survives_its_publish() {
    let slots = SlotTable::new(1);
    let waiter = Arc::new(Waiter::default());
    let held = slots
        .load(1, || {
            // This load holds the only slot claimed: the park marks it.
            assert_eq!(slots.park(waiter.clone()), Parking::Parked);
            table(1)
        })
        .unwrap();
    assert!(slots.wanted(0), "publishing the loaded slot kept the mark");
    assert_eq!(waiter.told(), 0);
    drop(held);
    assert_eq!(waiter.told(), 1);
}

#[test]
fn a_load_whose_open_fails_after_a_park_marked_its_slot_wakes_the_parked() {
    let slots = SlotTable::new(1);
    let waiter = Arc::new(Waiter::default());
    let failed = slots.load(1, || {
        assert_eq!(slots.park(waiter.clone()), Parking::Parked);
        Err(io::Error::other("the open fails"))
    });
    assert!(failed.is_err());
    assert_eq!(waiter.told(), 1, "the slot went back to empty");
    assert!(!slots.wanted(0));
    let held = slots
        .load_or_park(2, Arc::new(Waiter::default()), || table(2))
        .unwrap()
        .expect("the emptied slot is claimed");
    assert_eq!(byte(&held), 2);
}

#[test]
fn a_park_that_finds_a_slot_freed_since_its_sweep_claims_it_and_empties_the_list() {
    let slots = SlotTable::new(2);
    let held = slots.load(1, || table(1)).unwrap();
    let waiter = Arc::new(Waiter::default());
    let Parking::Claimed(index) = slots.park(waiter.clone()) else {
        panic!("one slot is empty: the park claims it");
    };
    assert_ne!(index, held.index());
    // Its own node went with the wake on the way out, so nothing is left
    // on the list for a later wake to find.
    assert_eq!(waiter.told(), 1);
    let mine = slots.fill(index, 2, || table(2)).unwrap();
    assert_eq!(byte(&mine), 2);
    // The busy slot it passed on the way is marked; its free wakes a list
    // that no longer holds this waiter.
    assert!(slots.wanted(held.index()));
    drop((held, mine));
    assert_eq!(waiter.told(), 1);
    assert!(!slots.wanted(0) && !slots.wanted(1));
}

/// A `Blocking` load drains the only slot while a park has it marked: the
/// draining reader's leave does not free the slot (the drainer takes it),
/// the drainer's claim and publish keep the mark, and its own leave wakes.
#[test]
fn a_drained_slot_keeps_its_mark_and_the_drainers_leave_wakes() {
    let slots = SlotTable::new(1);
    let waiter = Arc::new(Waiter::default());
    let held = slots.load(1, || table(1)).unwrap();
    assert!(
        slots
            .load_or_park(2, waiter.clone(), || table(2))
            .unwrap()
            .is_none()
    );
    std::thread::scope(|scope| {
        let drainer = scope.spawn(|| {
            let held = slots.load(3, || table(3)).unwrap();
            let read = byte(&held);
            (read, slots.wanted(held.index()))
        });
        wait_for("the drain", Duration::from_secs(60), || slots.drained(0));
        drop(held);
        let (read, wanted) = drainer.join().unwrap();
        assert_eq!(read, 3);
        assert!(wanted, "the drainer's claim and publish kept the mark");
    });
    assert_eq!(
        waiter.told(),
        1,
        "the drainer's leave woke the parked, once"
    );
    assert!(!slots.wanted(0));
}

fn write_table(env: &MemEnv, i: u8) -> PathBuf {
    let path = Path::new("/db").join(format!("{i:06}.sst"));
    env.create_dir_all(Path::new("/db")).unwrap();
    env.write(&path, &[i; 64]).unwrap();
    path
}

/// Through the env, in a unit run: the read parks, fails, and the run sees
/// that it parked; the queue gets one `SlotFreed` when the slot frees, and
/// the read then succeeds. Outside a unit run the same read is `Blocking`.
#[test]
fn a_unit_run_reopen_parks_on_its_queue_and_the_queue_is_told_once() {
    let mem = MemEnv::new();
    let path = write_table(&mem, 7);
    let env = OpenFileLimit::new(Arc::new(mem), 1);
    let file = env.open_read(&path).unwrap();
    let queue = Arc::new(QueueShared::new(test_id(), 1 << 20));
    // Another reader holds the only slot, mid-read.
    let busy = env.shared.slots.load(u64::MAX, || table(9)).unwrap();
    let mut buf = [0u8; 64];
    {
        let run = scope::unit_run(&queue);
        let err = file.read_exact_at(0, &mut buf).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::ResourceBusy);
        assert!(run.parked());
    }
    assert!(queue.inbox_is_empty(), "nothing freed yet");
    drop(busy);
    let told: Vec<Message> = queue.take_inbox().collect();
    assert_eq!(told.len(), 1);
    assert!(matches!(told[0], Message::SlotFreed));
    {
        let run = scope::unit_run(&queue);
        file.read_exact_at(0, &mut buf).unwrap();
        assert!(!run.parked());
    }
    assert_eq!(buf, [7; 64]);
}

/// Four threads read through a two-slot table while three others read as
/// queue units, each parking when both slots are in use and reading again
/// only once its queue is told a slot freed. Every read returns its table,
/// the bound holds, nothing is closed in use, and no park is left untold:
/// a lost wake would leave its thread waiting past the deadline.
#[test]
fn concurrent_parks_are_each_told_and_every_read_lands() {
    const TABLES: usize = 8;
    const CAPACITY: usize = 2;
    let counted = Counted::new();
    let paths: Vec<PathBuf> = (0..TABLES)
        .map(|i| {
            let path = Path::new("/db").join(format!("{i:06}.sst"));
            counted.create_dir_all(Path::new("/db")).unwrap();
            counted.write(&path, &[i as u8; 32]).unwrap();
            path
        })
        .collect();
    let env = OpenFileLimit::new(Arc::new(counted.clone()), CAPACITY);
    let handles: Vec<Box<dyn ReadFile>> = paths.iter().map(|p| env.open_read(p).unwrap()).collect();
    let parks = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for t in 0..4 {
            let handles = &handles;
            scope.spawn(move || {
                let mut buf = [0u8; 32];
                for round in 0..300 {
                    let i = (t * 3 + round * 5) % TABLES;
                    handles[i].read_exact_at(0, &mut buf).unwrap();
                    assert_eq!(buf, [i as u8; 32]);
                }
            });
        }
        for t in 0..3 {
            let (handles, parks) = (&handles, &parks);
            scope.spawn(move || {
                let queue = Arc::new(QueueShared::new(test_id(), 1 << 20));
                let mut buf = [0u8; 32];
                for round in 0..150 {
                    let i = (t * 7 + round * 3 + 1) % TABLES;
                    loop {
                        let parked = {
                            let run = scope::unit_run(&queue);
                            match handles[i].read_exact_at(0, &mut buf) {
                                Ok(()) => false,
                                Err(e) if run.parked() => {
                                    assert_eq!(e.kind(), io::ErrorKind::ResourceBusy);
                                    true
                                }
                                Err(e) => panic!("read {i}: {e}"),
                            }
                        };
                        if !parked {
                            break;
                        }
                        parks.fetch_add(1, StdOrdering::Relaxed);
                        wait_for("a parked read's wake", Duration::from_secs(60), || {
                            queue
                                .take_inbox()
                                .any(|message| matches!(message, Message::SlotFreed))
                        });
                    }
                    assert_eq!(buf, [i as u8; 32]);
                }
            });
        }
    });
    assert!(!counted.device.closed_in_use.load(StdOrdering::SeqCst));
    let most = counted.device.most_open.load(StdOrdering::SeqCst);
    assert!(most <= CAPACITY, "{most} descriptors open at once");
    assert!(
        parks.load(StdOrdering::Relaxed) > 0,
        "no read ever parked: the test ran nothing"
    );
}
