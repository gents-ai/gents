//! A commit that writes nothing, checked while another commit is in one of
//! the pipeline's windows (E50, `write_free.rs`): decided and not applied,
//! applied and not published, or discarded by a rollback.
//!
//! The worked example, the same as in `RepeatableRead.tla` (the read-only
//! anomaly), `Relaxations.lean` (`read_only_anomaly`) and
//! `tests/read_only_anomaly.rs`. A checking account holds 30 and a savings
//! account 0. A withdrawal reads both and takes 25 from checking, plus a fee
//! of 1 when the total it saw is under 35: having seen 30, it leaves 4. A
//! deposit puts 20 into savings. The withdrawal reads first, the deposit
//! commits, a report reads (30, 20), and the withdrawal commits checking = 4.
//! No order of the three gives the report's (30, 20) beside a final (4, 20):
//! the fee says the withdrawal came before the deposit, and the report says
//! the deposit came first. A report that checks its reads and commits after
//! the withdrawal was decided must be refused.

use std::io;
use std::sync::Arc;

use tempfile::TempDir;

use super::hook::Stage;
use super::park::Help;
use super::pipeline_tests::stopped_at;
use crate::column_family::{DEFAULT_CF_ID, prefix_key};
use crate::{
    Access, IsolationLevel, OptimisticTransactionDb, Options, ScanCheck, ScanDirection,
    Transaction, TransactionDb, TransactionError, TxnOptions,
};

const CHECKING: &[u8] = b"checking";
const SAVINGS: &[u8] = b"savings";

fn balance(bytes: Option<Vec<u8>>) -> u64 {
    bytes.map_or(0, |bytes| {
        u64::from_be_bytes(bytes.try_into().expect("eight bytes"))
    })
}

/// Either flavour behind one type.
enum AnyDb {
    Optimistic(OptimisticTransactionDb),
    Pessimistic(TransactionDb),
}

impl AnyDb {
    fn open(optimistic: bool, dir: &TempDir) -> Self {
        if optimistic {
            Self::Optimistic(OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap())
        } else {
            Self::Pessimistic(TransactionDb::open(dir.path(), Options::default()).unwrap())
        }
    }

    fn db(&self) -> &crate::Db {
        match self {
            Self::Optimistic(db) => db.db(),
            Self::Pessimistic(db) => db.db(),
        }
    }

    fn begin(&self, level: IsolationLevel) -> Transaction {
        let options = TxnOptions::new().isolation(level);
        match self {
            Self::Optimistic(db) => db.begin(&options),
            Self::Pessimistic(db) => db.begin(&options),
        }
    }
}

/// The example up to the report's reads: checking 30, the withdrawal's
/// reads (30, 0) at snapshot isolation, the deposit of 20 committed, and the
/// report's reads (30, 20) at `level`. Returns the withdrawal, its write
/// buffered (checking = 4), and the report.
fn up_to_the_report(db: &AnyDb, level: IsolationLevel) -> (Transaction, Transaction) {
    db.db().put(CHECKING, &30u64.to_be_bytes()).unwrap();
    let withdrawal = db.begin(IsolationLevel::SnapshotIsolation);
    let seen = (
        balance(withdrawal.get(CHECKING).unwrap()),
        balance(withdrawal.get(SAVINGS).unwrap()),
    );
    assert_eq!(seen, (30, 0));
    let deposit = db.begin(IsolationLevel::SnapshotIsolation);
    let before = balance(deposit.get(SAVINGS).unwrap());
    deposit.put(SAVINGS, &(before + 20).to_be_bytes()).unwrap();
    deposit.commit().unwrap();
    let report = db.begin(level);
    let read = (
        balance(report.get(CHECKING).unwrap()),
        balance(report.get(SAVINGS).unwrap()),
    );
    assert_eq!(
        read,
        (30, 20),
        "the report saw the deposit and not the withdrawal"
    );
    let fee = u64::from(seen.0 + seen.1 < 35);
    withdrawal
        .put(CHECKING, &(seen.0 - 25 - fee).to_be_bytes())
        .unwrap();
    (withdrawal, report)
}

/// E50 F2. The withdrawal is decided and stopped before it is applied; the
/// report, at a level that checks its reads, commits then. Its check applies
/// the decided withdrawal itself and finds checking changed: it is refused,
/// as a commit through the pipeline, decided after the withdrawal, would be.
/// The withdrawal then commits as decided. Stopped later, applied and not
/// published, the same holds.
#[test]
fn a_report_checked_after_the_withdrawal_was_decided_is_refused() {
    for optimistic in [true, false] {
        for level in [IsolationLevel::RepeatableRead, IsolationLevel::Serializable] {
            for stage in [Stage::Apply, Stage::Write] {
                let case = format!("{level:?}, optimistic {optimistic}, stopped at {stage:?}");
                let dir = TempDir::new().unwrap();
                let db = AnyDb::open(optimistic, &dir);
                let (withdrawal, report) = up_to_the_report(&db, level);
                let mut refused = None;
                let withdrawn = stopped_at(
                    stage,
                    || withdrawal.commit(),
                    || refused = Some(report.commit()),
                );
                withdrawn.unwrap_or_else(|e| panic!("{case}: the withdrawal failed: {e}"));
                let Some(Err(TransactionError::Conflict(lost))) = refused else {
                    panic!(
                        "{case}: the report committed (30, 20) after the withdrawal: {refused:?}"
                    );
                };
                assert_eq!(lost.key(), CHECKING, "{case}");
                assert_eq!(lost.mine(), Access::Read, "{case}");
                assert_eq!(balance(db.db().get(CHECKING).unwrap()), 4, "{case}");
            }
        }
    }
}

/// The boundary: the withdrawal took its slot and is stopped before it is
/// decided. The report's check does not count it and commits, placed before
/// the withdrawal, which then commits as decided after it: the report
/// committed first, the order no check on the report can refuse (see
/// `IsolationLevel`). Pins where the check's points stop.
#[test]
fn a_report_checked_before_the_withdrawal_is_decided_commits_first() {
    for level in [IsolationLevel::RepeatableRead, IsolationLevel::Serializable] {
        let dir = TempDir::new().unwrap();
        let db = AnyDb::open(true, &dir);
        let (withdrawal, report) = up_to_the_report(&db, level);
        let mut committed = None;
        stopped_at(
            Stage::Decide,
            || withdrawal.commit(),
            || committed = Some(report.commit()),
        )
        .unwrap();
        assert!(matches!(committed, Some(Ok(_))), "{level:?}: {committed:?}");
        assert_eq!(balance(db.db().get(CHECKING).unwrap()), 4, "{level:?}");
    }
}

/// The same window at a level that checks no plain read: the report commits,
/// as snapshot isolation and read committed promise. It read one snapshot.
#[test]
fn a_report_that_checks_nothing_commits_in_the_same_window() {
    for optimistic in [true, false] {
        for level in [
            IsolationLevel::ReadCommitted,
            IsolationLevel::SnapshotIsolation,
        ] {
            let dir = TempDir::new().unwrap();
            let db = AnyDb::open(optimistic, &dir);
            let (withdrawal, report) = up_to_the_report(&db, level);
            let mut committed = None;
            stopped_at(
                Stage::Apply,
                || withdrawal.commit(),
                || committed = Some(report.commit()),
            )
            .unwrap();
            assert!(
                matches!(committed, Some(Ok(_))),
                "{level:?}, optimistic {optimistic}: {committed:?}"
            );
        }
    }
}

/// A commit with nothing to check (ReadCommitted, or SnapshotIsolation with
/// plain reads only) takes no point and touches no other commit's slot: a put
/// stopped after its decision and before its apply is still unapplied when
/// the read-only commit returns, and the decided frontier has not moved.
#[test]
fn a_commit_with_nothing_to_check_touches_no_slot_in_flight() {
    for level in [
        IsolationLevel::ReadCommitted,
        IsolationLevel::SnapshotIsolation,
    ] {
        let dir = TempDir::new().unwrap();
        let db = AnyDb::open(true, &dir);
        db.db().put(CHECKING, &30u64.to_be_bytes()).unwrap();
        let tx = db.begin(level);
        assert_eq!(balance(tx.get(CHECKING).unwrap()), 30);
        let engine = db.db().engine();
        let mut frontier = None;
        stopped_at(
            Stage::Apply,
            || db.db().put(CHECKING, &31u64.to_be_bytes()),
            || {
                let before = engine.last_decided_seq();
                assert!(tx.commit().is_ok());
                frontier = Some((before, engine.last_decided_seq()));
            },
        )
        .unwrap();
        let (before, after) = frontier.unwrap();
        assert_eq!(
            before, after,
            "{level:?}: the read-only commit settled the put"
        );
    }
}

/// How a transaction reads the key it checks.
#[derive(Clone, Copy, Debug)]
enum Read {
    Plain,
    ForUpdate,
}

/// A transaction at `level` that read checking (30) the way `read` says, with
/// a write elsewhere after it so its read lies below the horizon.
fn reader(db: &AnyDb, level: IsolationLevel, read: Read) -> Transaction {
    db.db().put(CHECKING, &30u64.to_be_bytes()).unwrap();
    let tx = db.begin(level);
    let got = match read {
        Read::Plain => tx.get(CHECKING).unwrap(),
        Read::ForUpdate => tx.get_for_update(CHECKING).unwrap(),
    };
    assert_eq!(balance(got), 30);
    db.db().put(b"elsewhere", b"x").unwrap();
    tx
}

/// E50 F1, the verifier's probe. A read checked by value (an optimistic
/// `get_for_update` at DefraLevel) is not refused by an identical rewrite of
/// its key that is decided and not yet applied, or applied and not yet
/// published: the key holds what it read at both points. A rewrite with other
/// bytes in the same window refuses it, as the pipeline would. Reads checked
/// by sequence (RepeatableRead, Serializable, a `get_for_update` at snapshot
/// isolation, a pessimistic one at DefraLevel) are refused by either rewrite,
/// as a commit through the pipeline would be.
#[test]
fn an_identical_rewrite_in_flight_refuses_only_reads_checked_by_sequence() {
    let cases = [
        (true, IsolationLevel::DefraLevel, Read::ForUpdate, false),
        (true, IsolationLevel::RepeatableRead, Read::Plain, true),
        (true, IsolationLevel::Serializable, Read::Plain, true),
        (
            true,
            IsolationLevel::SnapshotIsolation,
            Read::ForUpdate,
            true,
        ),
        (false, IsolationLevel::DefraLevel, Read::ForUpdate, true),
    ];
    for (optimistic, level, read, by_sequence) in cases {
        for stage in [Stage::Apply, Stage::Publish] {
            for (bytes, identical) in [(30u64, true), (31, false)] {
                let case = format!(
                    "{level:?} {read:?}, optimistic {optimistic}, stopped at {stage:?}, \
                     rewrite to {bytes}"
                );
                let dir = TempDir::new().unwrap();
                let db = AnyDb::open(optimistic, &dir);
                let tx = reader(&db, level, read);
                let mut outcome = None;
                stopped_at(
                    stage,
                    || db.db().put(CHECKING, &bytes.to_be_bytes()),
                    || outcome = Some(tx.commit()),
                )
                .unwrap();
                let refused = matches!(outcome, Some(Err(TransactionError::Conflict(_))));
                assert_eq!(refused, by_sequence || !identical, "{case}: {outcome:?}");
                if !refused {
                    assert!(matches!(outcome, Some(Ok(_))), "{case}: {outcome:?}");
                }
            }
        }
    }
}

/// A read checked by value is placed at the published horizon, so it must
/// hold there: checking 30 is rewritten to 31 and published, then a rewrite
/// back to 30 is decided and stopped. At the decided frontier the key holds
/// 30 again, what the transaction read; at the horizon it holds 31, so the
/// commit is refused. Judged at the decided frontier alone it would pass.
#[test]
fn a_value_read_changed_at_the_horizon_is_refused_though_a_decided_write_restores_it() {
    for stage in [Stage::Apply, Stage::Write] {
        let dir = TempDir::new().unwrap();
        let db = AnyDb::open(true, &dir);
        let tx = reader(&db, IsolationLevel::DefraLevel, Read::ForUpdate);
        db.db().put(CHECKING, &31u64.to_be_bytes()).unwrap();
        let mut outcome = None;
        stopped_at(
            stage,
            || db.db().put(CHECKING, &30u64.to_be_bytes()),
            || outcome = Some(tx.commit()),
        )
        .unwrap();
        assert!(
            matches!(outcome, Some(Err(TransactionError::Conflict(_)))),
            "stopped at {stage:?}: {outcome:?}"
        );
    }
}

/// A validated scan that writes nothing is refused by a write into its range
/// that is decided and not yet published, as a commit through the pipeline,
/// decided after it, would be.
#[test]
fn a_validated_scan_is_refused_by_a_decided_write_into_its_range() {
    for level in [
        IsolationLevel::RepeatableRead,
        IsolationLevel::SnapshotIsolation,
    ] {
        for stage in [Stage::Apply, Stage::Write] {
            let case = format!("{level:?}, stopped at {stage:?}");
            let dir = TempDir::new().unwrap();
            let db = AnyDb::open(true, &dir);
            db.db().put(b"seat/1", b"taken").unwrap();
            let tx = db.begin(level);
            let mut cursor = tx.cursor(
                Some(&b"seat/"[..]),
                Some(&b"seat0"[..]),
                ScanDirection::Forward,
                ScanCheck::Range,
            );
            while !cursor.next_page(&tx, 64 * 1024).unwrap().done {}
            db.db().put(b"elsewhere", b"x").unwrap();
            let mut outcome = None;
            stopped_at(
                stage,
                || db.db().put(b"seat/2", b"taken"),
                || outcome = Some(tx.commit()),
            )
            .unwrap();
            let Some(Err(TransactionError::Conflict(lost))) = outcome else {
                panic!("{case}: {outcome:?}");
            };
            assert_eq!(lost.mine(), Access::ScannedRange, "{case}");
            assert_eq!(lost.key(), b"seat/2", "{case}");
        }
    }
}

/// E50 F3. Versions a rollback discarded stay in the view of a database
/// latched read-only when the memtable could not be rebuilt: above every
/// sequence a decided commit holds, never visible to a reader. A write-free
/// commit is not refused by them, a validated scan over them included.
/// Placed here by hand, as the failed rebuild would leave them.
#[test]
fn a_latched_database_s_discarded_versions_refuse_no_check() {
    let cases = [
        (true, IsolationLevel::RepeatableRead, Read::Plain),
        (true, IsolationLevel::Serializable, Read::Plain),
        (true, IsolationLevel::SnapshotIsolation, Read::ForUpdate),
        (true, IsolationLevel::DefraLevel, Read::ForUpdate),
        (false, IsolationLevel::DefraLevel, Read::Plain),
    ];
    for (optimistic, level, read) in cases {
        for scan in [false, true] {
            let case = format!("{level:?} {read:?}, optimistic {optimistic}, scan {scan}");
            let dir = TempDir::new().unwrap();
            let db = AnyDb::open(optimistic, &dir);
            let tx = reader(&db, level, read);
            if scan {
                let mut cursor = tx.cursor(
                    Some(&b"c"[..]),
                    Some(&b"d"[..]),
                    ScanDirection::Forward,
                    ScanCheck::Range,
                );
                while !cursor.next_page(&tx, 64 * 1024).unwrap().done {}
            }
            let engine = db.db().engine();
            let discarded = engine.last_decided_seq() + 1;
            engine.view.load().active.put(
                &prefix_key(DEFAULT_CF_ID, CHECKING),
                &99u64.to_be_bytes(),
                discarded,
            );
            engine.latch_wal_failure(&io::Error::other("the memtable could not be rebuilt"));
            let outcome = tx.commit();
            assert!(
                outcome.is_ok(),
                "{case}: refused by a discarded version: {outcome:?}"
            );
        }
    }
}

/// E50 F3. A rollback that discards a write applied and not yet written
/// leaves nothing for a later check to be refused by: the check made right
/// after it commits, and so does one made after a later commit took the
/// discarded sequence for another key.
#[test]
fn a_rollback_leaves_nothing_to_refuse_a_check() {
    let dir = TempDir::new().unwrap();
    let db = AnyDb::open(true, &dir);
    let first = reader(&db, IsolationLevel::Serializable, Read::Plain);
    let second = db.begin(IsolationLevel::Serializable);
    assert_eq!(balance(second.get(CHECKING).unwrap()), 30);
    let engine = db.db().engine();
    let mut right_after = None;
    let discarded = stopped_at(
        Stage::Write,
        || db.db().put(CHECKING, &99u64.to_be_bytes()),
        || {
            let pipe = engine.pipe().unwrap();
            let log = Arc::clone(&pipe.tip.load().after.log);
            log.poison(io::Error::other("injected log failure"));
            assert!(engine.sync_round(pipe, Help::Wait).is_ok());
            right_after = Some(first.commit());
        },
    );
    assert!(discarded.is_err(), "the rollback discarded the write");
    assert!(matches!(right_after, Some(Ok(_))), "{right_after:?}");
    db.db().put(b"another", b"x").unwrap();
    assert!(second.commit().is_ok());
}
