//! The pessimistic lock table against a sequential model of its policy (D49):
//! barging with bounded bypass, one holder per key, a waiter that keeps its
//! place across calls, a handoff to a waiter passed over `MAX_BYPASS` times,
//! and a wake for every waiter the lock may now serve.
//!
//! The model is the policy written the plain way, one key at a time, with no
//! atomics. Random schedules of `try_get_for_update`, transaction ends and
//! contention rounds run against the database and the model together, on one
//! thread, and every call's outcome and every wait's readiness must agree.
#![cfg(not(target_arch = "wasm32"))]

use proptest::prelude::*;
use regolith::sync::MAX_BYPASS;
use regolith::{
    LockWait, Options, Transaction, TransactionDb, TransactionError, TxnOptions, WouldBlock,
};
use tempfile::TempDir;

const SLOTS: usize = 4;
const KEYS: usize = 2;

/// One queued transaction, as the policy sees it.
#[derive(Debug, Clone)]
struct Waiter {
    slot: usize,
    /// Races lost since it queued.
    bypass: u32,
    /// A release asked it to try again and it has not yet.
    nudged: bool,
    /// It reached the bound: the next release is its.
    owed: bool,
}

/// One key's lock.
#[derive(Debug, Default, Clone)]
struct KeyModel {
    holder: Option<usize>,
    /// Waiting transactions, oldest first.
    queue: Vec<Waiter>,
    /// An owed waiter is queued: nobody barges.
    handoff: bool,
    /// A nudge is on its way to a waiter that has not tried again.
    nudge_out: bool,
    /// The holder was handed the lock and has not looked yet.
    handed_unseen: bool,
    /// Per slot: the last wait handed out was told the lock may be free.
    told: [bool; SLOTS],
}

impl KeyModel {
    fn owed(&self) -> usize {
        self.queue.iter().filter(|w| w.owed).count()
    }

    fn hands_off(&self) -> bool {
        self.handoff && self.owed() != 0
    }

    /// Nudge the front waiter, as one pass does.
    fn nudge_front(&mut self) {
        self.nudge_out = true;
        let front = &mut self.queue[0];
        if front.nudged {
            // Already nudged: nothing is sent, and the marker comes down.
            self.nudge_out = false;
        } else {
            front.nudged = true;
            self.told[front.slot] = true;
        }
    }

    /// One drain pass: hand the lock to the front while a handoff is owed,
    /// else wake the front to compete.
    fn pass(&mut self, wake_front: bool) {
        let mut handed = false;
        if self.handoff && self.holder.is_none() && !self.queue.is_empty() {
            let front = self.queue.remove(0);
            self.holder = Some(front.slot);
            self.handed_unseen = true;
            self.told[front.slot] = true;
            handed = true;
        }
        if !self.hands_off()
            && !self.queue.is_empty()
            && ((wake_front && !handed) || self.holder.is_none())
        {
            self.nudge_front();
        }
        if self.owed() == 0 {
            self.handoff = false;
        }
    }

    /// The holder lets go.
    fn unlock(&mut self) {
        self.holder = None;
        self.handed_unseen = false;
        if !self.queue.is_empty() && (!self.nudge_out || self.handoff) {
            self.pass(true);
        }
    }

    /// A waiter leaves the queue without the lock.
    fn withdraw(&mut self, slot: usize) {
        let at = self.queue.iter().position(|w| w.slot == slot).unwrap();
        let gone = self.queue.remove(at);
        self.nudge_out = false;
        self.pass(gone.nudged);
    }

    /// `slot` asks for the lock without waiting. `true`: it holds it now.
    fn try_lock(&mut self, slot: usize) -> bool {
        if self.holder == Some(slot) {
            if self.handed_unseen {
                self.handed_unseen = false;
                self.nudge_out = false;
            }
            return true;
        }
        let Some(at) = self.queue.iter().position(|w| w.slot == slot) else {
            if self.holder.is_none() && !self.handoff {
                self.holder = Some(slot);
                return true;
            }
            self.queue.push(Waiter {
                slot,
                bypass: 0,
                nudged: false,
                owed: false,
            });
            self.told[slot] = false;
            return false;
        };
        self.told[slot] = false;
        if !self.queue[at].nudged {
            return false;
        }
        self.queue[at].nudged = false;
        self.nudge_out = false;
        let free = self.holder.is_none() && !self.handoff;
        if free {
            self.queue.remove(at);
            self.holder = Some(slot);
            self.nudge_out = false;
            self.pass(false);
            return true;
        }
        let last = self.queue[at].bypass + 1 == MAX_BYPASS;
        if !self.handoff {
            self.queue[at].bypass += 1;
            if last {
                // Its last counted loss: it is owed, and barging is shut.
                self.queue[at].owed = true;
                self.handoff = true;
                self.pass(false);
            }
        }
        false
    }
}

#[derive(Debug, Clone)]
enum Op {
    /// `try_get_for_update` of a key by a slot's transaction.
    Try(usize, usize),
    /// Roll a slot's transaction back and begin a fresh one in it.
    End(usize),
    /// Rounds of release, barge and retry on one key: what brings a waiter
    /// to the bypass bound.
    Churn(usize, u32),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (0..SLOTS, 0..KEYS).prop_map(|(slot, key)| Op::Try(slot, key)),
        2 => (0..SLOTS).prop_map(Op::End),
        1 => (0..KEYS, 1..(MAX_BYPASS + 4)).prop_map(|(key, rounds)| Op::Churn(key, rounds)),
    ]
}

/// The database and the model, stepped together.
struct Run {
    db: TransactionDb,
    txns: Vec<Transaction>,
    /// The last wait each slot was handed for each key.
    waits: Vec<[Option<LockWait>; KEYS]>,
    model: [KeyModel; KEYS],
    _dir: TempDir,
}

fn key(n: usize) -> [u8; 1] {
    [b'a' + n as u8]
}

impl Run {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let db = TransactionDb::open(dir.path(), Options::default()).unwrap();
        let txns = (0..SLOTS).map(|_| db.begin(&TxnOptions::new())).collect();
        Self {
            db,
            txns,
            waits: (0..SLOTS).map(|_| [None, None]).collect(),
            model: [KeyModel::default(), KeyModel::default()],
            _dir: dir,
        }
    }

    fn try_lock(&mut self, slot: usize, k: usize) -> Result<(), TestCaseError> {
        let expected = self.model[k].try_lock(slot);
        match self.txns[slot].try_get_for_update(&key(k)) {
            Ok(_) => {
                prop_assert!(expected, "slot {} took key {} the model refuses", slot, k);
                self.waits[slot][k] = None;
            }
            Err(TransactionError::WouldBlock(WouldBlock::Locked(wait))) => {
                prop_assert!(
                    !expected,
                    "slot {} was refused key {} the model grants",
                    slot,
                    k
                );
                self.waits[slot][k] = Some(wait);
            }
            Err(other) => prop_assert!(false, "unexpected error: {:?}", other),
        }
        self.check()
    }

    fn end(&mut self, slot: usize) -> Result<(), TestCaseError> {
        for k in 0..KEYS {
            let model = &mut self.model[k];
            if model.holder == Some(slot) {
                if model.handed_unseen {
                    model.nudge_out = false;
                }
                model.unlock();
            } else if model.queue.iter().any(|w| w.slot == slot) {
                model.withdraw(slot);
            }
            model.told[slot] = false;
            self.waits[slot][k] = None;
        }
        let fresh = self.db.begin(&TxnOptions::new());
        std::mem::replace(&mut self.txns[slot], fresh).rollback();
        self.check()
    }

    fn churn(&mut self, k: usize, rounds: u32) -> Result<(), TestCaseError> {
        for _ in 0..rounds {
            if let Some(holder) = self.model[k].holder {
                self.end(holder)?;
            }
            let barger =
                (0..SLOTS).find(|&slot| !self.model[k].queue.iter().any(|w| w.slot == slot));
            if let Some(barger) = barger {
                self.try_lock(barger, k)?;
            }
            let nudged: Vec<usize> = self.model[k]
                .queue
                .iter()
                .filter(|w| w.nudged)
                .map(|w| w.slot)
                .collect();
            for slot in nudged {
                self.try_lock(slot, k)?;
            }
        }
        Ok(())
    }

    /// Every outstanding wait is ready exactly when the model told it.
    fn check(&self) -> Result<(), TestCaseError> {
        for k in 0..KEYS {
            let model = &self.model[k];
            prop_assert!(model.owed() <= 1, "two waiters owed one lock");
            for waiter in &model.queue {
                prop_assert!(waiter.bypass <= MAX_BYPASS, "a waiter lost past the bound");
            }
            for slot in 0..SLOTS {
                if let Some(wait) = &self.waits[slot][k] {
                    prop_assert_eq!(
                        wait.is_ready(),
                        model.told[slot],
                        "slot {} key {}: the wait's readiness",
                        slot,
                        k
                    );
                }
            }
        }
        Ok(())
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn the_lock_table_follows_the_sequential_model(ops in proptest::collection::vec(op(), 1..60)) {
        let mut run = Run::new();
        for op in ops {
            match op {
                Op::Try(slot, k) => run.try_lock(slot, k)?,
                Op::End(slot) => run.end(slot)?,
                Op::Churn(k, rounds) => run.churn(k, rounds)?,
            }
        }
        // Whatever the schedule left, ending everyone frees every key.
        for slot in 0..SLOTS {
            run.end(slot)?;
        }
        for k in 0..KEYS {
            prop_assert!(run.model[k].holder.is_none() && run.model[k].queue.is_empty());
            run.try_lock(0, k)?;
        }
    }
}

/// The model itself reaches the handoff: a schedule that never got a waiter
/// to the bound would agree with any policy.
#[test]
fn the_model_hands_off_at_the_bound() {
    let mut run = Run::new();
    run.try_lock(0, 0).unwrap();
    run.try_lock(1, 0).unwrap();
    run.churn(0, MAX_BYPASS).unwrap();
    assert!(run.model[0].handoff, "the waiter is owed");
    let holder = run.model[0].holder.unwrap();
    run.end(holder).unwrap();
    assert_eq!(run.model[0].holder, Some(1), "handed to the waiter");
    assert!(!run.model[0].handoff);
}
