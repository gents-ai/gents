//! A rename's two records are paired by the thread that made them.
//!
//! The shim records a rename as two records, its source and then its
//! destination, each written on its own, so two threads renaming at once can
//! leave them interleaved: `A.from, B.from, A.to, B.to`. The reconstruction
//! once kept one pending source for the whole journal, so `A.to` took B's
//! source: A's renamed file was cut back to B's synced length, and B's, with
//! no state at all, to nothing. Two files written, synced and renamed whole
//! came back short, and the verdict of a crash test built on that was about a
//! directory the database never left.

use std::path::{Path, PathBuf};

use crate::common::fault::journal::RECORD_BYTES;
use crate::common::fault::{self, CutPoint, Journal, PowerLossOptions, journal_path_for};
use tempfile::TempDir;

/// `O_WRONLY | O_CREAT`.
const CREATING: i64 = 0o101;

/// One record as the shim writes it, made by thread `tid`.
#[allow(clippy::too_many_arguments)]
fn record(
    seq: u64,
    tid: i64,
    kind: char,
    fd: i64,
    a: i64,
    b: i64,
    ret: i64,
    path: &Path,
) -> String {
    let path = path.to_str().unwrap();
    let mut line = format!("{seq}\t{tid}\t{kind}\t{fd}\t{a}\t{b}\t{ret}\t{path}");
    line.extend(std::iter::repeat_n('\0', RECORD_BYTES - 1 - line.len()));
    line.push('\n');
    line
}

/// The records of thread `tid` writing `len` bytes to `staging` on `fd`,
/// syncing them, and closing it, numbered from `seq`.
fn written_and_synced(seq: u64, tid: i64, fd: i64, staging: &Path, len: i64) -> [String; 4] {
    let none = Path::new("");
    [
        record(seq, tid, 'O', fd, 0, CREATING, fd, staging),
        record(seq + 1, tid, 'W', fd, 0, len, len, none),
        record(seq + 2, tid, 'S', fd, 1, 0, 0, none),
        record(seq + 3, tid, 'C', fd, 0, 0, 0, none),
    ]
}

/// Two threads each write a file under a staging name, sync it, and rename
/// it into place, the two renames interleaved record by record. Both files
/// keep every byte: each rename carries its own thread's file.
#[test]
fn interleaved_renames_each_carry_their_own_file() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    std::fs::create_dir(&db).unwrap();
    let name = |n: &str| -> PathBuf { db.join(n) };
    // What the two renames left on disk: 100 bytes from thread 1, 50 from 2.
    std::fs::write(name("one"), [1u8; 100]).unwrap();
    std::fs::write(name("two"), [2u8; 50]).unwrap();

    let mut records: Vec<String> = Vec::new();
    records.extend(written_and_synced(1, 1, 5, &name("one.tmp"), 100));
    records.extend(written_and_synced(5, 2, 6, &name("two.tmp"), 50));
    records.extend([
        record(9, 1, 'R', -1, 0, 0, 0, &name("one.tmp")),
        record(10, 2, 'R', -1, 0, 0, 0, &name("two.tmp")),
        record(11, 1, 'R', -1, 1, 0, 0, &name("one")),
        record(12, 2, 'R', -1, 1, 0, 0, &name("two")),
    ]);
    let path = journal_path_for(&db);
    std::fs::write(&path, records.concat()).unwrap();
    let journal = Journal::read(&path).unwrap();
    assert_eq!(journal.malformed, 0, "{journal}");

    let report =
        fault::simulate_power_loss_with(&db, &journal, CutPoint::End, &PowerLossOptions::default());
    assert_eq!(
        (
            std::fs::read(name("one")).unwrap().len(),
            std::fs::read(name("two")).unwrap().len(),
        ),
        (100, 50),
        "a renamed file lost bytes it synced before its rename: {report:?}"
    );
    assert_eq!(report.bytes_discarded, 0, "{report:?}");
}
