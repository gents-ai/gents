//! A stopped write is told when a rollback latches the engine (E35, on the
//! commit pipeline).
//!
//! A failed log write or sync does not latch the engine: the round discards
//! the commits it left unfinished and the pipeline goes on (`ordered.rs`,
//! G2). Only a rollback that cannot cut the log back latches, since the
//! log's tail is then unknown. That latch is reached on the thread that
//! holds the round, with both frontiers of the ring frozen, and it lands
//! the stall's unit like every other latch.

use std::path::PathBuf;
use std::sync::Arc;

use crate::column_family::{DEFAULT_CF_ID, prefix_key};
use crate::engine::wal::fault;
use crate::engine::wal_rotation_tests::SyncFault;
use crate::engine::{DurabilityMode, EngineOptions, RegolithEngine};
use crate::{Error, StallWait, WouldBlock, WriteBatchOp};

/// Fails every cut of a log under one directory until dropped.
struct CutFault(PathBuf);

impl CutFault {
    fn arm(dir: &tempfile::TempDir) -> Self {
        fault::arm_cut_failure(dir.path());
        Self(dir.path().to_path_buf())
    }
}

impl Drop for CutFault {
    fn drop(&mut self) {
        fault::disarm_cut_failure(&self.0);
    }
}

/// One put, applied below the stall admission as an admitted write is.
fn put(engine: &RegolithEngine, key: &[u8], durability: DurabilityMode) -> std::io::Result<u64> {
    let ops = vec![WriteBatchOp::Put {
        key: prefix_key(DEFAULT_CF_ID, key),
        value: b"v".to_vec(),
    }];
    engine.apply_batch(ops, durability, false)
}

/// A worker, one L0 table at the stop, and a compaction trigger the worker
/// never reaches: a stopped write waits for a relief that does not come.
fn stopped_engine() -> (tempfile::TempDir, Arc<RegolithEngine>) {
    let dir = tempfile::TempDir::new().unwrap();
    let engine = RegolithEngine::open(
        dir.path(),
        EngineOptions {
            max_background_compactions: 1,
            level0_stop_writes_trigger: 1,
            l0_compaction_trigger: 100,
            ..EngineOptions::default()
        },
    )
    .unwrap();
    put(&engine, b"k", DurabilityMode::Eventual).unwrap();
    engine.flush_active_memtable().unwrap();
    assert!(matches!(engine.stall_signal().classify(), Some((_, true))));
    (dir, engine)
}

/// The wait of a write the stall stops now.
fn stopped_write(engine: &RegolithEngine) -> StallWait {
    match engine.admit_write(false) {
        Err(Error::WouldBlock(WouldBlock::Stall(wait))) => wait,
        other => panic!("the write was not stopped: {other:?}"),
    }
}

/// A commit's sync fails, the round rolls it back, and the cut fails too:
/// the engine latches from inside the rollback, and the write a stall
/// stopped is told then, not when the database closes.
#[test]
fn a_rollback_that_latches_tells_the_write_a_stall_stopped() {
    let (dir, engine) = stopped_engine();
    let wait = stopped_write(&engine);
    assert!(!wait.is_ready());

    let _sync = SyncFault::arm(&dir);
    let _cut = CutFault::arm(&dir);
    let err =
        put(&engine, b"durable", DurabilityMode::Immediate).expect_err("the commit's sync fails");
    assert!(
        err.to_string().contains("injected WAL sync failure"),
        "{err}"
    );
    assert!(
        engine.is_latched(),
        "setup: the rollback could not cut the log"
    );
    assert!(
        wait.is_ready(),
        "the rollback latched the engine and the stopped write was not told"
    );
    let told = engine
        .admit_write(false)
        .expect_err("a latched engine admits no write")
        .to_string();
    assert!(told.contains("unknown state"), "{told}");
    let fresh = Error::from(
        put(&engine, b"fresh", DurabilityMode::Eventual).expect_err("the engine takes no write"),
    );
    assert_eq!(told, fresh.to_string());
}

/// The same failure with a cut that works latches nothing: the pipeline
/// discarded the commit and goes on, the stop is as it was, and the stopped
/// write goes on waiting for its relief.
#[test]
fn a_rollback_that_recovers_latches_nothing_and_tells_nobody() {
    let (dir, engine) = stopped_engine();
    let wait = stopped_write(&engine);

    let sync = SyncFault::arm(&dir);
    put(&engine, b"durable", DurabilityMode::Immediate).expect_err("the commit's sync fails");
    drop(sync);
    assert!(!engine.is_latched(), "the rollback cut the log back");
    assert!(!wait.is_ready(), "nothing changed what the stall counts");
    put(&engine, b"after", DurabilityMode::Immediate).expect("the pipeline goes on");
    assert!(matches!(engine.stall_signal().classify(), Some((_, true))));
}
