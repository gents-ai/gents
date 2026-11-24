//! The transaction callback paths hold no lock.
//!
//! The owner of a transaction and `close` on another thread race to end it,
//! and neither may wait for the other. This pins that at the source: none of
//! the files that implement the callbacks and the claim may name a mutex, a
//! read-write lock or a condition variable outside a comment. The behaviour
//! those files promise is `tests/transaction_callbacks.rs`'s.

// Native-only. wasm-pack builds every test target for wasm32, and this reads
// the source tree.
#![cfg(not(target_arch = "wasm32"))]

use std::path::Path;

const FILES: [&str; 5] = [
    "src/transaction/callbacks.rs",
    "src/transaction/claim.rs",
    "src/transaction/handoff.rs",
    "src/transaction/queue.rs",
    "src/engine/open_transactions.rs",
];

const LOCKS: [&str; 4] = ["Mutex", "RwLock", "Condvar", ".lock()"];

#[test]
fn the_callback_paths_name_no_lock() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut found = Vec::new();
    for file in FILES {
        let source = std::fs::read_to_string(root.join(file)).expect("read a source file");
        for (number, line) in source.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            if let Some(lock) = LOCKS.iter().find(|lock| code.contains(**lock)) {
                found.push(format!("{file}:{}: {lock}", number + 1));
            }
        }
    }
    assert!(found.is_empty(), "locks on the callback paths: {found:#?}");
}
