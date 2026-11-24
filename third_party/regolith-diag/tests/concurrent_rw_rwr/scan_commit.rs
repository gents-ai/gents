//! Scenario 7: a reader that scans and then commits.
//!
//! Each of eight readers scans a prefix of its own, a write to one of the
//! rows it was returned commits through the helper thread and is acknowledged,
//! and then the reader commits. Which commits abort is the contract of the
//! scan:
//!
//! * a scan is validated, row by row, only at `Serializable`, so a reader that
//!   writes nothing, or writes a key outside the rows, aborts there and
//!   commits at every other level;
//! * a row the scan returned and the reader then writes is validated at every
//!   level as a read from the begin snapshot, so a write of the very bytes the
//!   overwrite stored is not taken for a blind one, on either flavour.

use std::collections::BTreeSet;

use regolith::{IsolationLevel, TransactionError};

use super::support::{Storage, table_lookups};
use super::{Flavour, THREADS, each_case, optimistic, own, pessimistic, with_forced_writes};

const ROWS: usize = 4;

/// The row the helper overwrites under the reader.
const OVERWRITTEN: usize = 1;

const OVERWRITE: &[u8] = b"overwritten";

/// What the reader does between the forced overwrite and its commit.
#[derive(Clone, Copy, Debug)]
enum Then {
    /// Nothing: a commit with no writes still validates its reads.
    ReadOnly,
    /// Write a key outside the rows it scanned.
    WriteElsewhere,
    /// Write the overwritten row, with the bytes the overwrite stored.
    WriteScannedRow,
}

const THENS: [Then; 3] = [Then::ReadOnly, Then::WriteElsewhere, Then::WriteScannedRow];

fn row(r: usize, i: usize) -> Vec<u8> {
    format!("scan/{r}/row-{i}").into_bytes()
}

fn rows(r: usize) -> BTreeSet<Vec<u8>> {
    (0..ROWS).map(|i| row(r, i)).collect()
}

fn scan_then_commit<D: Flavour>(db: &D, level: IsolationLevel, storage: Storage, then: Then) {
    for r in 0..THREADS {
        for key in rows(r) {
            db.raw().put(&key, b"seed").unwrap();
        }
    }
    storage.flush(db.raw());
    with_forced_writes(
        db,
        |db, r| {
            db.raw().put(&row(r, OVERWRITTEN), OVERWRITE).unwrap();
            // The commit that follows must find this version in a table.
            storage.flush(db.raw());
        },
        |r, force| {
            let tx = db.begin_at(level);
            let walked: BTreeSet<Vec<u8>> = tx
                .scan_stream(
                    Some(format!("scan/{r}/").as_bytes()),
                    Some(format!("scan/{r}0").as_bytes()),
                )
                .map(|item| item.unwrap().0)
                .collect();
            assert_eq!(
                walked,
                rows(r),
                "{level:?} {then:?}: reader {r} scans its rows"
            );
            force();
            match then {
                Then::ReadOnly => {}
                Then::WriteElsewhere => tx.put(&own(r), b"x").unwrap(),
                Then::WriteScannedRow => tx.put(&row(r, OVERWRITTEN), OVERWRITE).unwrap(),
            }
            let (result, lookups) = table_lookups(|| tx.commit());
            let aborts =
                matches!(then, Then::WriteScannedRow) || level == IsolationLevel::Serializable;
            if aborts {
                match result {
                    Err(TransactionError::Conflict(conflict)) => assert_eq!(
                        conflict.key(),
                        row(r, OVERWRITTEN),
                        "{level:?} {then:?}: reader {r} conflicts on the overwritten row"
                    ),
                    other => panic!("{level:?} {then:?}: reader {r} should abort: {other:?}"),
                }
                assert!(
                    !storage.is_tables() || lookups > 0,
                    "{level:?} {then:?}: the refused commit never probed the table"
                );
            } else {
                assert!(
                    result.is_ok(),
                    "{level:?} {then:?}: an unvalidated scan commits: {result:?}"
                );
            }
        },
    );
}

#[test]
fn a_scan_reader_aborts_only_at_serializable_or_on_a_scanned_row_it_writes_optimistic() {
    each_case(|storage, level| {
        for then in THENS {
            let dir = tempfile::tempdir().unwrap();
            scan_then_commit(&optimistic(&dir, storage), level, storage, then);
        }
    });
}

#[test]
fn a_scan_reader_aborts_only_at_serializable_or_on_a_scanned_row_it_writes_pessimistic() {
    each_case(|storage, level| {
        for then in THENS {
            let dir = tempfile::tempdir().unwrap();
            scan_then_commit(&pessimistic(&dir, storage), level, storage, then);
        }
    });
}
