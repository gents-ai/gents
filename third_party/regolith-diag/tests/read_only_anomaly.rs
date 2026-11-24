//! The read-only anomaly of Fekete, O'Neil and O'Neil (2004) against the
//! real engine, with a report that writes nothing at every isolation level.
//!
//! The worked example, the same as in `IsolationLevel`'s docs, the TLA+
//! model (`RepeatableRead.tla`) and the Lean proof (`read_only_anomaly`). A
//! checking account holds 30 and a savings account 0. A withdrawal reads
//! both and takes 25 from checking, plus a fee of 1 when the total it saw is
//! under 35. A deposit puts 20 into savings. Both run at
//! `SnapshotIsolation`. The withdrawal reads first (30 and 0: it will charge
//! the fee); the deposit commits; a report then reads (30, 20); the
//! withdrawal commits checking = 30 - 25 - 1 = 4. No order of the three gives
//! what the report saw: the fee puts the withdrawal before the deposit, and
//! the report puts the deposit before the withdrawal.
//!
//! A report at `RepeatableRead` or `Serializable` checks its reads at its
//! commit, so one that commits after the withdrawal finds checking changed
//! and is refused: it never returns the impossible sheet. Its commit takes
//! no part in the commit pipeline (E50), and the check there is what keeps
//! it. A pessimistic report at `DefraLevel` checks its reads too. A report
//! at `ReadCommitted` or `SnapshotIsolation`, and an optimistic one at
//! `DefraLevel`, checks nothing, commits, and saw one snapshot: what those
//! levels promise, and why the type's docs tell callers to give the writers
//! a validating level. With the writers at `RepeatableRead` the withdrawal
//! is refused and every report is consistent, whenever it commits.

use regolith::{
    Access, IsolationLevel, OptimisticTransactionDb, Options, Transaction, TransactionDb,
    TransactionError, TxnOptions,
};

const LEVELS: [IsolationLevel; 5] = [
    IsolationLevel::ReadCommitted,
    IsolationLevel::SnapshotIsolation,
    IsolationLevel::RepeatableRead,
    IsolationLevel::Serializable,
    IsolationLevel::DefraLevel,
];

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
    fn open(optimistic: bool) -> Self {
        if optimistic {
            Self::Optimistic(OptimisticTransactionDb::open("bank", Options::memory()).unwrap())
        } else {
            Self::Pessimistic(TransactionDb::open("bank", Options::memory()).unwrap())
        }
    }

    fn begin(&self, level: IsolationLevel) -> Transaction {
        let options = TxnOptions::new().isolation(level);
        match self {
            Self::Optimistic(db) => db.begin(&options),
            Self::Pessimistic(db) => db.begin(&options),
        }
    }

    fn get(&self, key: &[u8]) -> u64 {
        balance(match self {
            Self::Optimistic(db) => db.db().get(key).unwrap(),
            Self::Pessimistic(db) => db.db().get(key).unwrap(),
        })
    }

    fn put(&self, key: &[u8], value: u64) {
        match self {
            Self::Optimistic(db) => db.db().put(key, &value.to_be_bytes()).unwrap(),
            Self::Pessimistic(db) => db.db().put(key, &value.to_be_bytes()).unwrap(),
        }
    }
}

/// What one run left: what the report read and how its commit ended, and
/// whether the withdrawal committed.
struct Outcome {
    seen: (u64, u64),
    report: Result<(), TransactionError>,
    withdrawn: bool,
}

/// The anomaly's schedule: the withdrawal reads, the deposit commits, the
/// report reads, then the withdrawal commits before the report when
/// `report_last`, after it otherwise.
fn run(
    optimistic: bool,
    writers: IsolationLevel,
    reader: IsolationLevel,
    report_last: bool,
) -> Outcome {
    let db = AnyDb::open(optimistic);
    db.put(CHECKING, 30);
    let withdrawal = db.begin(writers);
    let (checking, savings) = (
        balance(withdrawal.get(CHECKING).unwrap()),
        balance(withdrawal.get(SAVINGS).unwrap()),
    );
    assert_eq!((checking, savings), (30, 0));

    let deposit = db.begin(writers);
    let before = balance(deposit.get(SAVINGS).unwrap());
    deposit.put(SAVINGS, &(before + 20).to_be_bytes()).unwrap();
    deposit.commit().unwrap();

    let report = db.begin(reader);
    let seen = (
        balance(report.get(CHECKING).unwrap()),
        balance(report.get(SAVINGS).unwrap()),
    );

    let withdraw = |withdrawal: Transaction| {
        let fee = u64::from(checking + savings < 35);
        withdrawal
            .put(CHECKING, &(checking - 25 - fee).to_be_bytes())
            .unwrap();
        withdrawal.commit().is_ok()
    };
    let (report, withdrawn) = if report_last {
        let withdrawn = withdraw(withdrawal);
        (report.commit().map(drop), withdrawn)
    } else {
        let report = report.commit().map(drop);
        (report, withdraw(withdrawal))
    };
    assert_eq!(
        db.get(CHECKING),
        if withdrawn { 4 } else { 30 },
        "the withdrawal's commit decides checking"
    );
    Outcome {
        seen,
        report,
        withdrawn,
    }
}

/// Whether a report at `level` checks its plain reads at its commit.
fn checks_reads(level: IsolationLevel, optimistic: bool) -> bool {
    match level {
        IsolationLevel::RepeatableRead | IsolationLevel::Serializable => true,
        // A pessimistic transaction gets no write-free relaxation.
        IsolationLevel::DefraLevel => !optimistic,
        _ => false,
    }
}

/// Snapshot writers, a report committed after both: where the report checks
/// its reads it is refused on checking, which the withdrawal replaced after
/// the report read it, so the impossible sheet (30, 20) is never committed. Where
/// it checks nothing it commits the one snapshot it read.
#[test]
fn a_report_that_commits_after_the_withdrawal_is_refused_where_it_checks_its_reads() {
    for optimistic in [true, false] {
        for reader in LEVELS {
            let case = format!("{reader:?}, optimistic {optimistic}");
            let out = run(optimistic, IsolationLevel::SnapshotIsolation, reader, true);
            assert!(
                out.withdrawn,
                "{case}: snapshot isolation lets the withdrawal commit"
            );
            assert_eq!(
                out.seen,
                (30, 20),
                "{case}: the report saw the deposit alone"
            );
            if checks_reads(reader, optimistic) {
                let Err(TransactionError::Conflict(lost)) = out.report else {
                    panic!("{case}: the report committed a sheet no order produces");
                };
                assert_eq!(lost.key(), CHECKING, "{case}");
                assert_eq!(lost.mine(), Access::Read, "{case}");
            } else {
                assert!(
                    out.report.is_ok(),
                    "{case}: a report that checks nothing commits"
                );
            }
        }
    }
}

/// The paper's own order: the report commits before the withdrawal. Nothing
/// it read has changed when it commits, so it commits at every level; only
/// the writers' level can keep the sheet consistent, which the next test
/// shows.
#[test]
fn a_report_that_commits_first_commits_at_every_level() {
    for optimistic in [true, false] {
        for reader in LEVELS {
            let out = run(optimistic, IsolationLevel::SnapshotIsolation, reader, false);
            assert!(out.report.is_ok(), "{reader:?}, optimistic {optimistic}");
            assert_eq!(out.seen, (30, 20), "{reader:?}, optimistic {optimistic}");
            assert!(out.withdrawn, "{reader:?}, optimistic {optimistic}");
        }
    }
}

/// Writers at `RepeatableRead`: the withdrawal's read of `y` is checked and
/// it is refused, whichever order the commits come in, so every report
/// committed at any level saw a state of the one serial order, deposit then
/// nothing more.
#[test]
fn validating_writers_leave_every_report_consistent() {
    for optimistic in [true, false] {
        for reader in LEVELS {
            for report_last in [false, true] {
                let out = run(
                    optimistic,
                    IsolationLevel::RepeatableRead,
                    reader,
                    report_last,
                );
                let case = format!("{reader:?}, optimistic {optimistic}, last {report_last}");
                assert!(
                    !out.withdrawn,
                    "{case}: the withdrawal read a y the deposit replaced"
                );
                assert!(out.report.is_ok(), "{case}");
                assert_eq!(out.seen, (30, 20), "{case}");
            }
        }
    }
}
