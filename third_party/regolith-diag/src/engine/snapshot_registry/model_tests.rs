//! The registration protocol against a model, one step at a time.
//!
//! proptest picks an interleaving of protocol steps: handles on a few
//! threads read the horizon, announce, confirm, clone and release, commits
//! advance the horizon, and a compaction samples and scans slot by slot.
//! Four threads share two slots, so every slot is shared. Every step runs
//! on the real registry; the model only tracks what each handle holds.
//!
//! When a compaction finishes its scan the test checks the rules
//! `proofs/tla/SnapshotRegistry.tla` states:
//! - **MinBelowLive**: the least of the sampled horizon and the list is at
//!   most every snapshot live then;
//! - **LiveCovered**: every live snapshot is in the list, or at or above
//!   the sampled horizon;
//! - **the slack**: every listed value was announced by a pin that held it
//!   at some moment of the scan, and nothing else.
//!
//! After every step the registry's full list and count must equal the
//! model's exactly.

use std::collections::BTreeSet;

use proptest::prelude::*;

use super::*;

const HANDLES: usize = 5;
const THREADS: usize = 4;
const WIDTH: usize = 2;
/// The horizon stops here, so values repeat and pins join entries.
const MAX_SEQ: u64 = 5;

#[derive(Debug, Clone, Copy)]
enum Step {
    Advance,
    Read(usize),
    Announce(usize),
    Confirm(usize),
    Release(usize),
    Clone { from: usize, to: usize },
    Sample,
    ScanSlot(usize),
    Finish,
}

fn step() -> impl Strategy<Value = Step> {
    (0u8..9, 0..HANDLES, 0..HANDLES).prop_map(|(kind, a, b)| match kind {
        0 => Step::Advance,
        1 => Step::Read(a),
        2 => Step::Announce(a),
        3 => Step::Confirm(a),
        4 => Step::Release(a),
        5 => Step::Clone { from: a, to: b },
        6 => Step::Sample,
        7 => Step::ScanSlot(a % WIDTH),
        _ => Step::Finish,
    })
}

/// What one handle holds.
#[derive(Debug)]
enum Handle {
    Idle,
    Read(u64),
    Announced(u64, SnapshotPin),
    Live(u64, SnapshotPin),
    Done,
}

impl Handle {
    /// The sequence this handle has a count on, if any.
    fn held(&self) -> Option<u64> {
        match self {
            Handle::Announced(seq, _) | Handle::Live(seq, _) => Some(*seq),
            _ => None,
        }
    }
}

/// A compaction between its sample and its use.
struct Scan {
    /// The horizon at the sample.
    c: u64,
    scanned: BTreeSet<usize>,
    list: Vec<u64>,
    /// Every sequence a pin held at some moment since the sample.
    held_during: BTreeSet<u64>,
}

struct Model {
    r: SnapshotRegistry,
    horizon: ReadHorizon,
    handles: Vec<Handle>,
    scan: Option<Scan>,
}

impl Model {
    fn new() -> Self {
        Self {
            r: SnapshotRegistry::with_width(crate::env::std_env(), WIDTH),
            horizon: ReadHorizon::new(0),
            handles: (0..HANDLES).map(|_| Handle::Idle).collect(),
            scan: None,
        }
    }

    /// The slot handle `h`'s thread announces in.
    fn slot(h: usize) -> u32 {
        ((h % THREADS) % WIDTH) as u32
    }

    fn note_held(&mut self, seq: u64) {
        if let Some(scan) = &mut self.scan {
            scan.held_during.insert(seq);
        }
    }

    fn run(&mut self, step: Step) -> Result<(), TestCaseError> {
        match step {
            Step::Advance => {
                let now = self.horizon.visible();
                if now < MAX_SEQ {
                    self.horizon.publish(now + 1);
                }
            }
            Step::Read(h) => {
                if matches!(self.handles[h], Handle::Idle) {
                    self.handles[h] = Handle::Read(self.horizon.visible());
                }
            }
            Step::Announce(h) => {
                if let Handle::Read(seq) = self.handles[h] {
                    let at = self.r.announce(Self::slot(h), seq);
                    self.handles[h] =
                        Handle::Announced(seq, SnapshotPin::announced(Self::slot(h), at));
                    self.note_held(seq);
                }
            }
            Step::Confirm(h) => {
                if let Handle::Announced(seq, _) = self.handles[h] {
                    let Handle::Announced(_, pin) =
                        std::mem::replace(&mut self.handles[h], Handle::Done)
                    else {
                        unreachable!()
                    };
                    self.handles[h] = match self.r.confirm(&self.horizon, seq) {
                        Ok(()) => Handle::Live(seq, pin),
                        Err(now) => {
                            self.r.release(pin);
                            Handle::Read(now)
                        }
                    };
                }
            }
            Step::Release(h) => {
                if matches!(self.handles[h], Handle::Live(..)) {
                    // Back to idle: the handle may register again.
                    let Handle::Live(_, pin) =
                        std::mem::replace(&mut self.handles[h], Handle::Idle)
                    else {
                        unreachable!()
                    };
                    self.r.release(pin);
                }
            }
            Step::Clone { from, to } => {
                if let (Handle::Live(seq, pin), Handle::Idle) =
                    (&self.handles[from], &self.handles[to])
                {
                    let seq = *seq;
                    let copy = self.r.clone_pin(pin);
                    self.handles[to] = Handle::Live(seq, copy);
                    self.note_held(seq);
                }
            }
            Step::Sample => {
                if self.scan.is_none() {
                    self.r.sample();
                    let held_during = self.handles.iter().filter_map(Handle::held).collect();
                    self.scan = Some(Scan {
                        c: self.horizon.visible(),
                        scanned: BTreeSet::new(),
                        list: Vec::new(),
                        held_during,
                    });
                }
            }
            Step::ScanSlot(slot) => {
                if let Some(scan) = &mut self.scan
                    && scan.scanned.insert(slot)
                {
                    self.r
                        .scan_slot(slot, &mut |entry: Announced| scan.list.push(entry.seq));
                }
            }
            Step::Finish => {
                if self.scan.as_ref().is_some_and(|s| s.scanned.len() == WIDTH) {
                    let scan = self.scan.take().unwrap();
                    self.check_scan(&scan)?;
                }
            }
        }
        self.check_exact()
    }

    fn check_scan(&self, scan: &Scan) -> Result<(), TestCaseError> {
        let m = scan.list.iter().copied().fold(scan.c, u64::min);
        for handle in &self.handles {
            if let Handle::Live(seq, _) = handle {
                prop_assert!(m <= *seq, "MinBelowLive: minimum {m} above live {seq}");
                prop_assert!(
                    scan.list.contains(seq) || scan.c <= *seq,
                    "LiveCovered: live {seq} neither listed in {:?} nor at or above {}",
                    scan.list,
                    scan.c
                );
            }
        }
        for value in &scan.list {
            prop_assert!(
                scan.held_during.contains(value),
                "slack: {value} listed but no pin held it during the scan"
            );
        }
        Ok(())
    }

    fn check_exact(&self) -> Result<(), TestCaseError> {
        let held: Vec<u64> = self.handles.iter().filter_map(Handle::held).collect();
        let distinct: Vec<u64> = held
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        prop_assert_eq!(self.r.live_seqs(), distinct);
        prop_assert_eq!(self.r.live_count(), held.len() as u64);
        Ok(())
    }

    fn finish(mut self) {
        for handle in std::mem::take(&mut self.handles) {
            if let Handle::Announced(_, pin) | Handle::Live(_, pin) = handle {
                self.r.release(pin);
            }
        }
        assert_eq!(self.r.live_count(), 0);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    /// Every interleaving proptest finds keeps the compaction's list
    /// within the documented bounds, and the registry exact.
    #[test]
    fn the_registry_matches_the_model_under_every_interleaving(
        steps in proptest::collection::vec(step(), 0..120)
    ) {
        let mut model = Model::new();
        for step in steps {
            model.run(step)?;
        }
        model.finish();
    }
}
