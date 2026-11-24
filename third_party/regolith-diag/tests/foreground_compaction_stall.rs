//! Adversarial coverage for the foreground-compaction stall path.
//!
//! Every test that could wedge runs its workload on a helper thread and
//! fails on a deadline instead of hanging the harness.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// threads, the filesystem or proptest, none of which exist there. The browser
// suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use regolith::{
    CompactionOutcome, CompactionStyle, Db, Env, Error, FifoCompactionOptions, IsolationLevel,
    MergeOperator, OptimisticTransactionDb, Options, Transaction, TransactionDb, TransactionError,
    TxnOptions,
};
use std::sync::mpsc;
use std::time::Duration;

/// Run `body` on a helper thread, failing the test if it does not finish
/// within `secs`. A wedged engine reports as a failure, never as a hang.
fn with_deadline<F>(name: &str, secs: u64, body: F)
where
    F: FnOnce() + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
        let _ = tx.send(outcome.is_ok());
    });
    // A backstop against a wedge, not a performance gate. Its only job
    // is to turn a hang into a named failure rather than a bare harness
    // timeout, so it has to sit under nextest's own bound and well above
    // anything a merely slow host produces. The per-call numbers below
    // were calibrated on this workstation and are kept only as
    // documentation of what each body ought to cost: at 180s one of them
    // fired on a Windows runner where the engine was working fine, and
    // it fired before the harness could say anything more useful.
    //
    // 480s against the `ci` profile's 600s kill (.config/nextest.toml),
    // which also covers `cargo llvm-cov`, where instrumented code runs
    // several times slower than the gate.
    const WEDGE_BUDGET_SECS: u64 = 480;
    let secs = secs.max(WEDGE_BUDGET_SECS);
    match rx.recv_timeout(Duration::from_secs(secs)) {
        Err(_) => panic!("HUNG: {name} did not finish within {secs}s"),
        Ok(false) => panic!("FAILED (not hung): {name}, see the panic above"),
        Ok(true) => {}
    }
}

fn zero_worker_opts() -> Options {
    Options::default()
        .write_buffer_size(16 * 1024)
        .target_file_size(32 * 1024)
        .level_base_bytes(64 * 1024)
        .l0_compaction_trigger(2)
        .level0_slowdown_writes_trigger(3)
        .level0_stop_writes_trigger(4)
        .max_background_compactions(0)
        .inline_compaction(true)
        .block_cache_size(0)
}

fn val(n: usize) -> Vec<u8> {
    vec![(n % 251) as u8; 512]
}

/// Write until a stall rejects one, and return the reason text. Fails
/// the test if 20000 writes all succeed, so a test that expects a stall
/// cannot silently pass by never reaching one.
fn first_stall_reason(db: &Db, label: &str) -> String {
    for i in 0..20_000usize {
        let k = format!("k{i:08}");
        match db.put(k.as_bytes(), &val(i)) {
            Ok(()) => continue,
            Err(Error::Busy(reason)) => return reason.to_string(),
            Err(e) => panic!("{label}: unexpected error at put #{i}: {e:?}"),
        }
    }
    panic!("{label}: expected a stall within 20000 writes and never saw one");
}

#[test]
fn level_style_zero_workers_survives_the_stop_trigger() {
    with_deadline("level_style_zero_workers", 120, || {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path(), zero_worker_opts()).unwrap();
        for i in 0..20_000usize {
            let k = format!("k{i:08}");
            if let Err(e) = db.put(k.as_bytes(), &val(i)) {
                panic!("put {i} failed: {e:?}");
            }
        }
        for i in (0..20_000usize).step_by(97) {
            let k = format!("k{i:08}");
            assert_eq!(db.get(k.as_bytes()).unwrap().as_deref(), Some(&val(i)[..]));
        }
        db.close().unwrap();
    });
}

#[test]
fn embedded_profile_zero_workers_survives_the_stop_trigger() {
    with_deadline("embedded_profile_zero_workers", 180, || {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path(), Options::embedded()).unwrap();
        for i in 0..40_000usize {
            let k = format!("k{i:08}");
            if let Err(e) = db.put(k.as_bytes(), &val(i)) {
                panic!("embedded put {i} failed: {e:?}");
            }
        }
        db.close().unwrap();
    });
}

/// The L0 *count* triggers are level-style back-pressure. Under FIFO
/// nothing merges L0 files at all, so once the count reaches
/// `level0_stop_writes_trigger` no compaction can relieve it.
///
/// This is pre-existing and not specific to zero workers: the identical
/// configuration with one background worker **hangs** on the base commit
/// (`3e9a5fd`, measured: last accepted put #124, no progress in 45 s).
/// What this PR changes is that the writer now gives up with a bounded,
/// actionable [`Error::Busy`] instead of blocking forever, and the
/// message names the style and the knob.
#[test]
fn fifo_l0_count_trigger_reports_busy_instead_of_hanging() {
    with_deadline("fifo_zero_workers", 120, || {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path(), fifo_opts(0)).unwrap();
        let reason = first_stall_reason(&db, "fifo");
        assert!(
            reason.contains("FIFO compaction never merges them")
                && reason.contains("level0_stop_writes_trigger"),
            "the FIFO stall must name the cause and the knob, got: {reason}"
        );
    });
}

/// Same shape for universal: the picker merges on its own size-ratio and
/// size-amplification rules, and a well-formed size tier satisfies them
/// while holding more L0 files than the level-style count trigger allows.
/// Also pre-existing: one background worker on `3e9a5fd` hangs here too
/// (measured: last accepted put #217, no progress in 45 s).
#[test]
fn universal_l0_count_trigger_reports_busy_instead_of_hanging() {
    with_deadline("universal_zero_workers", 120, || {
        let dir = tempfile::tempdir().unwrap();
        let opts = zero_worker_opts().compaction_style(CompactionStyle::Universal);
        let db = Db::open(dir.path(), opts).unwrap();
        let reason = first_stall_reason(&db, "universal");
        assert!(
            reason.contains("size-amplification rules decline to merge")
                && reason.contains("level0_stop_writes_trigger"),
            "the universal stall must name the cause and the knob, got: {reason}"
        );
    });
}

/// Disabling the level-style trigger is the documented remedy, and it
/// has to actually work: the same workload that stalls with the trigger
/// on must run to completion with it off, under both styles.
#[test]
fn disabling_the_l0_count_trigger_unblocks_fifo_and_universal() {
    with_deadline("trigger_off", 180, || {
        for style in [CompactionStyle::Fifo, CompactionStyle::Universal] {
            let dir = tempfile::tempdir().unwrap();
            let opts = zero_worker_opts()
                .compaction_style(style)
                .level0_slowdown_writes_trigger(0)
                .level0_stop_writes_trigger(0)
                .fifo_compaction_options(FifoCompactionOptions {
                    max_table_files_size: 1024 * 1024 * 1024,
                });
            let db = Db::open(dir.path(), opts).unwrap();
            for i in 0..5_000usize {
                let k = format!("k{i:08}");
                db.put(k.as_bytes(), &val(i))
                    .unwrap_or_else(|e| panic!("{style:?} with the trigger off: put #{i}: {e:?}"));
            }
            assert_eq!(db.get(b"k00004999").unwrap(), Some(val(4999)));
            db.close().unwrap();
        }
    });
}

#[test]
fn concurrent_writers_with_zero_workers_never_return_busy() {
    with_deadline("concurrent_zero_workers", 480, || {
        let dir = tempfile::tempdir().unwrap();
        let db = std::sync::Arc::new(Db::open(dir.path(), zero_worker_opts()).unwrap());
        let mut handles = Vec::new();
        for t in 0..4usize {
            let db = std::sync::Arc::clone(&db);
            handles.push(std::thread::spawn(move || {
                // 1,500 rather than 4,000. With zero background
                // workers every flush and every level promotion runs on
                // these threads, so the cost is dominated by durable
                // operations, which a Windows CI disk serves about two
                // orders of magnitude slower than a Linux tmpfs. The
                // property is that a writer never sees `Busy`, and a
                // writer that survives 1,500 rotations of a 16 KiB
                // buffer has already crossed every stall threshold
                // `zero_worker_opts` sets.
                for i in 0..1500usize {
                    let k = format!("t{t}k{i:08}");
                    // A step another writer holds the inputs of hands this
                    // one the stall's wait, which that writer's step lands.
                    if let Err(e) = regolith::through_stalls(|| db.put(k.as_bytes(), &val(i))) {
                        return Err(format!("thread {t} put {i}: {e:?}"));
                    }
                }
                Ok(())
            }));
        }
        let mut failures = Vec::new();
        for h in handles {
            if let Err(e) = h.join().unwrap() {
                failures.push(e);
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    });
}

#[test]
fn busy_from_a_stall_must_not_have_written_anything() {
    // If a stalled `put` gives up with Busy, the key must not be readable:
    // a rejected write that landed anyway is silent corruption.
    with_deadline("busy_atomicity", 120, || {
        let dir = tempfile::tempdir().unwrap();
        let opts = zero_worker_opts()
            .compaction_style(CompactionStyle::Fifo)
            .fifo_compaction_options(FifoCompactionOptions {
                max_table_files_size: 1024 * 1024 * 1024,
            });
        let db = Db::open(dir.path(), opts).unwrap();
        for i in 0..20_000usize {
            let k = format!("k{i:08}");
            match db.put(k.as_bytes(), &val(i)) {
                Ok(()) => {}
                Err(Error::Busy(reason)) => {
                    let got = db.get(k.as_bytes()).unwrap();
                    assert!(
                        got.is_none(),
                        "put #{i} was rejected with Busy({reason}) but the key is readable"
                    );
                    return;
                }
                Err(e) => panic!("unexpected error at #{i}: {e:?}"),
            }
        }
    });
}

/// Once a stalled write gives up with `Busy` under a style whose picker
/// does not reduce the L0 file count, `compact_step` and `flush` cannot
/// rescue it: there is genuinely nothing to compact. The remedy is the
/// one the error message names, and the data must still be intact.
fn probe_recovery(style: CompactionStyle, label: &str) {
    let dir = tempfile::tempdir().unwrap();
    let opts = zero_worker_opts()
        .compaction_style(style)
        .fifo_compaction_options(FifoCompactionOptions {
            max_table_files_size: 1024 * 1024 * 1024,
        });
    let stalled_at;
    {
        let db = Db::open(dir.path(), opts.clone()).unwrap();
        let mut i = 0usize;
        loop {
            let k = format!("k{i:08}");
            match db.put(k.as_bytes(), &val(i)) {
                Ok(()) => i += 1,
                Err(Error::Busy(_)) => break,
                Err(e) => panic!("{label}: unexpected error at #{i}: {e:?}"),
            }
            assert!(i < 20_000, "{label}: expected a stall and never saw one");
        }
        stalled_at = i;

        // compact_step terminates rather than looping forever, and it
        // reports no work, because there is none to do under this style.
        let mut steps = 0usize;
        while db.compact_step().unwrap() == CompactionOutcome::DidWork {
            steps += 1;
            assert!(steps <= 200, "{label}: compact_step did not converge");
        }
        db.flush().unwrap();
        let retry = db.put(format!("k{stalled_at:08}").as_bytes(), &val(stalled_at));
        assert!(
            matches!(retry, Err(Error::Busy(_))),
            "{label}: expected the documented persistent stall, got {retry:?}"
        );
        db.close().unwrap();
    }

    // The documented remedy: this trigger is level-style, so turn it off.
    // Writing must resume, and nothing written before the stall may have
    // been lost by it.
    let unblocked = opts
        .level0_slowdown_writes_trigger(0)
        .level0_stop_writes_trigger(0);
    let db = Db::open(dir.path(), unblocked).unwrap();
    if style == CompactionStyle::Universal {
        // FIFO legitimately unlinks old files once over its byte cap;
        // universal never drops data, so every earlier key must survive.
        for i in (0..stalled_at).step_by(7) {
            let k = format!("k{i:08}");
            assert_eq!(
                db.get(k.as_bytes()).unwrap(),
                Some(val(i)),
                "{label}: key {k} lost across the stall"
            );
        }
    }
    for i in stalled_at..stalled_at + 500 {
        let k = format!("k{i:08}");
        db.put(k.as_bytes(), &val(i))
            .unwrap_or_else(|e| panic!("{label}: still unwritable with the trigger off: {e:?}"));
    }
    db.close().unwrap();
}

#[test]
fn fifo_busy_names_a_remedy_that_works() {
    with_deadline("fifo_recovery", 180, || {
        probe_recovery(CompactionStyle::Fifo, "fifo")
    });
}

#[test]
fn universal_busy_names_a_remedy_that_works() {
    with_deadline("universal_recovery", 180, || {
        probe_recovery(CompactionStyle::Universal, "universal")
    });
}

fn fifo_opts(workers: usize) -> Options {
    zero_worker_opts()
        .compaction_style(CompactionStyle::Fifo)
        .fifo_compaction_options(FifoCompactionOptions {
            max_table_files_size: 1024 * 1024 * 1024,
        })
        .max_background_compactions(workers)
}

/// A stall that a reopen silently cleared would be worse than one that
/// persists: it would mean back-pressure is not armed until the first
/// memtable rotation. `cached_stall_level` starts at 0, so `Db::open`
/// has to refresh it before the first write is admitted.
#[test]
fn back_pressure_is_armed_before_the_first_write_after_a_reopen() {
    with_deadline("fifo_reopen", 180, || {
        let dir = tempfile::tempdir().unwrap();
        let stalled_at;
        {
            let db = Db::open(dir.path(), fifo_opts(0)).unwrap();
            let mut i = 0usize;
            loop {
                let k = format!("k{i:08}");
                match db.put(k.as_bytes(), &val(i)) {
                    Ok(()) => i += 1,
                    Err(Error::Busy(_)) => break,
                    Err(e) => panic!("unexpected error at #{i}: {e:?}"),
                }
                assert!(i < 20_000, "expected the FIFO config to stall");
            }
            stalled_at = i;
            db.close().unwrap();
        }
        let db = Db::open(dir.path(), fifo_opts(0)).unwrap();
        let mut accepted = 0usize;
        for i in stalled_at..stalled_at + 5_000 {
            let k = format!("k{i:08}");
            match db.put(k.as_bytes(), &val(i)) {
                Ok(()) => accepted += 1,
                Err(_) => break,
            }
        }
        assert_eq!(
            accepted, 0,
            "reopening with L0 already past the stop trigger accepted unthrottled writes; \
             back-pressure was not armed at open"
        );
    });
}

/// `compact_step` is new public API. It must terminate.
#[test]
fn universal_compact_step_converges() {
    with_deadline("universal_converges", 120, || {
        let dir = tempfile::tempdir().unwrap();
        let opts = zero_worker_opts().compaction_style(CompactionStyle::Universal);
        let db = Db::open(dir.path(), opts).unwrap();
        for i in 0..300usize {
            let k = format!("k{i:08}");
            let _ = db.put(k.as_bytes(), &val(i));
        }
        let mut steps = 0usize;
        while db.compact_step().unwrap() == CompactionOutcome::DidWork {
            steps += 1;
            assert!(
                steps <= 200,
                "compact_step still reporting work after {steps} passes with no writer running"
            );
        }
    });
}

/// An environment that reports `durable_sync == false` promises nothing
/// when `sync_all` returns `Ok(())`. `DurabilityMode::Immediate` on such
/// an environment is therefore a durability guarantee the engine cannot
/// keep, and it must be refused rather than silently downgraded.
#[test]
fn immediate_durability_on_a_non_durable_env_is_refused() {
    let env = std::sync::Arc::new(regolith::MemEnv::new());
    let caps = env.capabilities();
    assert!(
        !caps.durable_sync,
        "precondition: MemEnv must report durable_sync = false"
    );
    let opts = Options::default()
        .env(env.clone())
        .durability(regolith::DurabilityMode::Immediate)
        .max_background_compactions(0);
    match Db::open("/mem-durability", opts) {
        Err(e) => {
            println!("refused as documented: {e:?}");
        }
        Ok(db) => {
            regolith::through_stalls(|| db.put(b"k", b"v")).unwrap();
            db.close().unwrap();
            panic!(
                "DurabilityMode::Immediate was accepted on an env with \
                 durable_sync = false; writes report durable but are not"
            );
        }
    }
}

/// `compact_step` is new public API that runs a compaction job on the
/// caller's thread. Driving it from several threads at once, alongside
/// background workers, `compact_range` and live writers, must not lose
/// or corrupt a record.
#[test]
fn compact_step_racing_workers_and_compact_range_loses_nothing() {
    with_deadline("compact_step_race", 300, || {
        let dir = tempfile::tempdir().unwrap();
        let opts = Options::default()
            .write_buffer_size(24 * 1024)
            .target_file_size(48 * 1024)
            .level_base_bytes(96 * 1024)
            .l0_compaction_trigger(2)
            .max_background_compactions(2)
            .block_cache_size(0);
        let db = std::sync::Arc::new(Db::open(dir.path(), opts).unwrap());
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let total = 30_000usize;

        let mut helpers = Vec::new();
        for _ in 0..3 {
            let db = std::sync::Arc::clone(&db);
            let stop = std::sync::Arc::clone(&stop);
            helpers.push(std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    db.compact_step().expect("compact_step");
                }
            }));
        }
        {
            let db = std::sync::Arc::clone(&db);
            let stop = std::sync::Arc::clone(&stop);
            helpers.push(std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    db.compact_range(None, None).wait().expect("compact_range");
                }
            }));
        }

        for i in 0..total {
            let k = format!("key/{i:09}");
            regolith::through_stalls(|| db.put(k.as_bytes(), &val(i)))
                .unwrap_or_else(|e| panic!("put {i}: {e:?}"));
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        for h in helpers {
            h.join().unwrap();
        }

        let mut missing = Vec::new();
        let mut wrong = Vec::new();
        for i in 0..total {
            let k = format!("key/{i:09}");
            match db.get(k.as_bytes()).unwrap() {
                None => missing.push(i),
                Some(v) if v != val(i) => wrong.push(i),
                Some(_) => {}
            }
        }
        assert!(
            missing.is_empty() && wrong.is_empty(),
            "missing {} keys (first: {:?}), wrong {} values (first: {:?})",
            missing.len(),
            missing.first(),
            wrong.len(),
            wrong.first()
        );
        db.close().unwrap();
    });
}

/// An environment with no cross-process file lock must still refuse a
/// second read-write handle on one directory inside this process.
///
/// Before this was enforced, two `Db` handles on one wasm directory both
/// opened, both accepted writes, both returned `Ok(())` from `close`,
/// and the second writer silently lost 94% of its committed records.
/// `Capabilities::file_lock` is `false` on such an environment because
/// a second *process* is not excluded; a second handle here is.
#[test]
fn a_second_handle_is_refused_even_without_a_cross_process_lock() {
    let env = std::sync::Arc::new(regolith::MemEnv::new());
    assert!(
        !env.capabilities().file_lock,
        "precondition: MemEnv reports no cross-process file lock"
    );
    let opts = || {
        Options::default()
            .env(env.clone())
            .max_background_compactions(0)
    };

    let first = Db::open("/lockless", opts()).unwrap();
    match Db::open("/lockless", opts()) {
        Ok(second) => {
            let _ = second.close();
            let _ = first.close();
            panic!("two read-write handles were opened on one directory");
        }
        Err(e) => {
            let text = format!("{e:?}");
            assert!(
                text.contains("already open"),
                "the refusal must name the cause, got: {text}"
            );
        }
    }

    // Releasing the first handle must release the directory: a registry
    // that leaked would brick every later open, which is exactly the
    // failure mode the deleted LOCK-file proxy had.
    first.close().unwrap();
    drop(first);
    let reopened = Db::open("/lockless", opts()).unwrap();
    regolith::through_stalls(|| reopened.put(b"k", b"v")).unwrap();
    reopened.close().unwrap();
}

/// With the plain writer already parked at the FIFO stop trigger, a commit
/// that writes must observe the same stall the plain write did, and a commit
/// that writes nothing must not wait at all.
fn probe_transactional_stall(
    plain: &Db,
    label: &str,
    begin: impl Fn(IsolationLevel) -> Transaction,
) {
    let reason = first_stall_reason(plain, label);
    assert!(
        reason.contains("FIFO compaction never merges them"),
        "{label}: expected the FIFO stop reason, got: {reason}"
    );

    for iso in [
        IsolationLevel::SnapshotIsolation,
        IsolationLevel::Serializable,
    ] {
        let tx = begin(iso);
        tx.get(b"k00000000").unwrap();
        let read_only = tx.commit();
        assert!(
            read_only.is_ok(),
            "{label}/{iso:?}: a read-only commit must not wait on a stall it never touches, \
             got {read_only:?}"
        );

        let key = format!("{label}/txn/{iso:?}").into_bytes();
        let tx = begin(iso);
        tx.put(&key, &val(1)).unwrap();
        match tx.commit() {
            Err(TransactionError::Engine(Error::Busy(got))) => {
                assert_eq!(
                    got, reason,
                    "{label}/{iso:?}: expected the plain write's stall reason"
                );
                assert!(
                    plain.get(&key).unwrap().is_none(),
                    "{label}/{iso:?}: a commit refused for a stall must not have written its key"
                );
            }
            other => panic!("{label}/{iso:?}: expected the plain write's stall, got {other:?}"),
        }
    }
}

#[test]
fn optimistic_commit_observes_the_stop_trigger_like_a_plain_write() {
    with_deadline("optimistic_txn_stall", 180, || {
        let dir = tempfile::tempdir().unwrap();
        let db = OptimisticTransactionDb::open(dir.path(), fifo_opts(0)).unwrap();
        probe_transactional_stall(db.db(), "optimistic", |iso| {
            db.begin(&TxnOptions::new().isolation(iso))
        });
        db.db().close().unwrap();
    });
}

#[test]
fn pessimistic_commit_observes_the_stop_trigger_like_a_plain_write() {
    with_deadline("pessimistic_txn_stall", 180, || {
        let dir = tempfile::tempdir().unwrap();
        let db = TransactionDb::open(dir.path(), fifo_opts(0)).unwrap();
        probe_transactional_stall(db.db(), "pessimistic", |iso| {
            db.begin(&TxnOptions::new().isolation(iso))
        });
        db.db().close().unwrap();
    });
}

/// An oversized transactional commit must be rejected the same way a plain
/// put is, a permanent size error, even with the plain writer already
/// parked at the FIFO stop trigger. Regression for admission ordering: when
/// sizes were checked after the write-stall wait, a commit that could never
/// pass validation got stalled first, and its size error came back looking
/// like a transient "engine busy" instead.
fn probe_oversized_commit_under_a_stop(
    plain: &Db,
    label: &str,
    begin: impl FnOnce() -> Transaction,
) {
    // Park the plain writer at the stop before touching the size limit, so
    // a broken admission order would see the stall ahead of the size check.
    let _ = first_stall_reason(plain, label);

    let oversized = vec![0u8; regolith::DEFAULT_MAX_VALUE_SIZE + 1];
    let message = match plain.put(b"oversized", &oversized) {
        Err(Error::InvalidArgument(message)) => message,
        other => panic!("{label}: expected an oversized-value InvalidArgument, got {other:?}"),
    };

    let tx = begin();
    tx.put(b"oversized", &oversized).unwrap();
    match tx.commit() {
        Err(TransactionError::Engine(Error::InvalidArgument(got))) => {
            assert_eq!(
                got, message,
                "{label}: expected the plain write's size message"
            );
        }
        other => panic!("{label}: expected a size InvalidArgument, got {other:?}"),
    }
}

#[test]
fn optimistic_commit_of_an_oversized_value_reports_the_size_error_not_a_stall() {
    with_deadline("optimistic_oversized_commit", 180, || {
        let dir = tempfile::tempdir().unwrap();
        let db = OptimisticTransactionDb::open(dir.path(), fifo_opts(0)).unwrap();
        probe_oversized_commit_under_a_stop(db.db(), "optimistic", || db.begin(&TxnOptions::new()));
        db.db().close().unwrap();
    });
}

#[test]
fn pessimistic_commit_of_an_oversized_value_reports_the_size_error_not_a_stall() {
    with_deadline("pessimistic_oversized_commit", 180, || {
        let dir = tempfile::tempdir().unwrap();
        let db = TransactionDb::open(dir.path(), fifo_opts(0)).unwrap();
        probe_oversized_commit_under_a_stop(db.db(), "pessimistic", || {
            db.begin(&TxnOptions::new())
        });
        db.db().close().unwrap();
    });
}

#[derive(Debug)]
struct ConcatMerge;

impl MergeOperator for ConcatMerge {
    fn name(&self) -> &'static str {
        "concat"
    }

    fn full_merge(
        &self,
        _key: &[u8],
        existing: Option<&[u8]>,
        operands: &[&[u8]],
    ) -> Option<Vec<u8>> {
        let mut out = existing.map(|e| e.to_vec()).unwrap_or_default();
        for op in operands {
            out.extend_from_slice(op);
        }
        Some(out)
    }
}

fn fifo_merge_opts(workers: usize) -> Options {
    fifo_opts(workers).merge_operator(Some(std::sync::Arc::new(ConcatMerge)))
}

/// A commit whose only buffered op is a merge must be admitted the same way
/// a put-carrying commit is. Regression for a `carries_writes` guard that
/// checked `writes` and `range_deletes` but dropped the `merges` arm, which
/// would let a merge-only commit skip admission and land straight through
/// an L0 stop.
fn probe_merge_only_commit_under_a_stop(
    plain: &Db,
    label: &str,
    begin: impl FnOnce() -> Transaction,
) {
    let reason = first_stall_reason(plain, label);

    let tx = begin();
    tx.merge(b"counter", b"x").unwrap();
    match tx.commit() {
        Err(TransactionError::Engine(Error::Busy(got))) => {
            assert_eq!(
                got, reason,
                "{label}: expected the plain write's stall reason"
            );
        }
        other => panic!("{label}: expected the plain write's stall, got {other:?}"),
    }
    assert!(
        plain.get(b"counter").unwrap().is_none(),
        "{label}: a merge-only commit refused for a stall must not have written its key"
    );
}

#[test]
fn optimistic_merge_only_commit_observes_the_stop_trigger_like_a_plain_write() {
    with_deadline("optimistic_merge_stall", 180, || {
        let dir = tempfile::tempdir().unwrap();
        let db = OptimisticTransactionDb::open(dir.path(), fifo_merge_opts(0)).unwrap();
        probe_merge_only_commit_under_a_stop(db.db(), "optimistic", || {
            db.begin(&TxnOptions::new())
        });
        db.db().close().unwrap();
    });
}

#[test]
fn pessimistic_merge_only_commit_observes_the_stop_trigger_like_a_plain_write() {
    with_deadline("pessimistic_merge_stall", 180, || {
        let dir = tempfile::tempdir().unwrap();
        let db = TransactionDb::open(dir.path(), fifo_merge_opts(0)).unwrap();
        probe_merge_only_commit_under_a_stop(db.db(), "pessimistic", || {
            db.begin(&TxnOptions::new())
        });
        db.db().close().unwrap();
    });
}
