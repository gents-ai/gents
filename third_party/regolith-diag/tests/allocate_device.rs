//! What `Db::allocate` costs at the device (plan 3.7): a reservation asks for
//! no sync of its own, and becomes durable with the next one.
//!
//! Counted on an `Env` that sees every sync of the write-ahead log, since a
//! count is the same whatever the host's load. The values an allocation
//! returns, and what survives a crash, are `allocate.rs`'s and
//! `append_allocate_power_loss.rs`'s.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// threads and the filesystem.
#![cfg(not(target_arch = "wasm32"))]

mod common;

use common::watched_env::WatchedEnv;
use regolith::{Db, DurabilityMode, Options};
use tempfile::TempDir;

#[test]
fn an_allocation_never_syncs_the_log_and_the_next_commit_covers_it() {
    for durability in [DurabilityMode::Immediate, DurabilityMode::Eventual] {
        let dir = TempDir::new().unwrap();
        let (env, watch) = WatchedEnv::on_host();
        let db = Db::open(
            dir.path(),
            Options::default()
                .env(env)
                .durability(durability)
                // No worker, so no thread but this one can sync anything.
                .max_background_compactions(0),
        )
        .unwrap();
        // The first write runs whatever the open left owing.
        db.put(b"warm", b"x").unwrap();
        let (log_syncs, dir_syncs) = (watch.log_syncs(), watch.dir_syncs());

        let mut next = 1u64;
        for n in [1u64, 5, 1, 64] {
            assert_eq!(
                db.allocate(b"ids", n).unwrap(),
                next..next + n,
                "{durability:?}"
            );
            next += n;
        }
        // A second counter is a second key, and no more of a reason to sync.
        assert_eq!(db.allocate(b"other", 2).unwrap(), 1..3);
        assert_eq!(
            watch.log_syncs(),
            log_syncs,
            "{durability:?}: an allocation synced the log"
        );
        assert_eq!(
            watch.dir_syncs(),
            dir_syncs,
            "{durability:?}: an allocation synced a directory"
        );

        // The commit that uses a value is ordered after its allocation, so
        // the sync that commit asks for is the one that covers it.
        db.put(b"used", &next.to_be_bytes()).unwrap();
        let synced = watch.log_syncs() - log_syncs;
        match durability {
            DurabilityMode::Immediate => {
                assert!(synced >= 1, "an immediate commit syncs the log")
            }
            _ => assert_eq!(synced, 0, "an eventual commit leaves the sync to the host"),
        }
        drop(db);

        let db = Db::open(dir.path(), Options::default().durability(durability)).unwrap();
        assert_eq!(
            db.allocate(b"ids", 1).unwrap(),
            next..next + 1,
            "{durability:?}"
        );
    }
}
