//! Models of `engine::step`, the step the version store's sections run in:
//! sections never overlap (ST1), none is left on the list with nobody to
//! run it (ST2), and a caller parked on its section is woken (ST4).
//!
//! The positive model drives the production [`Step`], whose list and whose
//! parking are loom's under `--cfg loom`. The calibration rebuilds the one
//! compare-and-swap ST2 rests on out of plain loom atomics, gets it wrong,
//! and must fail.

use loom::cell::UnsafeCell;
use loom::sync::Arc;
use loom::sync::atomic::{AtomicU64, Ordering};
use loom::thread;

use super::super::io::stack::Stack;
use super::super::step::Step;
use super::explore_bounded;

/// What the sections of the model share: a cell each writes, so two
/// sections at once are a race loom reports.
struct Guarded {
    step: Step,
    total: UnsafeCell<u64>,
}

// SAFETY: `total` is written only inside a section of `step` (ST1); the
// model exists to check exactly that.
#[allow(unsafe_code)]
unsafe impl Sync for Guarded {}

/// ST1, ST2 and ST4: two threads each hand the step one section. Whichever
/// push finds the step free runs both sections, or each runs its own; the
/// sections never overlap, both run, each caller gets its own section's
/// answer, and a caller that parked on its section is woken: a parked
/// thread nobody wakes is a deadlock loom reports.
///
/// Bound: 3 preemptions. Two sections, the smallest number one of which can
/// be pushed while the other's thread holds the step.
pub fn sections_never_overlap_and_none_is_left_behind() {
    explore_bounded(
        "sections_never_overlap_and_none_is_left_behind",
        Some(3),
        8,
        1,
        |witness| {
            let shared = Arc::new(Guarded {
                step: Step::new(),
                total: UnsafeCell::new(0),
            });
            let callers: Vec<_> = (1..=2u64)
                .map(|mine| {
                    let shared = Arc::clone(&shared);
                    let witness = witness.clone();
                    thread::spawn(move || {
                        let me = thread::current().id();
                        let (answer, ran_here) = shared.step.run(|| {
                            shared.total.with_mut(|total| {
                                // SAFETY: inside a section (ST1).
                                #[allow(unsafe_code)]
                                unsafe {
                                    *total += mine
                                }
                            });
                            (mine * 10, thread::current().id() == me)
                        });
                        assert_eq!(answer, mine * 10, "a caller got another section's answer");
                        if !ran_here {
                            // The thread that held the step ran it.
                            witness.record();
                        }
                    })
                })
                .collect();
            for caller in callers {
                caller.join().expect("caller");
            }
            let total = shared.total.with(|total| {
                // SAFETY: every caller has joined.
                #[allow(unsafe_code)]
                unsafe {
                    *total
                }
            });
            assert_eq!(total, 3, "a section was left on the list");
        },
    );
}

/// Calibration for [`sections_never_overlap_and_none_is_left_behind`]: a
/// holder that gives the step up with a plain store after its sweep,
/// instead of a compare-and-swap that succeeds only on an empty list. A
/// section pushed between the sweep and the store finds the step held,
/// leaves, and nobody runs it.
pub fn a_step_given_up_without_looking_leaves_a_section_behind() {
    explore_bounded(
        "a_step_given_up_without_looking_leaves_a_section_behind",
        Some(3),
        2,
        1,
        |witness| {
            let held = Arc::new(AtomicU64::new(0));
            let list: Arc<Stack<u64>> = Arc::new(Stack::new());
            let ran = Arc::new(AtomicU64::new(0));
            let callers: Vec<_> = (1..=2u64)
                .map(|mine| {
                    let (held, list, ran) =
                        (Arc::clone(&held), Arc::clone(&list), Arc::clone(&ran));
                    thread::spawn(move || {
                        list.push(mine, 0).expect("nothing refuses a push");
                        if held.swap(1, Ordering::SeqCst) == 0 {
                            for section in list.take(0) {
                                ran.fetch_add(section, Ordering::SeqCst);
                            }
                            held.store(0, Ordering::SeqCst);
                        }
                    })
                })
                .collect();
            for caller in callers {
                caller.join().expect("caller");
            }
            witness.record();
            assert_eq!(
                ran.load(Ordering::SeqCst),
                3,
                "a section was left on the list with nobody to run it"
            );
        },
    );
}
