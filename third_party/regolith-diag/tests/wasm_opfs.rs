#![cfg(all(target_arch = "wasm32", target_os = "unknown"))]

//! OPFS backend tests that need a Web Worker.
//!
//! `createSyncAccessHandle` exists only in a worker scope, so
//! [`OpfsMode::Sah`] can only be exercised here. Mirror mode is available
//! in a worker too, so both strategies are covered from one runner; the
//! main-thread half of the contract lives in `tests/wasm_opfs_main.rs`.
//!
//! Run against a real browser (`geckodriver` or `chromedriver` on PATH,
//! headless unless `NO_HEADLESS=1`):
//!
//! ```text
//! CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner \
//!   cargo test --target wasm32-unknown-unknown --test wasm_opfs
//! ```
//!
//! `wasm-pack test` cannot be used here: it appends `--tests` to the
//! cargo invocation, which builds every test target in the package, and
//! the rest of `tests/` is native-only.

use std::path::Path;

use regolith::env::opfs::{OpfsEnv, OpfsMode, OpfsOptions};
use regolith::env::{Env, WriteMode};
use regolith::{CompactionOutcome, Db, Options, WriteBatch};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_dedicated_worker);

/// Options for a database that fits a browser tab and runs compaction on
/// the calling thread, which is the only thread this target has.
fn db_options(env: &OpfsEnv) -> Options {
    Options::embedded().env(env.as_env())
}

/// The shipped wasm profile, pointed at OPFS. Unlike [`db_options`]
/// this keeps a block cache, which is the whole reason the profile is
/// separate from `Options::embedded`: a browser tab has no OS page
/// cache to absorb a miss.
fn wasm_profile_options(env: &OpfsEnv) -> Options {
    Options::wasm().env(env.as_env())
}

#[wasm_bindgen_test]
async fn a_worker_mount_selects_sync_access_handles() {
    let env = OpfsEnv::mount("regolith-test-probe", OpfsOptions::default())
        .await
        .expect("mount");
    assert_eq!(env.mode(), OpfsMode::Sah);
    assert_eq!(env.pending_bytes(), 0);
    assert_eq!(env.resident_bytes(), 0);
}

#[wasm_bindgen_test]
async fn sync_access_handles_report_real_durability() {
    let env = OpfsEnv::mount("regolith-test-caps", OpfsOptions::default())
        .await
        .expect("mount");
    let caps = env.capabilities();
    assert!(caps.durable_sync, "a sync access handle flush is durable");
    assert!(caps.atomic_rename, "rename settles by slot generation");
    assert!(caps.sync_dir, "a name binding lives in the slot header");
    assert!(!caps.hard_link, "OPFS has no links");
    assert!(!caps.threads, "this target has one thread");
    assert!(!caps.file_lock, "there is no second process to exclude");
}

#[wasm_bindgen_test]
async fn the_full_lifecycle_survives_a_remount() {
    let name = "regolith-test-lifecycle";
    {
        let env = OpfsEnv::mount(name, OpfsOptions::default())
            .await
            .expect("mount");
        assert_eq!(env.mode(), OpfsMode::Sah);
        let db = Db::open(env.db_path(), db_options(&env)).expect("open");

        db.put(b"alpha", b"one").expect("put");
        db.put(b"bravo", b"two").expect("put");
        db.put(b"charlie", b"three").expect("put");
        assert_eq!(db.get(b"alpha").expect("get").as_deref(), Some(&b"one"[..]));

        db.delete(b"bravo").expect("delete");
        assert_eq!(db.get(b"bravo").expect("get"), None);

        let snapshot = db.snapshot();
        db.put(b"alpha", b"rewritten").expect("put");
        assert_eq!(
            snapshot.get(b"alpha").expect("snapshot get").as_deref(),
            Some(&b"one"[..]),
            "a snapshot must not see a later write"
        );
        drop(snapshot);

        let mut batch = WriteBatch::new();
        batch.put(b"delta", b"four");
        batch.put(b"echo", b"five");
        db.write(batch).expect("batch");

        let scanned = db.scan(None, None).expect("scan");
        let keys: Vec<&[u8]> = scanned.iter().map(|(k, _)| k.as_slice()).collect();
        assert_eq!(
            keys,
            vec![&b"alpha"[..], &b"charlie"[..], &b"delta"[..], &b"echo"[..]]
        );

        let mut cursor = db.iter();
        cursor.seek(b"delta");
        assert!(cursor.valid());
        assert_eq!(cursor.key(), Some(&b"delta"[..]));
        cursor.next();
        assert_eq!(cursor.key(), Some(&b"echo"[..]));
        drop(cursor);

        db.flush().expect("flush");
        db.compact_range(None, None).wait().expect("compact");
        while db.compact_step().expect("compact step") == CompactionOutcome::DidWork {}
        db.close().expect("close");
    }

    let env = OpfsEnv::mount(name, OpfsOptions::default())
        .await
        .expect("remount");
    let db = Db::open(env.db_path(), db_options(&env)).expect("reopen");
    assert_eq!(
        db.get(b"alpha").expect("get").as_deref(),
        Some(&b"rewritten"[..]),
        "the last write must survive a remount"
    );
    assert_eq!(
        db.get(b"bravo").expect("get"),
        None,
        "a delete must survive"
    );
    assert_eq!(db.get(b"echo").expect("get").as_deref(), Some(&b"five"[..]));
    db.close().expect("close");
}

#[wasm_bindgen_test]
async fn mirror_mode_needs_persist_to_survive() {
    let name = "regolith-test-mirror";
    let options = OpfsOptions {
        force_mode: Some(OpfsMode::Mirror),
        ..OpfsOptions::default()
    };
    {
        let env = OpfsEnv::mount(name, options).await.expect("mount");
        assert_eq!(env.mode(), OpfsMode::Mirror);
        assert!(
            !env.capabilities().durable_sync,
            "mirror mode is durable only across persist"
        );

        let db = Db::open(env.db_path(), db_options(&env)).expect("open");
        db.put(b"key", b"value").expect("put");
        db.flush().expect("flush");
        db.close().expect("close");

        assert!(
            env.pending_bytes() > 0,
            "close alone cannot await a persist"
        );
        env.persist().await.expect("persist");
        assert_eq!(env.pending_bytes(), 0);
        assert!(env.resident_bytes() > 0);
    }

    let env = OpfsEnv::mount(name, options).await.expect("remount");
    let db = Db::open(env.db_path(), db_options(&env)).expect("reopen");
    assert_eq!(
        db.get(b"key").expect("get").as_deref(),
        Some(&b"value"[..]),
        "persisted bytes must come back"
    );
    db.close().expect("close");
}

#[wasm_bindgen_test]
async fn an_exhausted_pool_is_reported_and_grow_pool_clears_it() {
    let options = OpfsOptions {
        initial_slots: 3,
        ..OpfsOptions::default()
    };
    let env = OpfsEnv::mount("regolith-test-pool", options)
        .await
        .expect("mount");
    env.create_dir_all(Path::new("regolith-test-pool/d"))
        .expect("mkdir");

    let mut opened = 0usize;
    loop {
        let path = format!("regolith-test-pool/d/f{opened}");
        match env.open_write(Path::new(&path), WriteMode::Truncate) {
            Ok(_) => opened += 1,
            Err(e) => {
                assert!(
                    e.to_string().contains("handle pool is full"),
                    "the error must name the pool, got: {e}"
                );
                break;
            }
        }
        assert!(opened <= 3, "a 3 slot pool must not open a fourth file");
    }
    assert_eq!(opened, 3);
    assert_eq!(env.free_slots(), 0);

    env.grow_pool(2).await.expect("grow");
    assert_eq!(env.free_slots(), 2);
    env.open_write(Path::new("regolith-test-pool/d/after"), WriteMode::Truncate)
        .expect("a grown pool must accept a new file");
}

#[wasm_bindgen_test]
async fn a_rename_keeps_the_newer_slot_across_a_remount() {
    let name = "regolith-test-rename";
    let manifest = format!("{name}/MANIFEST");
    let staged = format!("{name}/MANIFEST.tmp");
    {
        let env = OpfsEnv::mount(name, OpfsOptions::default())
            .await
            .expect("mount");
        env.create_dir_all(Path::new(name)).expect("mkdir");

        let mut old = env
            .open_write(Path::new(&manifest), WriteMode::Truncate)
            .expect("open manifest");
        old.write_all(b"stale").expect("write");
        old.sync_all().expect("sync");
        drop(old);

        let mut new = env
            .open_write(Path::new(&staged), WriteMode::Truncate)
            .expect("open staged");
        new.write_all(b"fresh").expect("write");
        new.sync_all().expect("sync");
        drop(new);

        env.rename(Path::new(&staged), Path::new(&manifest))
            .expect("rename");
        assert!(!env.exists(Path::new(&staged)));
        assert_eq!(env.read(Path::new(&manifest)).expect("read"), b"fresh");
    }

    let env = OpfsEnv::mount(name, OpfsOptions::default())
        .await
        .expect("remount");
    assert_eq!(
        env.read(Path::new(&manifest)).expect("read"),
        b"fresh",
        "the higher generation must win at mount"
    );
    assert!(
        !env.exists(Path::new(&staged)),
        "the released slot must not resurrect the old name"
    );
}

#[wasm_bindgen_test]
async fn directories_list_their_entries_after_a_remount() {
    let name = "regolith-test-readdir";
    {
        let env = OpfsEnv::mount(name, OpfsOptions::default())
            .await
            .expect("mount");
        let dir = format!("{name}/sst");
        env.create_dir_all(Path::new(&dir)).expect("mkdir");
        for id in 0..3u32 {
            let path = format!("{dir}/{id:06}.sst");
            let mut file = env
                .open_write(Path::new(&path), WriteMode::Truncate)
                .expect("open");
            file.write_all(&[id as u8; 16]).expect("write");
            file.sync_all().expect("sync");
        }
    }

    let env = OpfsEnv::mount(name, OpfsOptions::default())
        .await
        .expect("remount");
    let dir = format!("{name}/sst");
    let mut names: Vec<String> = env
        .read_dir(Path::new(&dir))
        .expect("read_dir")
        .map(|entry| entry.expect("walk").file_name())
        .collect();
    names.sort();
    assert_eq!(names, vec!["000000.sst", "000001.sst", "000002.sst"]);
    assert_eq!(
        env.metadata(Path::new(&format!("{dir}/000001.sst")))
            .expect("metadata")
            .len,
        16
    );
}

#[wasm_bindgen_test]
async fn mounting_with_fewer_slots_never_orphans_a_file() {
    let name = "regolith-test-shrink";
    {
        let env = OpfsEnv::mount(
            name,
            OpfsOptions {
                initial_slots: 8,
                ..OpfsOptions::default()
            },
        )
        .await
        .expect("mount");
        env.create_dir_all(Path::new(name)).expect("mkdir");
        for id in 0..8u8 {
            let path = format!("{name}/f{id}");
            let mut file = env
                .open_write(Path::new(&path), WriteMode::Truncate)
                .expect("open");
            file.write_all(&[id; 4]).expect("write");
            file.sync_all().expect("sync");
        }
        assert_eq!(env.free_slots(), 0);
    }

    let env = OpfsEnv::mount(
        name,
        OpfsOptions {
            initial_slots: 2,
            ..OpfsOptions::default()
        },
    )
    .await
    .expect("remount with a smaller pool");
    for id in 0..8u8 {
        let path = format!("{name}/f{id}");
        assert_eq!(
            env.read(Path::new(&path)).expect("read"),
            vec![id; 4],
            "a file above the requested slot count must still be reachable"
        );
    }
    assert_eq!(env.free_slots(), 0, "the pool grew back to what was stored");
}

#[wasm_bindgen_test]
async fn bytes_written_through_a_slot_survive_a_remount() {
    let name = "regolith-test-durable";
    let file = format!("{name}/wal/000001.wal");
    {
        let env = OpfsEnv::mount(name, OpfsOptions::default())
            .await
            .expect("mount");
        env.create_dir_all(Path::new(&format!("{name}/wal")))
            .expect("mkdir");
        let mut handle = env
            .open_write(Path::new(&file), WriteMode::Truncate)
            .expect("open");
        handle.write_all(b"hello ").expect("write");
        handle.write_all(b"world").expect("write");
        handle.sync_all().expect("sync");
        assert_eq!(handle.len().expect("len"), 11);
    }
    let env = OpfsEnv::mount(name, OpfsOptions::default())
        .await
        .expect("remount");
    assert_eq!(env.read(Path::new(&file)).expect("read"), b"hello world");
    assert_eq!(env.metadata(Path::new(&file)).expect("meta").len, 11);
}

#[wasm_bindgen_test]
async fn append_reopens_at_the_end_and_truncate_starts_over() {
    let name = "regolith-test-modes";
    let file = format!("{name}/MANIFEST");
    let env = OpfsEnv::mount(name, OpfsOptions::default())
        .await
        .expect("mount");
    env.create_dir_all(Path::new(name)).expect("mkdir");

    let mut w = env
        .open_write(Path::new(&file), WriteMode::Truncate)
        .expect("open");
    w.write_all(b"aaaa").expect("write");
    w.sync_all().expect("sync");
    drop(w);

    let mut w = env
        .open_write(Path::new(&file), WriteMode::Append)
        .expect("reopen");
    w.write_all(b"bbbb").expect("write");
    w.sync_all().expect("sync");
    drop(w);
    assert_eq!(env.read(Path::new(&file)).expect("read"), b"aaaabbbb");

    let mut w = env
        .open_write(Path::new(&file), WriteMode::Truncate)
        .expect("reopen");
    w.write_all(b"c").expect("write");
    w.sync_all().expect("sync");
    drop(w);
    assert_eq!(env.read(Path::new(&file)).expect("read"), b"c");
}

#[wasm_bindgen_test]
async fn positional_reads_and_set_len_behave() {
    let name = "regolith-test-positional";
    let file = format!("{name}/sst/000001.sst");
    let env = OpfsEnv::mount(name, OpfsOptions::default())
        .await
        .expect("mount");
    env.create_dir_all(Path::new(&format!("{name}/sst")))
        .expect("mkdir");
    let mut w = env
        .open_write(Path::new(&file), WriteMode::Truncate)
        .expect("open");
    w.write_all(&(0u8..64).collect::<Vec<u8>>()).expect("write");
    w.sync_all().expect("sync");
    drop(w);

    let r = env.open_read(Path::new(&file)).expect("open read");
    assert_eq!(r.len().expect("len"), 64);
    let mut buf = [0u8; 8];
    r.read_exact_at(56, &mut buf).expect("tail read");
    assert_eq!(buf, [56, 57, 58, 59, 60, 61, 62, 63]);
    let mut over = [0u8; 8];
    assert!(
        r.read_exact_at(60, &mut over).is_err(),
        "reading past the end must fail"
    );
    drop(r);

    let mut w = env
        .open_write(Path::new(&file), WriteMode::Append)
        .expect("reopen");
    w.set_len(16).expect("truncate");
    w.sync_all().expect("sync");
    drop(w);
    assert_eq!(env.metadata(Path::new(&file)).expect("meta").len, 16);
}

#[wasm_bindgen_test]
async fn removing_a_file_frees_its_slot_permanently() {
    let name = "regolith-test-remove";
    let file = format!("{name}/wal/000001.wal");
    {
        let env = OpfsEnv::mount(name, OpfsOptions::default())
            .await
            .expect("mount");
        env.create_dir_all(Path::new(&format!("{name}/wal")))
            .expect("mkdir");
        let mut f = env
            .open_write(Path::new(&file), WriteMode::Truncate)
            .expect("open");
        f.write_all(b"transient").expect("write");
        f.sync_all().expect("sync");
        drop(f);
        let before = env.free_slots();
        env.remove_file(Path::new(&file)).expect("remove");
        assert_eq!(env.free_slots(), before + 1);
        assert!(!env.exists(Path::new(&file)));
    }
    let env = OpfsEnv::mount(name, OpfsOptions::default())
        .await
        .expect("remount");
    assert!(
        !env.exists(Path::new(&file)),
        "a removed file must not come back"
    );
}

#[wasm_bindgen_test]
async fn a_stale_handle_reports_instead_of_corrupting_another_file() {
    let name = "regolith-test-stale";
    let file = format!("{name}/victim");
    let env = OpfsEnv::mount(name, OpfsOptions::default())
        .await
        .expect("mount");
    env.create_dir_all(Path::new(name)).expect("mkdir");
    let mut handle = env
        .open_write(Path::new(&file), WriteMode::Truncate)
        .expect("open");
    handle.write_all(b"before").expect("write");
    handle.sync_all().expect("sync");

    env.remove_file(Path::new(&file)).expect("remove");
    let err = handle
        .write_all(b"after")
        .expect_err("a stale handle must fail");
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
}

#[wasm_bindgen_test]
async fn the_wasm_profile_survives_a_full_lifecycle_in_a_browser() {
    let name = "regolith-test-wasm-profile";
    {
        let env = OpfsEnv::mount(name, OpfsOptions::default())
            .await
            .expect("mount");
        let options = wasm_profile_options(&env);
        assert!(
            options.get_block_cache_size() > 0,
            "the wasm profile must keep a block cache; there is no page cache behind it"
        );
        let db = Db::open(env.db_path(), options).expect("open with Options::wasm()");

        // Enough distinct keys to fill blocks and make the cache do
        // something, then read them back through it.
        for i in 0..2000u32 {
            db.put(format!("key-{i:06}").as_bytes(), &[b'v'; 128])
                .expect("put");
        }
        db.flush().expect("flush");
        db.compact_range(None, None).wait().expect("compact");
        while db.compact_step().expect("compact step") == CompactionOutcome::DidWork {}

        for i in (0..2000u32).step_by(97) {
            assert_eq!(
                db.get(format!("key-{i:06}").as_bytes())
                    .expect("get")
                    .as_deref(),
                Some(&[b'v'; 128][..]),
                "key-{i:06} must read back through the block cache"
            );
        }
        db.close().expect("close");
    }

    let env = OpfsEnv::mount(name, OpfsOptions::default())
        .await
        .expect("remount");
    let db = Db::open(env.db_path(), wasm_profile_options(&env)).expect("reopen");
    assert_eq!(
        db.get(b"key-001000").expect("get").as_deref(),
        Some(&[b'v'; 128][..]),
        "the wasm profile must survive a close and remount"
    );
    db.close().expect("close");
}

#[wasm_bindgen_test]
async fn a_background_compaction_worker_is_rejected_at_the_option() {
    // wasm has no threads, so a worker count must fail at `validate`
    // with the option named, not as an io error from inside `open`.
    // This is the wasm-side half of the check; the library's own
    // `#[cfg(test)]` tests cannot cover it because they only ever run
    // on the host.
    let env = OpfsEnv::mount("regolith-test-wasm-workers", OpfsOptions::default())
        .await
        .expect("mount");

    let options = wasm_profile_options(&env).max_background_compactions(1);
    let message = options
        .validate()
        .expect_err("wasm has no threads, so a worker count must not validate")
        .to_string();
    assert!(
        message.contains("max_background_compactions"),
        "the error must name the option that caused it, got: {message}"
    );

    // And the rejection must reach `Db::open`, not just `validate`.
    let options = wasm_profile_options(&env).max_background_compactions(1);
    Db::open(env.db_path(), options).expect_err("open must refuse a worker count on wasm");

    // Both shipped profiles must be openable as they ship.
    assert_eq!(Options::wasm().get_max_background_compactions(), 0);
    assert_eq!(Options::default().get_max_background_compactions(), 0);
    Options::default()
        .validate()
        .expect("Options::default must be valid on wasm, or nothing opens without tuning");
}
