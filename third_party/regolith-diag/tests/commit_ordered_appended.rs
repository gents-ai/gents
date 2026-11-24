//! `Transaction::appended`: a transaction's own appends to one log, oldest
//! first, in the order its commit gives them positions.
//!
//! The tests are the configurations of `AppendedEntries.tla` run against the
//! code: an empty transaction, appends to two logs, a fresh layout value per
//! call, a savepoint rollback, a skipped once key, and the log after commit.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// the filesystem. The browser suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use std::ops::ControlFlow;
use std::sync::Arc;

use regolith::prelude::*;
use tempfile::TempDir;

/// Entries at `<name>/<20 decimal digits>`, head at `<name>-head`.
struct Journal {
    name: &'static str,
    head: String,
}

impl Journal {
    fn named(name: &'static str) -> Arc<dyn LogLayout> {
        Arc::new(Self {
            name,
            head: format!("{name}-head"),
        })
    }
}

impl LogLayout for Journal {
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

fn open(dir: &TempDir) -> OptimisticTransactionDb {
    OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap()
}

fn texts(entries: Vec<Vec<u8>>) -> Vec<String> {
    entries
        .into_iter()
        .map(|e| String::from_utf8(e).unwrap())
        .collect()
}

/// Every entry of `name`'s log, positions 1 to head.
fn log_of(db: &Db, name: &str) -> Vec<String> {
    let head = db
        .get(format!("{name}-head").as_bytes())
        .unwrap()
        .map_or(0, |b| u64::from_be_bytes(b.try_into().unwrap()));
    (1..=head)
        .map(|p| {
            let bytes = db
                .get(format!("{name}/{p:020}").as_bytes())
                .unwrap()
                .expect("an entry below the head");
            String::from_utf8(bytes).unwrap()
        })
        .collect()
}

#[test]
fn nothing_appended_gives_an_empty_list() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    let tx = db.begin(&TxnOptions::new());
    assert!(tx.appended(&Journal::named("journal")).is_empty());
}

#[test]
fn entries_come_oldest_first() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    let log = Journal::named("journal");
    let tx = db.begin(&TxnOptions::new());
    for entry in ["a", "b", "c"] {
        tx.append(&log, entry.as_bytes(), None).unwrap();
    }
    assert_eq!(texts(tx.appended(&log)), ["a", "b", "c"]);
}

#[test]
fn another_log_is_left_out_and_a_fresh_layout_names_the_same_log() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    let tx = db.begin(&TxnOptions::new());
    tx.append(&Journal::named("journal"), b"1a", None).unwrap();
    tx.append(&Journal::named("other"), b"2a", None).unwrap();
    tx.append(&Journal::named("journal"), b"1b", None).unwrap();
    assert_eq!(texts(tx.appended(&Journal::named("journal"))), ["1a", "1b"]);
    assert_eq!(texts(tx.appended(&Journal::named("other"))), ["2a"]);
    assert!(tx.appended(&Journal::named("third")).is_empty());
}

#[test]
fn a_rollback_to_a_savepoint_keeps_only_the_appends_before_it() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    let log = Journal::named("journal");
    let mut tx = db.begin(&TxnOptions::new());
    tx.append(&log, b"kept", None).unwrap();
    tx.set_savepoint();
    let at_savepoint = tx.appended(&log);
    tx.append(&log, b"dropped-1", None).unwrap();
    tx.append(&Journal::named("other"), b"dropped-2", None)
        .unwrap();
    tx.rollback_to_savepoint().unwrap();
    assert_eq!(tx.appended(&log), at_savepoint);
    assert_eq!(texts(tx.appended(&log)), ["kept"]);
    assert!(tx.appended(&Journal::named("other")).is_empty());
    tx.append(&log, b"after", None).unwrap();
    assert_eq!(texts(tx.appended(&log)), ["kept", "after"]);
}

#[test]
fn an_append_whose_once_key_the_commit_will_skip_is_still_listed() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    let log = Journal::named("journal");
    let first = db.begin(&TxnOptions::new());
    first.append(&log, b"x", Some(b"once")).unwrap();
    first.commit().unwrap();

    let again = db.begin(&TxnOptions::new());
    again.append(&log, b"x", Some(b"once")).unwrap();
    assert_eq!(texts(again.appended(&log)), ["x"]);
    again.commit().unwrap();
    assert_eq!(log_of(db.db(), "journal"), ["x"]);
}

#[test]
fn after_commit_the_log_positions_follow_the_order_appended_returned() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    let log = Journal::named("journal");
    let earlier = db.begin(&TxnOptions::new());
    earlier.append(&log, b"earlier", None).unwrap();
    earlier.commit().unwrap();

    let tx = db.begin(&TxnOptions::new());
    for entry in ["a", "b", "c"] {
        tx.append(&log, entry.as_bytes(), None).unwrap();
        tx.append(&Journal::named("other"), b"noise", None).unwrap();
    }
    let seen = texts(tx.appended(&log));
    tx.commit().unwrap();

    let committed = log_of(db.db(), "journal");
    assert_eq!(committed[0], "earlier");
    assert_eq!(committed[1..], seen[..]);
}

/// What `with_appended` visits, in the order it visits it.
fn visited(tx: &Transaction, log: &Arc<dyn LogLayout>) -> Vec<String> {
    let mut seen = Vec::new();
    tx.with_appended(log, |entry| {
        seen.push(String::from_utf8(entry.to_vec()).unwrap());
        ControlFlow::Continue(())
    });
    seen
}

#[test]
fn with_appended_visits_the_same_entries_newest_first() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    let mut tx = db.begin(&TxnOptions::new());
    assert!(visited(&tx, &Journal::named("journal")).is_empty());
    tx.append(&Journal::named("journal"), b"1a", None).unwrap();
    tx.append(&Journal::named("other"), b"2a", None).unwrap();
    tx.append(&Journal::named("journal"), b"1b", None).unwrap();
    tx.set_savepoint();
    tx.append(&Journal::named("journal"), b"1c", None).unwrap();
    for name in ["journal", "other", "third"] {
        let log = Journal::named(name);
        let mut oldest_first = texts(tx.appended(&log));
        oldest_first.reverse();
        assert_eq!(visited(&tx, &log), oldest_first, "{name}");
    }
    assert_eq!(visited(&tx, &Journal::named("journal")), ["1c", "1b", "1a"]);
    tx.rollback_to_savepoint().unwrap();
    assert_eq!(visited(&tx, &Journal::named("journal")), ["1b", "1a"]);
}

#[test]
fn with_appended_stops_when_the_visit_breaks() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    let log = Journal::named("journal");
    let tx = db.begin(&TxnOptions::new());
    for entry in ["a", "b", "c"] {
        tx.append(&log, entry.as_bytes(), None).unwrap();
    }
    let mut calls = 0;
    tx.with_appended(&log, |_| {
        calls += 1;
        ControlFlow::Break(())
    });
    assert_eq!(calls, 1);
}
