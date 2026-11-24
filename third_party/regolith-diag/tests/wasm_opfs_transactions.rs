#![cfg(all(target_arch = "wasm32", target_os = "unknown"))]

//! Transactions on OPFS: a commit-ordered append and its commit, in a
//! browser.
//!
//! The other OPFS suites write through `Db` alone. This one opens a
//! transactional database on the same storage and runs what a caller of
//! `Transaction::append` relies on, at both durabilities: several appends in
//! one transaction, two transactions that append side by side, a once key
//! appended twice, a savepoint rollback, the refusals at the call, and the
//! log continuing after the database is closed and the storage mounted
//! again. The native half of the same contract is
//! `tests/commit_ordered_append.rs`.
//!
//! Run against a real browser (`geckodriver` or `chromedriver` on PATH,
//! headless unless `NO_HEADLESS=1`):
//!
//! ```text
//! CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner \
//!   cargo test --target wasm32-unknown-unknown --test wasm_opfs_transactions
//! ```
//!
//! `wasm-pack test` cannot be used here: it appends `--tests` to the
//! cargo invocation, which builds every test target in the package, and
//! the rest of `tests/` is native-only.

use std::sync::Arc;

use regolith::env::opfs::{OpfsEnv, OpfsMode, OpfsOptions};
use regolith::prelude::*;
use regolith::{DurabilityMode, Error};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_dedicated_worker);

/// Entries at `journal/<20 decimal digits>`, head at `journal-head`.
struct Journal;

impl LogLayout for Journal {
    fn head_key(&self) -> &[u8] {
        b"journal-head"
    }

    fn entry_key(&self, position: u64, out: &mut Vec<u8>) {
        out.extend_from_slice(format!("journal/{position:020}").as_bytes());
    }

    fn max_entry_key_len(&self) -> usize {
        "journal/".len() + 20
    }
}

/// A layout whose keys the classifier below does not call a log.
struct Elsewhere;

impl LogLayout for Elsewhere {
    fn head_key(&self) -> &[u8] {
        b"elsewhere-head"
    }

    fn entry_key(&self, position: u64, out: &mut Vec<u8>) {
        out.extend_from_slice(format!("elsewhere/{position:020}").as_bytes());
    }

    fn max_entry_key_len(&self) -> usize {
        "elsewhere/".len() + 20
    }
}

/// The journal and the once keys are log keys.
struct LogKeys;

impl KeyClassifier for LogKeys {
    fn classify(&self, key: &[u8]) -> KeyClass {
        if key.starts_with(b"journal") || key.starts_with(b"once/") {
            KeyClass::Log
        } else {
            KeyClass::Ordinary
        }
    }
}

fn journal() -> Arc<dyn LogLayout> {
    Arc::new(Journal)
}

/// A transactional database that fits a browser tab and runs compaction on
/// the calling thread, which is the only thread this target has.
fn open(env: &OpfsEnv, durability: DurabilityMode) -> OptimisticTransactionDb {
    OptimisticTransactionDb::open(
        env.db_path(),
        Options::embedded().env(env.as_env()).durability(durability),
    )
    .expect("open")
    .with_policy(Arc::new(LogKeys))
}

/// A transaction at the level that consults the classifier.
fn begin(db: &OptimisticTransactionDb) -> Transaction {
    db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel))
}

/// The log's entries, positions 1 to its head, failing on a hole.
fn log_of(db: &Db) -> Vec<String> {
    let head = db.get(b"journal-head").expect("get").map_or(0, |raw| {
        u64::from_be_bytes(raw.try_into().expect("an 8-byte head"))
    });
    (1..=head)
        .map(|position| {
            let entry = db
                .get(format!("journal/{position:020}").as_bytes())
                .expect("get")
                .unwrap_or_else(|| panic!("position {position} is missing below the head {head}"));
            String::from_utf8(entry).expect("a text entry")
        })
        .collect()
}

/// Everything a caller of `append` relies on, in one database, then the
/// same log read back. Returns the entries the log must hold.
fn append_in_every_way(db: &OptimisticTransactionDb) -> Vec<&'static str> {
    let log = journal();

    // Several appends in one transaction take consecutive positions.
    let tx = begin(db);
    tx.append(&log, b"first", None).expect("append");
    tx.append(&log, b"second", None).expect("append");
    tx.commit().expect("commit");

    // Two transactions that both appended before either committed: an
    // append is never validated, so both commit, in commit order.
    let (early, late) = (begin(db), begin(db));
    early.append(&log, b"commits second", None).expect("append");
    late.append(&log, b"commits first", None).expect("append");
    late.commit().expect("commit");
    early.commit().expect("an append never conflicts");

    // One once key appended twice in a transaction, and again in a later
    // one, yields one entry.
    let tx = begin(db);
    tx.append(&log, b"once", Some(b"once/k")).expect("append");
    tx.append(&log, b"once again", Some(b"once/k"))
        .expect("append");
    tx.commit().expect("commit");
    let tx = begin(db);
    tx.append(&log, b"once more", Some(b"once/k"))
        .expect("append");
    tx.commit().expect("commit");

    // A savepoint rollback takes back the appends made after it.
    let mut tx = begin(db);
    tx.append(&log, b"before the savepoint", None)
        .expect("append");
    tx.set_savepoint();
    tx.append(&log, b"rolled back", None).expect("append");
    tx.rollback_to_savepoint().expect("rollback");
    tx.commit().expect("commit");

    // Refused at the call, and nothing of it is buffered: a write of a log
    // key, and an append through a layout whose keys are not a log.
    let tx = begin(db);
    assert!(matches!(
        tx.put(b"journal-head", &9u64.to_be_bytes()),
        Err(TransactionError::Engine(Error::LogKeyWrite))
    ));
    let elsewhere: Arc<dyn LogLayout> = Arc::new(Elsewhere);
    assert!(matches!(
        tx.append(&elsewhere, b"refused", None),
        Err(TransactionError::Engine(Error::InvalidArgument(_)))
    ));
    tx.commit()
        .expect("a transaction that buffered nothing commits");

    vec![
        "first",
        "second",
        "commits first",
        "commits second",
        "once",
        "before the savepoint",
    ]
}

#[wasm_bindgen_test]
async fn appends_are_numbered_in_commit_order_and_continue_after_a_remount() {
    for (durability, name) in [
        (DurabilityMode::Immediate, "regolith-test-append-immediate"),
        (DurabilityMode::Eventual, "regolith-test-append-eventual"),
    ] {
        let expected = {
            let env = OpfsEnv::mount(name, OpfsOptions::default())
                .await
                .expect("mount");
            assert_eq!(env.mode(), OpfsMode::Sah);
            let db = open(&env, durability);
            let expected = append_in_every_way(&db);
            assert_eq!(log_of(db.db()), expected, "{durability:?}");
            assert_eq!(db.db().get(b"elsewhere-head").expect("get"), None);
            db.db().close().expect("close");
            expected
        };

        let env = OpfsEnv::mount(name, OpfsOptions::default())
            .await
            .expect("remount");
        let db = open(&env, durability);
        assert_eq!(
            log_of(db.db()),
            expected,
            "{durability:?}: every committed append must survive a remount"
        );
        // The head came back with the entries, so the next append takes the
        // next position, and the once key still holds its own.
        let tx = begin(&db);
        tx.append(&journal(), b"after the remount", None)
            .expect("append");
        tx.append(&journal(), b"once, later", Some(b"once/k"))
            .expect("append");
        tx.commit().expect("commit");
        let mut continued = expected.clone();
        continued.push("after the remount");
        assert_eq!(log_of(db.db()), continued, "{durability:?}");
        db.db().close().expect("close");
    }
}

/// Mirror mode is durable only across `OpfsEnv::persist`: it offers no
/// durable sync, so a database on it runs at Eventual. Appends a persist
/// wrote out come back after a remount; the ones committed after the last
/// persist may be lost, but what comes back is never reordered or holed.
#[wasm_bindgen_test]
async fn appends_in_mirror_mode_come_back_once_they_are_persisted() {
    let name = "regolith-test-append-mirror";
    let options = OpfsOptions {
        force_mode: Some(OpfsMode::Mirror),
        ..OpfsOptions::default()
    };
    let durability = DurabilityMode::Eventual;
    let persisted = {
        let env = OpfsEnv::mount(name, options).await.expect("mount");
        assert_eq!(env.mode(), OpfsMode::Mirror);
        assert!(!env.capabilities().durable_sync);
        let db = open(&env, durability);
        let persisted = append_in_every_way(&db);
        db.db().close().expect("close");
        env.persist().await.expect("persist");
        assert_eq!(env.pending_bytes(), 0);
        persisted
    };

    // Appends committed after the persist, by a database closed but never
    // persisted again.
    let later = ["after the persist", "and after it", "last of all"];
    {
        let env = OpfsEnv::mount(name, options).await.expect("remount");
        let db = open(&env, durability);
        assert_eq!(
            log_of(db.db()),
            persisted,
            "persisted appends must come back"
        );
        let tx = begin(&db);
        tx.append(&journal(), later[0].as_bytes(), None)
            .expect("append");
        tx.append(&journal(), later[1].as_bytes(), None)
            .expect("append");
        tx.commit().expect("commit");
        let tx = begin(&db);
        tx.append(&journal(), later[2].as_bytes(), None)
            .expect("append");
        tx.commit().expect("commit");
        db.db().close().expect("close");
        assert!(
            env.pending_bytes() > 0,
            "the later appends wait for a persist"
        );
    }

    let env = OpfsEnv::mount(name, options).await.expect("remount");
    let db = open(&env, durability);
    // `log_of` fails on a hole below the head.
    let log = log_of(db.db());
    let committed: Vec<&str> = persisted.iter().copied().chain(later).collect();
    assert!(
        log.len() >= persisted.len() && log.iter().zip(&committed).all(|(a, b)| a == b),
        "the log must be the persisted appends and then a prefix of the later ones, in \
         commit order: {log:?}"
    );
    // The log continues at the head that came back.
    let tx = begin(&db);
    tx.append(&journal(), b"after the remount", None)
        .expect("append");
    tx.commit().expect("commit");
    let mut continued = log.clone();
    continued.push("after the remount".to_string());
    assert_eq!(log_of(db.db()), continued);
    db.db().close().expect("close");
}
