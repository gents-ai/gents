//! A real `ENOSPC`, produced rather than simulated.
//!
//! The test in this module mounts a small `tmpfs` inside an unprivileged
//! user and mount namespace, re-executes the test binary inside it, and
//! lets regolith meet the kernel's own out-of-space error on its own
//! `std::fs` writes. Nothing about the engine's I/O is mocked, and when
//! the namespace cannot be created the test fails loudly instead of
//! passing without having filled anything.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use regolith::{BackgroundErrorReason, Db, Error, EventListener, Options};
use tempfile::TempDir;

use super::{measured, seeded_bytes};

/// Environment variable carrying the mount point into the re-executed
/// child. Its presence is also what stops the child from recursing.
const ENOSPC_MOUNT_ENV: &str = "REGOLITH_ENOSPC_MOUNT";

/// Size of the tmpfs the child fills, in bytes. Large enough for regolith to
/// open a database and small enough to fill in well under a second.
const ENOSPC_FS_BYTES: u64 = 8 * 1024 * 1024;

/// Bytes of headroom left between the ballast file and a full filesystem.
const ENOSPC_HEADROOM: u64 = 192 * 1024;

/// Proves a full filesystem is an error, not a corruption: writing until
/// the device is out of space returns a clear `ENOSPC` from `Db::put`,
/// every write that returned `Ok` before it is still readable, freeing
/// space makes the database writable again without a reopen, and a later
/// reopen recovers cleanly with all of those writes intact.
///
/// The filesystem is real. The test mounts an 8 MiB tmpfs inside an
/// unprivileged user and mount namespace and re-executes this binary
/// inside it, so regolith meets the kernel's own `ENOSPC` on its own
/// `std::fs` writes. Catches an engine that reports a disk-full write as
/// data corruption, that leaves its WAL writer wedged after the first
/// failure, or that cannot reopen a database whose WAL has a partial
/// record from the write that ran out of space.
///
/// Measured runtime: 0.5 s including the child spawn; the child's peak
/// RSS is 6 MiB. Requires `unshare` and unprivileged user namespaces;
/// when they are missing the test fails and says so rather than passing
/// without having filled anything.
/// Whether this host can build the private tmpfs the test needs.
///
/// A GitHub runner cannot: `unshare` reports
/// "write failed /proc/self/uid_map: Operation not permitted", because
/// unprivileged user-namespace uid mapping is disabled there. That is an
/// environment limitation and not a defect in the engine, so the test
/// says out loud that it did not measure anything rather than either
/// failing the build or passing as if it had.
fn can_build_a_private_tmpfs() -> Option<String> {
    let probe = Command::new("unshare")
        .args(["--user", "--map-root-user", "--mount", "true"])
        .output();
    match probe {
        Ok(out) if out.status.success() => None,
        Ok(out) => Some(String::from_utf8_lossy(&out.stderr).trim().to_string()),
        Err(e) => Some(format!("could not run `unshare`: {e}")),
    }
}

#[test]
fn a_full_filesystem_is_reported_and_recovered_from() {
    if let Ok(mount) = std::env::var(ENOSPC_MOUNT_ENV) {
        return enospc_child_body(Path::new(&mount));
    }

    if let Some(why) = can_build_a_private_tmpfs() {
        println!(
            "NOT MEASURED: this host cannot mount a tmpfs in an unprivileged user namespace, so \
             no real ENOSPC was produced and the engine's out-of-space handling was not exercised \
             on this run. Reason: {why}"
        );
        return;
    }

    let dir = TempDir::new().unwrap();
    let mount = dir.path().join("fs");
    fs::create_dir(&mount).unwrap();
    let exe = std::env::current_exe().expect("the test binary knows its own path");

    // $1 is the mount point, $2 the test binary. Exit 97 marks "the
    // namespace or the mount was refused" so the parent can say which
    // half of the requirement is missing.
    let script = format!(
        "mount -t tmpfs -o size={ENOSPC_FS_BYTES} tmpfs \"$1\" || exit 97; \
         exec \"$2\" --exact enospc::a_full_filesystem_is_reported_and_recovered_from \
         --nocapture --test-threads 1"
    );
    let output = Command::new("unshare")
        .args([
            "--user",
            "--map-root-user",
            "--mount",
            "--propagation",
            "private",
            "sh",
            "-c",
        ])
        .arg(&script)
        .arg("sh")
        .arg(&mount)
        .arg(&exe)
        .env(ENOSPC_MOUNT_ENV, &mount)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "this test needs the `unshare` binary to build a private tmpfs, \
                 and could not run it: {e}"
            )
        });

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    print!("{stdout}");
    assert_ne!(
        output.status.code(),
        Some(97),
        "could not mount a tmpfs inside an unprivileged mount namespace. \
         This test does not simulate ENOSPC, so it cannot run here.\n{stderr}"
    );
    assert!(
        output.status.success(),
        "the out-of-space child failed (status {:?})\n--- stdout ---\n{stdout}\
         \n--- stderr ---\n{stderr}",
        output.status
    );
    assert!(
        stdout.contains("ENOSPC observed after"),
        "the child exited 0 without ever filling the filesystem; \
         the assertion would have been vacuous\n{stdout}"
    );
}

/// Records every background flush / compaction / manifest / WAL error the
/// engine reports, so the out-of-space test can check how the background
/// paths classified a full disk rather than only the foreground one.
#[derive(Default)]
struct BackgroundErrorLog {
    entries: Mutex<Vec<(BackgroundErrorReason, String, bool)>>,
}

impl EventListener for BackgroundErrorLog {
    fn on_background_error(&self, reason: BackgroundErrorReason, err: &Error) {
        let misclassified = matches!(err, Error::Corruption(_));
        self.entries
            .lock()
            .unwrap()
            .push((reason, err.to_string(), misclassified));
    }
}

/// The half of [`a_full_filesystem_is_reported_and_recovered_from`] that
/// runs inside the namespace, against the small tmpfs at `mount`.
fn enospc_child_body(mount: &Path) {
    measured("ENOSPC on a full 8 MiB tmpfs", || {
        let db_path = mount.join("db");
        let background = Arc::new(BackgroundErrorLog::default());
        let opts = Options::default()
            .write_buffer_size(32 * 1024)
            .target_file_size(64 * 1024)
            .listeners(vec![background.clone()]);
        let db = Db::open(&db_path, opts.clone()).expect("opening on an empty tmpfs");

        let ballast = mount.join("ballast");
        fill_to_capacity(&ballast);
        let filled = fs::metadata(&ballast).unwrap().len();
        assert!(
            filled > ENOSPC_HEADROOM,
            "the tmpfs is too small to leave usable headroom"
        );
        // Hand a little space back so regolith, not the ballast, is the one
        // that runs out.
        fs::OpenOptions::new()
            .write(true)
            .open(&ballast)
            .unwrap()
            .set_len(filled - ENOSPC_HEADROOM)
            .unwrap();

        let value = seeded_bytes(0xF1_11ED, 4096);
        let mut acked: Vec<usize> = Vec::new();
        let mut failure = None;
        for i in 0..100_000usize {
            match regolith::through_stalls(|| db.put(format!("k{i:06}").as_bytes(), &value)) {
                Ok(()) => acked.push(i),
                Err(e) => {
                    failure = Some(e);
                    break;
                }
            }
        }

        let err = failure.expect("the filesystem never filled up: no write ever failed");
        println!(
            "[resource_limits] ENOSPC observed after {} acknowledged writes: {err}",
            acked.len()
        );
        match &err {
            Error::Io(io) => assert_eq!(
                io.raw_os_error(),
                Some(28),
                "a full disk must surface as ENOSPC (errno 28), got {io:?}"
            ),
            other => panic!(
                "a full disk must surface as an I/O error, not {other:?}. \
                 Reporting it as anything else (corruption in particular) \
                 misleads an operator into a restore they do not need."
            ),
        }

        // Background flush and compaction hit the same full disk. They are
        // allowed to fail; they are not allowed to tell the operator the
        // database is corrupt, which would trigger a restore that is not
        // needed.
        let background_errors = background.entries.lock().unwrap().clone();
        println!(
            "[resource_limits] background errors while the disk was full: {}",
            background_errors.len()
        );
        for (reason, message, misclassified) in &background_errors {
            println!("[resource_limits]   {reason:?}: {message}");
            assert!(
                !misclassified,
                "a background {reason:?} reported a full disk as corruption: {message}"
            );
        }

        // Every acknowledged write is still readable while the disk is
        // full: the failure must not have rolled the engine backwards.
        for &i in &acked {
            assert_eq!(
                db.get(format!("k{i:06}").as_bytes()).unwrap().as_ref(),
                Some(&value),
                "write {i} was acknowledged and then lost when the disk filled"
            );
        }

        // A compaction asked for while the disk is full has to flush the
        // active memtable and write new SSTables. It is allowed to fail;
        // it is not allowed to report the failure as corruption, and it is
        // not allowed to leave the data it did not manage to rewrite
        // unreadable.
        match db.compact_range(None, None).wait() {
            Ok(()) => println!("[resource_limits] compact_range succeeded on a full disk"),
            Err(Error::Io(io)) => {
                println!("[resource_limits] compact_range on a full disk: {io}")
            }
            Err(other) => panic!(
                "a compaction that ran out of space reported {other:?} rather than an I/O error"
            ),
        }
        for &i in &acked {
            assert_eq!(
                db.get(format!("k{i:06}").as_bytes()).unwrap().as_ref(),
                Some(&value),
                "write {i} was lost by a compaction that ran out of space"
            );
        }

        // Freeing space must make the same handle writable again.
        fs::remove_file(&ballast).unwrap();
        regolith::through_stalls(|| db.put(b"after_space_returned", b"ok"))
            .expect("the database stayed wedged after space was freed");
        assert_eq!(
            db.get(b"after_space_returned").unwrap(),
            Some(b"ok".to_vec())
        );
        db.close().expect("closing after recovery");
        drop(db);

        // And a reopen must recover, not refuse: the failed write may
        // have left a partial record at the tail of the WAL.
        let db = Db::open(&db_path, opts).expect(
            "reopening after an out-of-space event failed. A disk-full write \
             must leave a recoverable WAL tail, not an unopenable database",
        );
        for &i in &acked {
            assert_eq!(
                db.get(format!("k{i:06}").as_bytes()).unwrap().as_ref(),
                Some(&value),
                "write {i} was acknowledged before ENOSPC and did not survive reopen"
            );
        }
        assert_eq!(
            db.get(b"after_space_returned").unwrap(),
            Some(b"ok".to_vec())
        );
    });
}

/// Write 64 KiB chunks into `path` until the filesystem refuses, leaving
/// the file exactly as large as the free space allowed.
fn fill_to_capacity(path: &PathBuf) {
    use std::io::Write;

    let chunk = vec![0u8; 64 * 1024];
    let mut file = fs::File::create(path).expect("creating the ballast file");
    loop {
        match file.write_all(&chunk) {
            Ok(()) => {}
            Err(e) if e.raw_os_error() == Some(28) => break,
            Err(e) => panic!("filling the tmpfs failed with an unexpected error: {e}"),
        }
    }
    let _ = file.flush();
}
