#![cfg(not(target_arch = "wasm32"))]

use regolith::{OptimisticTransactionDb, Options, PerfContext, PerfLevel, ReadMode, TxnOptions};
use tempfile::TempDir;

#[test]
fn validation_resumes_after_a_cold_miss_without_revisiting_the_checked_prefix() {
    const KEYS: u32 = 2_000;
    let dir = TempDir::new().unwrap();
    let options = || Options::default().block_size(256).block_cache_size(65_536);
    {
        let db = OptimisticTransactionDb::open(dir.path(), options()).unwrap();
        for i in 0..KEYS {
            db.db()
                .put(format!("k{i:05}").as_bytes(), &[b'x'; 64])
                .unwrap();
        }
        db.db().flush().unwrap();
        db.db().close().unwrap();
    }
    let db = OptimisticTransactionDb::open(dir.path(), options()).unwrap();
    let mut queue = db.db().io_queue();
    let txn = db.begin(&TxnOptions::new().read_mode(ReadMode::CacheOnly(queue.id())));
    for i in 0..KEYS {
        txn.put(format!("k{i:05}").as_bytes(), b"mine").unwrap();
    }
    db.db().put(b"other", b"x").unwrap();
    db.db().flush().unwrap();
    let previous = PerfContext::set_level(PerfLevel::EnableCount);
    PerfContext::reset();
    queue.block_on(txn.commit_nowait()).unwrap();
    let lookups = PerfContext::capture().block_cache_lookup_count;
    PerfContext::set_level(previous);
    eprintln!("validation cache probes: {lookups} for {KEYS} keys");
    assert!(
        lookups <= u64::from(KEYS) * 16,
        "validation performed {lookups} cache probes for {KEYS} keys"
    );
    assert_eq!(db.db().get(b"k01000").unwrap(), Some(b"mine".to_vec()));
}
