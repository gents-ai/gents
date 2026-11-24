//! The `regolith.disk-available-*` properties report the database's
//! filesystem where the environment has one, and nothing where it does not.

use std::sync::Arc;

use regolith::env::{Env, MemEnv};
use regolith::{Db, Options};

#[cfg(unix)]
#[test]
fn a_store_on_disk_reports_its_free_space_and_inodes() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path(), Options::default()).unwrap();
    let bytes = db.get_int_property("regolith.disk-available-bytes");
    let inodes = db.get_int_property("regolith.disk-available-inodes");
    assert!(bytes.is_some_and(|b| b > 0), "free bytes: {bytes:?}");
    assert!(inodes.is_some(), "free inodes: {inodes:?}");
}

/// A cap on open table files puts a layer between the database and its
/// environment; the question still reaches the environment.
#[cfg(unix)]
#[test]
fn a_store_with_an_open_file_limit_reports_its_free_space_too() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path(), Options::default().max_open_files(8)).unwrap();
    let bytes = db.get_int_property("regolith.disk-available-bytes");
    let inodes = db.get_int_property("regolith.disk-available-inodes");
    assert!(bytes.is_some_and(|b| b > 0), "free bytes: {bytes:?}");
    assert!(inodes.is_some(), "free inodes: {inodes:?}");
}

#[test]
fn an_environment_without_a_filesystem_reports_nothing() {
    let env: Arc<dyn Env> = Arc::new(MemEnv::new());
    let db = Db::open(
        std::path::Path::new("/db"),
        Options::default().env(env).max_background_compactions(0),
    )
    .unwrap();
    assert_eq!(db.get_int_property("regolith.disk-available-bytes"), None);
    assert_eq!(db.get_int_property("regolith.disk-available-inodes"), None);
}
