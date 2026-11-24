//! A cursor the caller positioned must stay where it was put.
//!
//! `Entries` seeks on the caller's behalf when it is handed a cursor nobody
//! placed, which is what makes `db.iter().entries()` work without an explicit
//! seek. It used to decide that by asking whether the cursor was valid, and an
//! invalid cursor is also what a seek past the end of the range produces. The
//! two cases needed telling apart: one wants a seek, the other has already had
//! one and must not be undone.

use regolith::{Db, Options};

fn seeded(dir: &std::path::Path) -> Db {
    let db = Db::open(dir, Options::default()).unwrap();
    for key in [b"a", b"b", b"c"] {
        db.put(key, b"v").unwrap();
    }
    db
}

fn keys(
    entries: impl Iterator<Item = regolith::Result<(Vec<u8>, regolith::DbSlice)>>,
) -> Vec<String> {
    entries
        .map(|item| String::from_utf8(item.unwrap().0).unwrap())
        .collect()
}

#[test]
fn scan_stream_from_past_the_last_key_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let db = seeded(dir.path());
    assert!(keys(db.scan_stream(Some(b"z"), None).unwrap()).is_empty());
}

#[test]
fn snapshot_scan_stream_from_past_the_last_key_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let db = seeded(dir.path());
    assert!(keys(db.snapshot().scan_stream(Some(b"z"), None)).is_empty());
}

#[test]
fn scan_stream_from_below_the_first_key_returns_the_range() {
    let dir = tempfile::tempdir().unwrap();
    let db = seeded(dir.path());
    assert_eq!(
        keys(db.scan_stream(Some(b"A"), None).unwrap()),
        ["a", "b", "c"]
    );
}

#[test]
fn entries_rev_from_a_cursor_walked_off_the_low_end_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let db = seeded(dir.path());
    let snapshot = db.snapshot();

    let mut cursor = snapshot.owned_iter();
    cursor.seek_for_prev(b"a");
    assert_eq!(cursor.key(), Some(b"a".as_slice()));
    cursor.prev();
    assert!(!cursor.valid(), "walked below the first key");
    assert!(cursor.positioned(), "but the caller placed it");

    assert!(keys(cursor.entries_rev()).is_empty());
}

#[test]
fn entries_from_a_cursor_walked_off_the_high_end_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let db = seeded(dir.path());
    let snapshot = db.snapshot();

    let mut cursor = snapshot.owned_iter();
    cursor.seek(b"c");
    cursor.next();
    assert!(!cursor.valid(), "walked past the last key");

    assert!(keys(cursor.entries()).is_empty());
}

#[test]
fn entries_from_a_cursor_seeked_past_the_end_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let db = seeded(dir.path());
    let snapshot = db.snapshot();

    let mut cursor = snapshot.owned_iter();
    cursor.seek(b"z");
    assert!(!cursor.valid());

    assert!(keys(cursor.entries()).is_empty());
}

/// The fallback this rests on: a cursor nobody placed still gets seeked, in
/// both directions. Removing the auto-seek entirely would break these.
#[test]
fn an_unpositioned_cursor_still_seeks_itself() {
    let dir = tempfile::tempdir().unwrap();
    let db = seeded(dir.path());
    let snapshot = db.snapshot();

    let forward = snapshot.owned_iter();
    assert!(!forward.positioned(), "fresh cursor is unpositioned");
    assert_eq!(keys(forward.entries()), ["a", "b", "c"]);

    let backward = snapshot.owned_iter();
    assert_eq!(keys(backward.entries_rev()), ["c", "b", "a"]);
}

/// A seek that lands on a key is honoured rather than restarted, which is the
/// behaviour the auto-seek was always meant to leave alone.
#[test]
fn a_seek_that_lands_is_where_iteration_starts() {
    let dir = tempfile::tempdir().unwrap();
    let db = seeded(dir.path());
    let snapshot = db.snapshot();

    let mut forward = snapshot.owned_iter();
    forward.seek(b"b");
    assert_eq!(keys(forward.entries()), ["b", "c"]);

    let mut backward = snapshot.owned_iter();
    backward.seek_for_prev(b"b");
    assert_eq!(keys(backward.entries_rev()), ["b", "a"]);
}
