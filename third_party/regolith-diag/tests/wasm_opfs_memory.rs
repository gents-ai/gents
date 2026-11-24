#![cfg(all(target_arch = "wasm32", target_os = "unknown"))]

//! What a browser database costs in wasm linear memory.
//!
//! Linear memory grows in 64 KiB pages and is never returned to the host,
//! so every high-water mark a page reaches is permanent for the life of
//! the module. That makes the peak across a whole lifecycle the number
//! that matters, not the steady state.
//!
//! Run against a real browser (`geckodriver` or `chromedriver` on PATH,
//! headless unless `NO_HEADLESS=1`):
//!
//! ```text
//! CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner \
//!   cargo test --target wasm32-unknown-unknown --test wasm_opfs_memory
//! ```
//!
//! `wasm-pack test` cannot be used here: it appends `--tests` to the
//! cargo invocation, which builds every test target in the package, and
//! the rest of `tests/` is native-only.

use regolith::env::opfs::{OpfsEnv, OpfsOptions};
use regolith::{CompactionOutcome, Db, Options};
use wasm_bindgen::JsCast as _;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_dedicated_worker);

/// The budget an embedded profile is meant to hold. Measured peak for
/// this lifecycle is around 2 MiB, so a real regression trips this well
/// before ordinary codegen drift does.
const BUDGET_KIB: usize = 4 * 1024;

fn linear_memory_kib() -> usize {
    let memory: js_sys::WebAssembly::Memory = wasm_bindgen::memory().unchecked_into();
    let buffer: js_sys::ArrayBuffer = js_sys::Reflect::get(&memory, &"buffer".into())
        .expect("WebAssembly.Memory has a buffer")
        .unchecked_into();
    buffer.byte_length() as usize / 1024
}

#[wasm_bindgen_test]
async fn the_whole_lifecycle_fits_the_embedded_budget() {
    let baseline = linear_memory_kib();

    let env = OpfsEnv::mount("regolith-test-memory", OpfsOptions::default())
        .await
        .expect("mount");
    let options = Options::embedded().env(env.as_env());
    let db = Db::open(env.db_path(), options).expect("open");

    for i in 0..5000u32 {
        db.put(format!("k{i:06}").as_bytes(), &[b'v'; 128])
            .expect("put");
    }
    db.flush().expect("flush");
    while db.compact_step().expect("compact step") == CompactionOutcome::DidWork {}
    db.close().expect("close");

    let peak = linear_memory_kib();
    assert!(
        peak <= BUDGET_KIB,
        "open, 5000 puts, flush, compact and close reached {peak} KiB of linear \
         memory (baseline {baseline} KiB), over the {BUDGET_KIB} KiB budget"
    );
}
