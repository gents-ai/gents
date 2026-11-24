//! `Transaction::truncate_log` through a `CacheOnly` handle whose log head is
//! in a block the cache does not hold.
//!
//! The head read is the only place a truncate can block. It comes before
//! anything is buffered, so a miss leaves the transaction unchanged, and a
//! retry through `regolith::ready` truncates as a blocking call would.
#![cfg(not(target_arch = "wasm32"))]

mod common;

use std::sync::Arc;

use common::device_env::DeviceEnv;
use regolith::{
    LogLayout, OptimisticTransactionDb, Options, ReadMode, TransactionError, TxnOptions,
    WouldBlock, ready,
};
use tempfile::TempDir;

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
        b"journal/".len() + 20
    }
}

fn entry_key(position: u64) -> Vec<u8> {
    let mut key = Vec::new();
    Journal.entry_key(position, &mut key);
    key
}

/// A journal of three entries in the middle of enough other keys to fill
/// many small blocks on either side, flushed and reopened so the cache holds
/// none of them.
fn cold(dir: &TempDir) -> OptimisticTransactionDb {
    let (env, _device) = DeviceEnv::new();
    let options = || {
        Options::default()
            .env(Arc::<DeviceEnv>::clone(&env))
            .block_size(256)
    };
    {
        let db = OptimisticTransactionDb::open(dir.path(), options()).unwrap();
        let log: Arc<dyn LogLayout> = Arc::new(Journal);
        for text in [&b"one"[..], b"two", b"three"] {
            let tx = db.begin(&TxnOptions::new());
            tx.append(&log, text, None).unwrap();
            tx.commit().unwrap();
        }
        for i in 0..2_000u32 {
            db.db().put(format!("a{i:05}").as_bytes(), b"x").unwrap();
            db.db().put(format!("k{i:05}").as_bytes(), b"x").unwrap();
        }
        db.db().flush().unwrap();
        db.db().close().unwrap();
    }
    OptimisticTransactionDb::open(dir.path(), options()).unwrap()
}

#[test]
fn a_cold_head_waits_with_nothing_buffered_and_the_retry_truncates() {
    let dir = TempDir::new().unwrap();
    let db = cold(&dir);
    let log: Arc<dyn LogLayout> = Arc::new(Journal);
    let mut queue = db.db().io_queue();
    let cache_only = TxnOptions::new().read_mode(ReadMode::CacheOnly(queue.id()));

    let missed = db.begin(&cache_only);
    assert!(matches!(
        missed.truncate_log(&log, 1..=2),
        Err(TransactionError::WouldBlock(WouldBlock::Io(_)))
    ));
    missed.commit().unwrap();
    for position in 1..=3 {
        assert!(
            db.db().get(&entry_key(position)).unwrap().is_some(),
            "a missed truncate buffered a delete of entry {position}"
        );
    }

    let tx = db.begin(&cache_only);
    queue
        .block_on(ready(|| tx.truncate_log(&log, 1..=2)))
        .expect("the retry reads the head once the queue loaded it");
    tx.commit().unwrap();
    assert_eq!(db.db().get(&entry_key(1)).unwrap(), None);
    assert_eq!(db.db().get(&entry_key(2)).unwrap(), None);
    assert_eq!(
        db.db().get(&entry_key(3)).unwrap().as_deref(),
        Some(&b"three"[..])
    );
    assert_eq!(
        db.db().get(Journal.head_key()).unwrap().as_deref(),
        Some(&3u64.to_be_bytes()[..])
    );
}
