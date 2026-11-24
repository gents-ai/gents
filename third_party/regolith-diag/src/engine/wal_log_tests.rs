//! A log written by position holds the same records, replays the same way
//! and refuses the same things as one written by appending.
//!
//! Every test runs over each kind of Env a log is written through
//! ([`homes`]): the in-memory one, whose log is this crate's own lock-free
//! code, and the file system.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use proptest::prelude::*;
use tempfile::TempDir;

use super::super::recovery_tests::RecordingEnv;
use super::super::seal::{Keyring, test_keys};
use super::super::wal::{Wal, WalEntry, encode_put_record, fault};
use super::super::wal_frame::{HEADER_LEN, KIND_GROUP};
use super::super::wal_replay::{WalPosition, WalReplayIter};
use super::{Log, Place};
use crate::env::{Env, MemEnv, std_env};

/// Where a test keeps its logs: an Env and a directory of its own in it.
struct Home {
    env: Arc<dyn Env>,
    dir: PathBuf,
    /// The directory on the file system, removed when the test ends.
    _on_disk: Option<TempDir>,
}

/// One home in each kind of Env. Under miri the in-memory one alone: a log
/// on the file system syncs its directory at its first sync, which the
/// interpreter does not do, and the in-memory log is the code miri is run
/// for.
fn homes() -> Vec<Home> {
    // A directory no other test has: the sync fault is armed by directory,
    // for every Env at once.
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = PathBuf::from(format!(
        "wal-log-tests/{}",
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mem: Arc<dyn Env> = Arc::new(MemEnv::new());
    mem.create_dir_all(&dir).unwrap();
    let mut homes = vec![Home {
        env: mem,
        dir,
        _on_disk: None,
    }];
    if !cfg!(miri) {
        let on_disk = TempDir::new().unwrap();
        homes.push(Home {
            env: std_env(),
            dir: on_disk.path().to_path_buf(),
            _on_disk: Some(on_disk),
        });
    }
    homes
}

/// The record of one put at `seq`, placed at `offset` of `log`, as the bytes
/// a writer hands to [`Log::write_record`].
fn put_record(log: &Log, offset: u64, key: &[u8], value: &[u8], seq: u64) -> (Place, Vec<u8>) {
    let mut buf = vec![0u8; log.prefix_len()];
    encode_put_record(&mut buf, key, value, seq);
    let place = log
        .place(offset, buf.len() - log.prefix_len(), log.epoch())
        .unwrap();
    log.finish_record(KIND_GROUP, &place, &mut buf).unwrap();
    (place, buf)
}

fn replay(
    env: &Arc<dyn Env>,
    path: &Path,
    keyring: Option<&Keyring>,
) -> std::io::Result<Vec<WalEntry>> {
    let mut iter = WalReplayIter::open(env, path, WalPosition::Newest, keyring)?;
    let mut entries = Vec::new();
    while let Some(entry) = iter.next_entry()? {
        entries.push(entry);
    }
    Ok(entries)
}

fn put(key: &[u8], value: &[u8], seq: u64) -> WalEntry {
    WalEntry::Put {
        key: key.to_vec(),
        value: value.to_vec(),
        seq,
    }
}

#[test]
fn records_written_out_of_order_replay_in_offset_order() {
    for home in homes() {
        let path = home.dir.join("wal_000001.log");
        let log = Log::create_in(&home.env, &path, None).unwrap();
        let (first, a) = put_record(&log, log.stamp_len(), b"a", b"1", 1);
        let (second, b) = put_record(&log, first.end(), b"b", b"2", 2);
        let (third, c) = put_record(&log, second.end(), b"c", b"3", 3);
        assert!(log.write_record(&third, &c).unwrap());
        assert!(log.write_record(&first, &a).unwrap());
        assert!(log.write_record(&second, &b).unwrap());
        // A helper writing a record its owner already wrote changes nothing.
        assert!(log.write_record(&second, &b).unwrap());
        log.sync_through(third.end()).unwrap();
        assert_eq!(log.durable(), third.end());
        assert_eq!(
            replay(&home.env, &path, None).unwrap(),
            vec![put(b"a", b"1", 1), put(b"b", b"2", 2), put(b"c", b"3", 3)]
        );
    }
}

#[test]
fn a_record_is_stamped_with_what_a_sync_covered_and_never_more_than_its_offset() {
    for home in homes() {
        let log = Log::create_in(&home.env, &home.dir.join("wal_000001.log"), None).unwrap();
        let (first, a) = put_record(&log, log.stamp_len(), b"a", b"1", 1);
        assert_eq!(first.synced_through, 0, "nothing is durable before a sync");
        log.write_record(&first, &a).unwrap();
        log.sync_through(first.end()).unwrap();
        let (second, _) = put_record(&log, first.end(), b"b", b"2", 2);
        assert_eq!(second.synced_through, first.end());
        // Decided before the sync, written after it: it still claims only
        // what was durable when it was decided.
        assert_eq!(first.synced_through, 0);
    }
}

#[test]
fn the_bytes_of_a_record_are_the_same_whoever_frames_it() {
    for home in homes() {
        for keyring in [None, Some(test_keys::keyring(&[1]))] {
            let path = home.dir.join(match keyring {
                None => "wal_000001.log",
                Some(_) => "wal_000002.log",
            });
            let log = Log::create_in(&home.env, &path, keyring.as_deref()).unwrap();
            let mut one = vec![0u8; log.prefix_len()];
            encode_put_record(&mut one, b"key", b"value", 9);
            let mut two = one.clone();
            let place = log
                .place(log.stamp_len(), one.len() - log.prefix_len(), log.epoch())
                .unwrap();
            log.finish_record(KIND_GROUP, &place, &mut one).unwrap();
            log.finish_record(KIND_GROUP, &place, &mut two).unwrap();
            assert_eq!(one, two, "two helpers must write identical bytes");
            assert_eq!(one.len() as u64, place.len);
            log.write_record(&place, &one).unwrap();
            log.sync_through(place.end()).unwrap();
            assert_eq!(
                replay(&home.env, &path, keyring.as_deref()).unwrap(),
                vec![put(b"key", b"value", 9)]
            );
        }
    }
}

#[test]
fn a_sealed_record_holds_no_plaintext_and_opens_only_under_its_key() {
    for home in homes() {
        let path = home.dir.join("wal_000001.log");
        let keyring = test_keys::keyring(&[1]);
        let log = Log::create_in(&home.env, &path, Some(&keyring)).unwrap();
        assert_eq!(log.seal_key(), Some(crate::KeyId(1)));
        let (place, record) = put_record(&log, log.stamp_len(), b"secret-key", b"secret-value", 1);
        assert!(!record.windows(6).any(|w| w == b"secret"));
        log.write_record(&place, &record).unwrap();
        log.sync_through(place.end()).unwrap();
        assert_eq!(
            replay(&home.env, &path, Some(&keyring)).unwrap(),
            vec![put(b"secret-key", b"secret-value", 1)]
        );
        assert!(replay(&home.env, &path, Some(&test_keys::wrong_keyring(&[1]))).is_err());
    }
}

#[test]
fn a_write_decided_before_a_rollback_is_not_made_after_it() {
    for home in homes() {
        let path = home.dir.join("wal_000001.log");
        let log = Log::create_in(&home.env, &path, None).unwrap();
        let (kept, a) = put_record(&log, log.stamp_len(), b"kept", b"1", 1);
        log.write_record(&kept, &a).unwrap();
        let (stale, b) = put_record(&log, kept.end(), b"stale", b"2", 2);
        log.poison(std::io::Error::other("the device said no"));
        assert!(log.is_poisoned());
        assert!(
            log.sync_through(kept.end()).is_err(),
            "a poisoned log is not vouched for"
        );
        let (cause, cut) = log.rollback_to(kept.end(), log.epoch() + 1);
        cut.unwrap();
        assert!(cause.unwrap().to_string().contains("said no"));
        assert!(!log.is_poisoned());
        // The discarded commit's record, held by a thread that was slow.
        assert!(!log.write_record(&stale, &b).unwrap());
        // The offset is taken by the next commit.
        let (next, c) = put_record(&log, kept.end(), b"next", b"3", 2);
        assert!(log.write_record(&next, &c).unwrap());
        assert!(!log.write_record(&stale, &b).unwrap());
        log.sync_through(next.end()).unwrap();
        assert_eq!(
            replay(&home.env, &path, None).unwrap(),
            vec![put(b"kept", b"1", 1), put(b"next", b"3", 2)]
        );
    }
}

/// A rollback by a call that may wait does not cut the file while a write is
/// in flight, and goes on once the last one has left: it is woken by that
/// write, not by looking again.
#[test]
fn a_rollback_waits_for_the_writes_in_flight_and_cuts_once_they_left() {
    for home in homes() {
        let path = home.dir.join("wal_000001.log");
        let log = Log::create_in(&home.env, &path, None).unwrap();
        let (kept, a) = put_record(&log, log.stamp_len(), b"kept", b"1", 1);
        log.write_record(&kept, &a).unwrap();
        let (discarded, b) = put_record(&log, kept.end(), b"discarded", b"2", 2);
        log.write_record(&discarded, &b).unwrap();
        let whole = home.env.metadata(&path).unwrap().len;
        assert_eq!(whole, discarded.end());

        let moved_to = log.epoch() + 1;
        // Two writes of the old epoch stand in the gate.
        let (first, second) = (log.write_in_flight(), log.write_in_flight());
        std::thread::scope(|scope| {
            let (done, rolled_back) = std::sync::mpsc::channel();
            let log = &log;
            scope.spawn(move || {
                let _ = done.send(log.rollback_to(kept.end(), moved_to));
            });
            // The rollback has moved the gate on, so it is past the point
            // where it looks for writes in flight, or about to look.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while log.epoch() != moved_to {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the rollback never began"
                );
                std::thread::yield_now();
            }
            drop(first);
            // One write is still in flight: whatever the rollback's thread
            // has done by now, it has not cut.
            assert_eq!(home.env.metadata(&path).unwrap().len, whole);
            assert!(rolled_back.try_recv().is_err(), "the rollback did not wait");
            drop(second);
            let (cause, cut) = rolled_back
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("the last write to leave wakes the rollback");
            cut.unwrap();
            assert!(cause.is_none());
        });
        assert_eq!(home.env.metadata(&path).unwrap().len, kept.end());
    }
}

#[test]
fn a_failed_sync_does_not_move_the_durable_offset() {
    for home in homes() {
        let path = home.dir.join("wal_000001.log");
        let log = Log::create_in(&home.env, &path, None).unwrap();
        let (place, record) = put_record(&log, log.stamp_len(), b"k", b"v", 1);
        log.write_record(&place, &record).unwrap();
        fault::arm_sync_failure(&home.dir);
        let failed = log.sync_through(place.end());
        fault::disarm_sync_failure(&home.dir);
        assert!(failed.unwrap_err().to_string().contains("injected"));
        assert_eq!(log.durable(), 0);
    }
}

#[test]
fn a_closed_log_replays_whole_and_is_vouched_for_by_close() {
    for home in homes() {
        for keyring in [None, Some(test_keys::keyring(&[1]))] {
            let path = home.dir.join(match keyring {
                None => "wal_000001.log",
                Some(_) => "wal_000002.log",
            });
            let log = Log::create_in(&home.env, &path, keyring.as_deref()).unwrap();
            let (place, record) = put_record(&log, log.stamp_len(), b"k", b"v", 1);
            log.write_record(&place, &record).unwrap();
            log.close(place.end()).unwrap();
            assert_eq!(
                replay(&home.env, &path, keyring.as_deref()).unwrap(),
                vec![put(b"k", b"v", 1)]
            );
            // CLOSE proves the whole file durable: damage in it refuses the
            // open instead of being dropped as a tail. The file is rewritten
            // with no handle of the log left on it.
            drop(log);
            let mut bytes = home.env.read(&path).unwrap();
            let at = place.offset as usize + 30;
            bytes[at] ^= 0xFF;
            home.env.write(&path, &bytes).unwrap();
            assert!(replay(&home.env, &path, keyring.as_deref()).is_err());
        }
    }
}

/// A log through the recording Env, in a `wal/` directory of `dir`, and the
/// record of what the host saw, cleared once the log exists.
fn recorded_log(dir: &TempDir) -> (Log, Arc<RecordingEnv>, PathBuf) {
    let wal_dir = dir.path().join("wal");
    std::fs::create_dir_all(&wal_dir).unwrap();
    let recording = Arc::new(RecordingEnv::default());
    let env: Arc<dyn Env> = recording.clone();
    let path = wal_dir.join("wal_000001.log");
    let log = Log::create_in(&env, &path, None).unwrap();
    *recording.events.lock().unwrap() = Default::default();
    (log, recording, path)
}

/// A close that finds no record syncs once, after CLOSE: the one sync
/// covers the stamp and CLOSE together (`WalRecovery.tla`, the torn stamp
/// row), and the log replays as cleanly closed.
// Counts the host's syncs on the file system, which miri cannot sync.
#[cfg_attr(miri, ignore)]
#[test]
fn a_close_with_no_record_syncs_once_after_close() {
    let dir = TempDir::new().unwrap();
    let (log, recording, path) = recorded_log(&dir);
    log.close(log.stamp_len()).unwrap();
    assert_eq!(
        recording.events.lock().unwrap().log,
        vec![format!("write {HEADER_LEN}"), "sync".to_string()],
        "CLOSE, then one sync"
    );
    assert_eq!(log.durable(), log.stamp_len() + HEADER_LEN as u64);
    let env: Arc<dyn Env> = recording.clone();
    assert!(replay(&env, &path, None).unwrap().is_empty());
}

/// RED `CloseWithoutSync`: a record no sync covered is synced before CLOSE
/// is written, so a crash cannot keep CLOSE and lose the record.
// Counts the host's syncs on the file system, which miri cannot sync.
#[cfg_attr(miri, ignore)]
#[test]
fn a_close_after_an_unsynced_record_syncs_it_before_close() {
    let dir = TempDir::new().unwrap();
    let (log, recording, _) = recorded_log(&dir);
    let (place, record) = put_record(&log, log.stamp_len(), b"k", b"v", 1);
    log.write_record(&place, &record).unwrap();
    log.close(place.end()).unwrap();
    assert_eq!(
        recording.events.lock().unwrap().log,
        vec![
            format!("write {}", record.len()),
            "sync".to_string(),
            format!("write {HEADER_LEN}"),
            "sync".to_string()
        ],
        "the record synced, then CLOSE, then a sync"
    );
}

/// A close after a sync that already covered every record skips the
/// first sync too: nothing before CLOSE is left unsynced.
// Counts the host's syncs on the file system, which miri cannot sync.
#[cfg_attr(miri, ignore)]
#[test]
fn a_close_after_every_record_is_synced_syncs_once() {
    let dir = TempDir::new().unwrap();
    let (log, recording, _) = recorded_log(&dir);
    let (place, record) = put_record(&log, log.stamp_len(), b"k", b"v", 1);
    log.write_record(&place, &record).unwrap();
    log.sync_through(place.end()).unwrap();
    log.close(place.end()).unwrap();
    assert_eq!(
        recording.events.lock().unwrap().log,
        vec![
            format!("write {}", record.len()),
            "sync".to_string(),
            format!("write {HEADER_LEN}"),
            "sync".to_string()
        ],
        "the record's own sync, then CLOSE and one sync"
    );
}

/// Each record that passes a multiple of the early writeback's size kicks
/// once, and the lane's call hands over exactly the multiples passed since
/// its last call, never a byte a record has not passed.
// Records the host's writeback calls on the file system, which miri cannot
// reach.
#[cfg_attr(miri, ignore)]
#[test]
fn early_writeback_hands_over_the_multiples_records_passed() {
    let dir = TempDir::new().unwrap();
    let wal_dir = dir.path().join("wal");
    std::fs::create_dir_all(&wal_dir).unwrap();
    let recording = Arc::new(RecordingEnv::default());
    let env: Arc<dyn Env> = recording.clone();
    let kicks = Arc::new(AtomicU64::new(0));
    let counted = Arc::clone(&kicks);
    let early = super::EarlyWriteback::new(
        4096,
        Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }),
    );
    let log = Log::create_in(&env, &wal_dir.join("wal_000001.log"), None)
        .unwrap()
        .with_early_writeback(Some(early));
    let mut at = log.stamp_len();
    let mut write = |len: usize| {
        let (place, record) = put_record(&log, at, b"k", &vec![7u8; len], 1);
        log.write_record(&place, &record).unwrap();
        at = place.end();
    };
    // Below the first multiple: no kick, nothing to hand over.
    write(1000);
    log.write_back_ahead();
    assert_eq!(kicks.load(Ordering::SeqCst), 0);
    // One record passes 4096 and 8192 at once: one kick, two multiples.
    write(9000);
    assert_eq!(kicks.load(Ordering::SeqCst), 1);
    log.write_back_ahead();
    // A record that passes nothing changes nothing.
    write(10);
    log.write_back_ahead();
    // The next one passes 12288.
    write(4000);
    assert_eq!(kicks.load(Ordering::SeqCst), 2);
    log.write_back_ahead();
    let handed: Vec<String> = recording
        .events
        .lock()
        .unwrap()
        .ops
        .iter()
        .filter(|o| o.starts_with("writeback"))
        .cloned()
        .collect();
    assert_eq!(handed, ["writeback 0 8192", "writeback 8192 4096"]);
    assert_eq!(log.handed(), 12288);
}

/// One step of a writer: which record it writes.
fn order(records: usize) -> impl Strategy<Value = Vec<usize>> {
    proptest::collection::vec(0..records, 0..records * 3)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Whatever order the records are written in, and however often, the log
    /// replays exactly as one the same records were appended to.
    #[test]
    fn a_positional_log_replays_as_the_appended_one_does(
        puts in proptest::collection::vec(
            (proptest::collection::vec(any::<u8>(), 1..12), proptest::collection::vec(any::<u8>(), 0..48)),
            1..16,
        ),
        extra in order(16),
        sealed in any::<bool>(),
    ) {
        for home in homes() {
            let env = &home.env;
            let keyring = sealed.then(|| test_keys::keyring(&[1]));

            let appended = home.dir.join("wal_000001.log");
            let mut wal = Wal::create_in(env, &appended, keyring.as_deref()).unwrap();
            for (i, (key, value)) in puts.iter().enumerate() {
                wal.append_put(key, value, i as u64 + 1).unwrap();
            }
            wal.sync_data().unwrap();

            let positional = home.dir.join("wal_000002.log");
            let log = Log::create_in(env, &positional, keyring.as_deref()).unwrap();
            let mut offset = log.stamp_len();
            let mut records = Vec::new();
            for (i, (key, value)) in puts.iter().enumerate() {
                let (place, bytes) = put_record(&log, offset, key, value, i as u64 + 1);
                offset = place.end();
                records.push((place, bytes));
            }
            for pick in &extra {
                if let Some((place, bytes)) = records.get(*pick) {
                    log.write_record(place, bytes).unwrap();
                }
            }
            for (place, bytes) in records.iter().rev() {
                log.write_record(place, bytes).unwrap();
            }
            log.sync_through(offset).unwrap();

            prop_assert_eq!(
                replay(env, &positional, keyring.as_deref()).unwrap(),
                replay(env, &appended, keyring.as_deref()).unwrap()
            );
            prop_assert_eq!(
                env.metadata(&positional).unwrap().len,
                env.metadata(&appended).unwrap().len,
                "the two logs frame every record the same way"
            );
        }
    }
}
