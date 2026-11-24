//! WAL files are ordered by the number in their name, not by the name's
//! text (E34).
//!
//! `wal_filename` pads the id to six digits, so from id 1,000,000 on the
//! text order and the numeric order part ways: `wal_1000000.log` sorts
//! before `wal_999999.log`. Recovery treats the last file of its list as the
//! one the database was writing when it stopped, the only one a crash may
//! have left with a half-written record. These tests put real records in
//! logs 999,999 and 1,000,000 and recover across that boundary.

#![cfg(not(target_arch = "wasm32"))]

use std::path::{Path, PathBuf};

use super::wal::wal_filename;
use super::{Wal, wal_scan};
use crate::column_family::{DEFAULT_CF_ID, prefix_key};
use crate::{Db, Options};

/// The last log id whose padded name is six digits long.
const LAST_SIX_DIGIT_ID: u64 = 999_999;

/// A database directory that was opened once and holds no data, so the
/// logs a test adds are everything recovery has to replay.
fn empty_database() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    drop(Db::open(dir.path(), Options::default()).unwrap());
    dir
}

/// Write log `id` with one put per `(key, value, seq)`, synced and closed.
fn write_log(db: &Path, id: u64, puts: &[(&[u8], &[u8], u64)]) -> PathBuf {
    let path = db.join("wal").join(wal_filename(id));
    let mut wal = Wal::create(&path).unwrap();
    for (key, value, seq) in puts {
        wal.append_put(&prefix_key(DEFAULT_CF_ID, key), value, *seq)
            .unwrap();
    }
    wal.sync_data().unwrap();
    path
}

/// Cut the last record of the log at `path` short, as a power cut during
/// its write would.
fn tear_last_record(path: &Path) {
    let len = std::fs::metadata(path).unwrap().len();
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_len(len - 3).unwrap();
    file.sync_all().unwrap();
}

fn read(db: &Db, key: &[u8]) -> Option<Vec<u8>> {
    db.get(key).unwrap().map(|value| value.to_vec())
}

#[test]
fn the_listing_orders_logs_by_number_across_the_six_digit_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let ids = [
        LAST_SIX_DIGIT_ID + 2,
        7,
        LAST_SIX_DIGIT_ID,
        10_000_000,
        LAST_SIX_DIGIT_ID + 1,
        123_456,
    ];
    for id in ids {
        std::fs::write(dir.path().join(wal_filename(id)), b"").unwrap();
    }
    // Names with no id come first, by name; a file that is no log is left out.
    std::fs::write(dir.path().join("zz-no-id.log"), b"").unwrap();
    std::fs::write(dir.path().join("aa-no-id.wal"), b"").unwrap();
    std::fs::write(dir.path().join("wal_000003.tmp"), b"").unwrap();

    let env = Options::default().env;
    let live = |min_wal_id: u64| -> Vec<String> {
        wal_scan::live(&*env, dir.path(), min_wal_id)
            .unwrap()
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    };
    // Before any reset every log is live, the ones with no id too.
    assert_eq!(
        live(0),
        [
            "aa-no-id.wal",
            "zz-no-id.log",
            "wal_000007.log",
            "wal_123456.log",
            "wal_999999.log",
            "wal_1000000.log",
            "wal_1000001.log",
            "wal_10000000.log",
        ]
    );
    // The minimum is compared as a number as well: a seven-digit log is
    // above a six-digit minimum, and stays in its place.
    assert_eq!(
        live(LAST_SIX_DIGIT_ID),
        [
            "wal_999999.log",
            "wal_1000000.log",
            "wal_1000001.log",
            "wal_10000000.log",
        ]
    );
}

/// The ordinary crash: log 999,999 was closed by a rotation and is still
/// live, and the power went while log 1,000,000 was being written. The torn
/// record is the tail of the newest log, so the database opens with every
/// acknowledged write. Ordered as text, log 1,000,000 comes first, its torn
/// record reads as damage in a closed file, and the open is refused.
#[test]
fn a_power_cut_in_log_1000000_recovers_with_log_999999_still_live() {
    for read_only in [false, true] {
        let dir = empty_database();
        write_log(
            dir.path(),
            LAST_SIX_DIGIT_ID,
            &[(b"both", b"old", 1), (b"earlier", b"kept", 2)],
        );
        let newest = write_log(
            dir.path(),
            LAST_SIX_DIGIT_ID + 1,
            &[
                (b"both", b"new", 3),
                (b"later", b"kept", 4),
                (b"torn", b"lost", 5),
            ],
        );
        tear_last_record(&newest);

        let db = if read_only {
            Db::open_read_only(dir.path(), Options::default())
        } else {
            Db::open(dir.path(), Options::default())
        }
        .unwrap_or_else(|e| panic!("read_only={read_only}: a torn newest log must open: {e}"));
        // The overwrite in the later log wins, and both logs' writes are there.
        assert_eq!(read(&db, b"both").as_deref(), Some(&b"new"[..]));
        assert_eq!(read(&db, b"earlier").as_deref(), Some(&b"kept"[..]));
        assert_eq!(read(&db, b"later").as_deref(), Some(&b"kept"[..]));
        // The write the power cut tore was never acknowledged.
        assert_eq!(read(&db, b"torn"), None);
        if !read_only {
            // The next log takes the next number, not one that exists.
            db.put(b"after", b"reopen").unwrap();
            drop(db);
            let db = Db::open(dir.path(), Options::default()).unwrap();
            assert_eq!(read(&db, b"after").as_deref(), Some(&b"reopen"[..]));
            assert_eq!(read(&db, b"both").as_deref(), Some(&b"new"[..]));
        }
    }
}

/// The dangerous one: log 999,999 lost its last record to damage, and log
/// 1,000,000 holds whole records written after it. No crash leaves that, so
/// the open is refused. Ordered as text, log 999,999 comes last, its damage
/// is taken for a torn tail, and the database opens without an acknowledged
/// write while serving the later ones.
#[test]
fn damage_in_log_999999_is_refused_when_log_1000000_holds_later_writes() {
    for read_only in [false, true] {
        let dir = empty_database();
        let closed = write_log(
            dir.path(),
            LAST_SIX_DIGIT_ID,
            &[(b"earlier", b"kept", 1), (b"acknowledged", b"damaged", 2)],
        );
        write_log(dir.path(), LAST_SIX_DIGIT_ID + 1, &[(b"later", b"kept", 3)]);
        tear_last_record(&closed);

        let opened = if read_only {
            Db::open_read_only(dir.path(), Options::default())
        } else {
            Db::open(dir.path(), Options::default())
        };
        let err = match opened {
            Ok(db) => panic!(
                "read_only={read_only}: opened without an acknowledged write (acknowledged={:?}, \
                 later={:?})",
                read(&db, b"acknowledged"),
                read(&db, b"later")
            ),
            Err(err) => err.to_string(),
        };
        assert!(
            err.contains("wal_999999.log"),
            "the refusal names the damaged log: {err}"
        );
    }
}
