//! Property test for `Transaction::append`: for any schedule of overlapping
//! transactions, any commit order and any mix of commits, rollbacks and
//! conflicts, the log is what `Append.lean` proves it is.
//!
//! - `dense_from_one`: the positions are exactly 1 to the head, per log.
//! - `unique_positions`: no two entries share a position, and no entry is
//!   written twice.
//! - `at_most_once`: no once key has two entries, and each holds its entry's
//!   position.
//! - `commit_order`: a smaller position belongs to an earlier commit, or to an
//!   earlier append of the same one.
//! - `snapshot_sees_prefix`: a snapshot taken after any commit sees rows 1 to
//!   its head, all of them, and they never change as later commits land.
//!
//! The state is also compared with a model that numbers the committed
//! transactions in commit order, the ordered step of `Append.lean`.

// Native-only. wasm-pack builds every test target for wasm32.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use proptest::prelude::*;
use regolith::prelude::*;
use regolith::{DurabilityMode, MemEnv, Snapshot};

const LOGS: [&str; 2] = ["alpha", "beta"];
const ONCE_KEYS: u8 = 3;

struct Named {
    name: &'static str,
    head: String,
}

impl LogLayout for Named {
    fn head_key(&self) -> &[u8] {
        self.head.as_bytes()
    }

    fn entry_key(&self, position: u64, out: &mut Vec<u8>) {
        out.extend_from_slice(format!("{}/{position:020}", self.name).as_bytes());
    }

    fn max_entry_key_len(&self) -> usize {
        self.name.len() + 1 + 20
    }
}

fn layouts() -> Vec<Arc<dyn LogLayout>> {
    LOGS.iter()
        .map(|&name| {
            Arc::new(Named {
                name,
                head: format!("{name}-head"),
            }) as Arc<dyn LogLayout>
        })
        .collect()
}

#[derive(Clone, Copy, Debug)]
enum Fate {
    Commit,
    Rollback,
    Conflict,
}

#[derive(Clone, Debug)]
struct Plan {
    /// `(log, once key)` of each append, in call order.
    appends: Vec<(usize, Option<u8>)>,
    fate: Fate,
}

fn plan() -> impl Strategy<Value = Plan> {
    let append = (0..LOGS.len(), prop::option::of(0..ONCE_KEYS));
    (
        prop::collection::vec(append, 0..4),
        prop_oneof![
            6 => Just(Fate::Commit),
            1 => Just(Fate::Rollback),
            1 => Just(Fate::Conflict),
        ],
    )
        .prop_map(|(appends, fate)| Plan { appends, fate })
}

/// A schedule: the plans and the order their transactions resolve in.
fn schedule() -> impl Strategy<Value = (Vec<Plan>, Vec<usize>)> {
    prop::collection::vec(plan(), 1..9).prop_flat_map(|plans| {
        let order = Just((0..plans.len()).collect::<Vec<_>>()).prop_shuffle();
        (Just(plans), order)
    })
}

fn be(bytes: Vec<u8>) -> u64 {
    u64::from_be_bytes(bytes.try_into().expect("a position is eight bytes"))
}

fn entry_text(txn: usize, append: usize) -> String {
    format!("t{txn}-a{append}")
}

/// What the ordered step of `Append.lean` leaves, for the committed plans in
/// commit order.
#[derive(Debug, Default, PartialEq)]
struct Model {
    heads: [u64; LOGS.len()],
    /// Per log, the entries in position order.
    rows: [Vec<String>; LOGS.len()],
    onces: HashMap<u8, u64>,
}

impl Model {
    fn commit(&mut self, txn: usize, plan: &Plan) {
        for (i, &(log, once)) in plan.appends.iter().enumerate() {
            if once.is_some_and(|o| self.onces.contains_key(&o)) {
                continue;
            }
            self.heads[log] += 1;
            self.rows[log].push(entry_text(txn, i));
            if let Some(o) = once {
                self.onces.insert(o, self.heads[log]);
            }
        }
    }
}

/// Read a snapshot's logs: per log its head and every row up to it, failing
/// on a missing row.
fn read_logs(snap: &Snapshot) -> [Vec<String>; LOGS.len()] {
    std::array::from_fn(|log| {
        let name = LOGS[log];
        let head = snap
            .get(format!("{name}-head").as_bytes())
            .unwrap()
            .map_or(0, be);
        (1..=head)
            .map(|p| {
                let row = snap
                    .get(format!("{name}/{p:020}").as_bytes())
                    .unwrap()
                    .unwrap_or_else(|| panic!("{name} position {p} missing below head {head}"));
                String::from_utf8(row).unwrap()
            })
            .collect()
    })
}

fn run(plans: &[Plan], order: &[usize], durability: DurabilityMode) {
    let env = MemEnv::new();
    let db = OptimisticTransactionDb::open(
        Path::new("/prop"),
        Options::default()
            .env(Arc::new(env))
            .max_background_compactions(0)
            .durability(durability),
    )
    .unwrap();
    let layouts = layouts();

    // Every transaction begins before any resolves, so they all overlap.
    let mut txns: Vec<Option<Transaction>> = plans
        .iter()
        .map(|_| Some(db.begin(&TxnOptions::new())))
        .collect();
    for (t, plan) in plans.iter().enumerate() {
        let tx = txns[t].as_ref().unwrap();
        for (i, &(log, once)) in plan.appends.iter().enumerate() {
            let once = once.map(|o| format!("once/{o}"));
            tx.append(
                &layouts[log],
                entry_text(t, i).as_bytes(),
                once.as_deref().map(str::as_bytes),
            )
            .unwrap();
        }
        if matches!(plan.fate, Fate::Conflict) {
            tx.put(format!("contended{t}").as_bytes(), b"mine").unwrap();
        }
    }
    for (t, plan) in plans.iter().enumerate() {
        if matches!(plan.fate, Fate::Conflict) {
            db.db()
                .put(format!("contended{t}").as_bytes(), b"theirs")
                .unwrap();
        }
    }

    let mut model = Model::default();
    let mut rank_of: HashMap<usize, usize> = HashMap::new();
    let mut snapshots: Vec<(Snapshot, [Vec<String>; LOGS.len()])> = Vec::new();
    for &t in order {
        let tx = txns[t].take().unwrap();
        match plans[t].fate {
            Fate::Commit => {
                tx.commit().unwrap();
                rank_of.insert(t, rank_of.len());
                model.commit(t, &plans[t]);
            }
            Fate::Rollback => tx.rollback(),
            Fate::Conflict => {
                assert!(matches!(tx.commit(), Err(TransactionError::Conflict(_))));
            }
        }
        let snap = db.db().snapshot();
        let rows = read_logs(&snap);
        snapshots.push((snap, rows));
    }

    // The final state is the model's.
    let last = db.db().snapshot();
    let rows = read_logs(&last);
    assert_eq!(
        (0..LOGS.len())
            .map(|l| rows[l].len() as u64)
            .collect::<Vec<_>>(),
        model.heads
    );
    assert_eq!(rows, model.rows);
    for o in 0..ONCE_KEYS {
        let held = db.db().get(format!("once/{o}").as_bytes()).unwrap().map(be);
        assert_eq!(held, model.onces.get(&o).copied(), "once key {o}");
    }

    // Unique: no entry appears twice. Commit order: along each log, the
    // (commit rank, append index) of the entries strictly increases.
    let mut seen = std::collections::HashSet::new();
    for log_rows in &rows {
        let mut previous: Option<(usize, usize)> = None;
        for row in log_rows {
            assert!(seen.insert(row.clone()), "{row} was written twice");
            let (t, a) = row
                .strip_prefix('t')
                .and_then(|r| r.split_once("-a"))
                .map(|(t, a)| (t.parse::<usize>().unwrap(), a.parse::<usize>().unwrap()))
                .unwrap();
            let key = (rank_of[&t], a);
            assert!(
                previous.is_none_or(|p| p < key),
                "{row} out of commit order"
            );
            previous = Some(key);
        }
    }
    // At most once: of the appends sharing a once key, one entry exists.
    for o in 0..ONCE_KEYS {
        let sharing = plans
            .iter()
            .enumerate()
            .filter(|(_, p)| matches!(p.fate, Fate::Commit))
            .flat_map(|(t, p)| {
                p.appends
                    .iter()
                    .enumerate()
                    .filter(move |(_, a)| a.1 == Some(o))
                    .map(move |(i, _)| entry_text(t, i))
            })
            .filter(|text| seen.contains(text))
            .count();
        assert!(sharing <= 1, "once key {o} has {sharing} entries");
    }

    // Prefix: every snapshot still reads what it read when it was taken, and
    // what it read is a prefix of the final log.
    for (snap, rows_then) in &snapshots {
        assert_eq!(&read_logs(snap), rows_then, "a snapshot's rows changed");
        for (then, now) in rows_then.iter().zip(&rows) {
            assert_eq!(&now[..then.len()], &then[..]);
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

    #[test]
    fn any_schedule_leaves_a_dense_unique_commit_ordered_log((plans, order) in schedule()) {
        run(&plans, &order, DurabilityMode::Eventual);
    }
}
