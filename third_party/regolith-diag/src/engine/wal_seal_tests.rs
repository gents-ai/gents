//! Sealed write-ahead logs: the frame, the stamp, and the replay rule
//! (`proofs/tla/WalRecovery.tla`, `Frame = "AEAD"` and RED
//! `StampNotSealed`) on sealed logs.

use super::*;

use crate::engine::recovery_tests::RecordingEnv;
use crate::engine::seal::test_keys::{TestKeys, keyring, wrong_keyring};
use crate::engine::wal_seal::{FLAG_SEALED, SEALED_STAMP_LEN};

const MARKER: &[u8] = b"SEALED-VALUE-MARKER";

fn sealed_wal(dir: &TempDir, ring: &Keyring) -> (Wal, PathBuf) {
    let path = dir.path().join("sealed.wal");
    let wal = Wal::create_in(&crate::env::std_env(), &path, Some(ring)).unwrap();
    (wal, path)
}

/// A sealed log of `n` put groups, synced after each when `sync_each`.
fn sealed_log(dir: &TempDir, ring: &Keyring, n: u64, sync_each: bool) -> (PathBuf, Vec<u64>) {
    let (mut wal, path) = sealed_wal(dir, ring);
    for i in 0..n {
        wal.append_put(format!("k{i}").as_bytes(), MARKER, i + 1)
            .unwrap();
        if sync_each {
            wal.sync_data().unwrap();
        }
    }
    drop(wal);
    let offsets = record_offsets(&fs::read(&path).unwrap());
    (path, offsets)
}

#[test]
fn a_sealed_log_round_trips_every_operation_and_holds_no_plaintext() {
    let dir = TempDir::new().unwrap();
    let ring = keyring(&[7]);
    let (mut wal, path) = sealed_wal(&dir, &ring);
    wal.append_put(b"key1", MARKER, 1).unwrap();
    wal.append_delete(b"key2", 2).unwrap();
    wal.append_merge(b"key3", MARKER, 3).unwrap();
    wal.append_delete_range(b"a", b"b", 4).unwrap();
    let big = vec![0x42u8; 3 * COALESCE_LEN];
    wal.append_put(b"big", &big, 5).unwrap();
    wal.close().unwrap();
    drop(wal);

    let bytes = fs::read(&path).unwrap();
    assert_eq!(&bytes[..4], b"REGO");
    assert_eq!(u16::from_le_bytes([bytes[6], bytes[7]]), FLAG_SEALED);
    assert!(!bytes.windows(MARKER.len()).any(|w| w == MARKER));
    assert!(!bytes.windows(4).any(|w| w == b"key1"));
    assert!(!dir.path().join("sealed.tmp").exists());

    let (entries, tail) = replay_with(&path, WalPosition::Newest, Some(&ring)).unwrap();
    assert!(tail.is_none());
    assert_eq!(entries.len(), 5);
    assert_eq!(
        entries[0],
        WalEntry::Put {
            key: b"key1".to_vec(),
            value: MARKER.to_vec(),
            seq: 1
        }
    );
    assert_eq!(
        entries[2],
        WalEntry::Merge {
            key: b"key3".to_vec(),
            operand: MARKER.to_vec(),
            seq: 3
        }
    );
    assert!(matches!(&entries[4], WalEntry::Put { value, .. } if value == &big));
    let offsets = record_offsets(&bytes);
    assert_eq!(offsets.len(), 7, "five groups and CLOSE");
}

/// The stamp is written to a staging name, synced, renamed into place and
/// the directory synced, all before `create_in` returns and before any
/// record: the order `WalRecovery.tla` needs (RED `StampNotSealed`).
#[test]
fn the_sealed_stamp_is_durable_before_the_log_exists() {
    let dir = TempDir::new().unwrap();
    let wal_dir = dir.path().join("wal");
    fs::create_dir_all(&wal_dir).unwrap();
    let recording = Arc::new(RecordingEnv::default());
    let env: Arc<dyn Env> = recording.clone();
    let ring = keyring(&[1]);
    let path = wal_dir.join("wal_000001.log");
    let mut wal = Wal::create_in(&env, &path, Some(&ring)).unwrap();
    assert_eq!(
        recording.events.lock().unwrap().ops,
        vec![
            format!("write {SEALED_STAMP_LEN}"),
            "sync".to_string(),
            "rename wal_000001.log".to_string(),
            "sync dir".to_string(),
        ]
    );
    assert_eq!(wal.offset(), SEALED_STAMP_LEN as u64);
    wal.append_put(b"k", b"v", 1).unwrap();
    wal.sync_data().unwrap();
    let ops = recording.events.lock().unwrap().ops.clone();
    let record = HEADER_LEN + put_record_len(b"k", b"v") + crate::engine::seal::OVERHEAD;
    assert_eq!(
        ops[4..],
        [format!("write {record}"), "sync".to_string()],
        "the directory was synced at creation, not again"
    );
    assert!(!wal_dir.join("wal_000001.tmp").exists());
}

/// RED `StampNotSealed` (Lean `unsealed_stamp_drops_log`, and
/// `wrong_key_refuses`). Nothing in the log was synced, so no record
/// proves anything and P is 0. Under the wrong key every tag fails; had
/// the stamp not been sealed, replay would have dropped the whole log as a
/// torn tail. The sealed stamp fails first, and the open refuses, naming
/// the file.
#[test]
fn red_stamp_not_sealed_a_wrong_key_refuses_and_never_drops_the_log() {
    let dir = TempDir::new().unwrap();
    let (path, _) = sealed_log(&dir, &keyring(&[1]), 3, false);
    let err = replay_with(&path, WalPosition::Newest, Some(&wrong_keyring(&[1]))).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    let message = err.to_string();
    assert!(message.contains("sealed.wal"), "{message}");
    assert!(
        message.contains("does not verify under key id 1"),
        "{message}"
    );
    // The right key opens it whole.
    let (entries, tail) = replay_with(&path, WalPosition::Newest, Some(&keyring(&[1]))).unwrap();
    assert_eq!(entries.len(), 3);
    assert!(tail.is_none());
}

#[test]
fn an_unknown_key_and_a_missing_keyring_refuse_with_typed_errors() {
    let dir = TempDir::new().unwrap();
    let (path, _) = sealed_log(&dir, &keyring(&[1]), 1, true);
    let err = replay_with(&path, WalPosition::Newest, Some(&keyring(&[2]))).unwrap_err();
    assert!(matches!(
        crate::Error::from(err),
        crate::Error::UnknownKey {
            id: crate::KeyId(1)
        }
    ));
    let err = replay_at(&path, WalPosition::Newest).unwrap_err();
    assert!(matches!(
        crate::Error::from(err),
        crate::Error::KeyProviderRequired
    ));
}

/// A log is sealed under the key current when it is created; a rotation
/// takes effect at the next log, and the old one still replays.
#[test]
fn a_rotation_seals_the_next_log_and_the_old_one_still_replays() {
    let dir = TempDir::new().unwrap();
    let keys = TestKeys::new(&[1, 2]);
    let ring = Keyring::new(keys.clone());
    let (path_one, _) = sealed_log(&dir, &ring, 2, true);
    keys.set_current(2);
    let path_two = dir.path().join("two.wal");
    let mut wal = Wal::create_in(&crate::env::std_env(), &path_two, Some(&ring)).unwrap();
    assert_eq!(wal.seal_key(), Some(crate::KeyId(2)));
    wal.append_put(b"later", b"v", 3).unwrap();
    drop(wal);
    let (one, _) = replay_with(&path_one, WalPosition::Earlier, Some(&ring)).unwrap();
    let (two, _) = replay_with(&path_two, WalPosition::Newest, Some(&ring)).unwrap();
    assert_eq!(one.len() + two.len(), 3);
}

/// A failed tag is an unusable record under the O < P rule: in the newest
/// log's unsynced tail it ends the log and is reported; below a record
/// that proves it synced it refuses; in an earlier log it refuses, naming
/// the file.
#[test]
fn a_failed_tag_is_an_unusable_record_under_the_same_rule() {
    let dir = TempDir::new().unwrap();
    let ring = keyring(&[1]);

    let (path, offsets) = sealed_log(&dir, &ring, 3, false);
    let last = offsets[2] as usize;
    flip_byte(&path, last + HEADER_LEN + 20);
    let (entries, tail) = replay_with(&path, WalPosition::Newest, Some(&ring)).unwrap();
    assert_eq!(keys(&entries), vec![b"k0".to_vec(), b"k1".to_vec()]);
    assert_eq!(tail.unwrap().offset, offsets[2]);

    let dir = TempDir::new().unwrap();
    let (path, offsets) = sealed_log(&dir, &ring, 3, true);
    flip_byte(&path, offsets[0] as usize + HEADER_LEN + 5);
    let err = replay_with(&path, WalPosition::Newest, Some(&ring)).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("already made durable"), "{message}");
    assert!(message.contains(&offsets[0].to_string()), "O: {message}");

    let err = replay_with(&path, WalPosition::Earlier, Some(&ring)).unwrap_err();
    assert!(err.to_string().contains("sealed.wal"), "{err}");
    assert!(err.to_string().contains("newer log"), "{err}");
}

/// The tag binds the payload to its header, its offset and its log: a
/// record of another sealed log under the same key, at the same offset,
/// does not verify here.
#[test]
fn a_record_from_another_sealed_log_does_not_verify_here() {
    let ring = keyring(&[1]);
    let (dir_a, dir_b) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (path_a, offsets) = sealed_log(&dir_a, &ring, 2, false);
    let (path_b, _) = sealed_log(&dir_b, &ring, 2, false);
    let mut a = fs::read(&path_a).unwrap();
    let b = fs::read(&path_b).unwrap();
    // Keep a's header (so the cheap header check passes) and take b's
    // sealed payload for the last record.
    let payload = offsets[1] as usize + HEADER_LEN;
    a[payload..].copy_from_slice(&b[payload..]);
    fs::write(&path_a, &a).unwrap();
    let (entries, tail) = replay_with(&path_a, WalPosition::Newest, Some(&ring)).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(tail.unwrap().offset, offsets[1]);
}

/// The scan past damage finds a sealed record whose tag verifies and that
/// proves the damage synced, and passes over one whose tag fails.
#[test]
fn the_scan_past_damage_reads_sealed_records_by_their_tag() {
    let dir = TempDir::new().unwrap();
    let ring = keyring(&[1]);
    let (path, offsets) = sealed_log(&dir, &ring, 4, true);
    let bytes = fs::read(&path).unwrap();
    let file = crate::env::std_env().open_read(&path).unwrap();
    let nonce = nonce_of(&bytes);
    let sealer = ring.current().unwrap();
    let end = bytes.len() as u64;
    let damaged = offsets[1];
    assert_eq!(
        wal_frame::proof_past(&*file, nonce, damaged + 1, end, damaged, Some(&sealer)).unwrap(),
        Some(offsets[2])
    );
    let wrong = wrong_keyring(&[1]).current().unwrap();
    assert_eq!(
        wal_frame::proof_past(&*file, nonce, damaged + 1, end, damaged, Some(&wrong)).unwrap(),
        None,
        "a record whose tag fails proves nothing"
    );
}

/// Every single-byte flip of a sealed, synced log either refuses or keeps
/// a reported prefix, as for an unsealed one; a flip in the stamp always
/// refuses.
#[test]
fn every_byte_flip_of_a_sealed_log_refuses_or_keeps_a_reported_prefix() {
    let dir = TempDir::new().unwrap();
    let ring = keyring(&[1]);
    let (path, offsets) = sealed_log(&dir, &ring, 4, true);
    let bytes = fs::read(&path).unwrap();
    for at in 0..bytes.len() {
        let mut damaged = bytes.clone();
        damaged[at] ^= 0x01;
        fs::write(&path, &damaged).unwrap();
        let outcome = replay_with(&path, WalPosition::Newest, Some(&ring));
        if at < SEALED_STAMP_LEN {
            assert!(outcome.is_err(), "a flip at stamp byte {at} opened");
            continue;
        }
        let damaged_group = offsets.iter().filter(|&&o| o <= at as u64).count() - 1;
        match outcome {
            Ok((entries, tail)) => {
                assert_eq!(entries.len(), damaged_group, "flip at {at}");
                assert_eq!(tail.unwrap().offset, offsets[damaged_group], "flip at {at}");
                assert_eq!(damaged_group, 3, "only the last group has no later proof");
            }
            Err(_) => assert!(damaged_group < 3, "flip at {at} refused"),
        }
    }
}

/// A sealed stamp shorter than its full length cannot be a crash: the log
/// appears under its name only once the whole stamp is durable.
#[test]
fn a_short_sealed_stamp_is_damage() {
    let dir = TempDir::new().unwrap();
    let ring = keyring(&[1]);
    let (path, _) = sealed_log(&dir, &ring, 1, true);
    let bytes = fs::read(&path).unwrap();
    fs::write(&path, &bytes[..SEALED_STAMP_LEN - 1]).unwrap();
    let err = replay_with(&path, WalPosition::Newest, Some(&ring)).unwrap_err();
    assert!(err.to_string().contains("header is damaged"), "{err}");
}

/// A sealed header's payload check is zero and its length counts the
/// frame; any other shape is not one the writer produced.
#[test]
fn a_sealed_header_has_the_shape_the_writer_gives_it() {
    let ring = keyring(&[1]);
    let sealer = ring.current().unwrap();
    let mut out = Vec::new();
    wal_seal::frame_record(&sealer, KIND_GROUP, b"ops", 0, 9, 56, &mut out).unwrap();
    let header = wal_frame::decode_header(&out, 9, 56, true).unwrap();
    assert_eq!(header.len as usize, 3 + crate::engine::seal::OVERHEAD);
    assert!(
        wal_frame::decode_header(&out, 9, 56, false).is_some(),
        "an unsealed reader sees a well-formed header"
    );
    let mut close = Vec::new();
    wal_seal::frame_record(&sealer, wal_frame::KIND_CLOSE, b"", 56, 9, 56, &mut close).unwrap();
    assert!(wal_frame::decode_header(&close, 9, 56, true).is_some());
    assert!(
        wal_frame::decode_header(&close, 9, 56, false).is_none(),
        "an unsealed CLOSE carries nothing"
    );
    // Re-sign a nonzero payload check: the header verifies its own check
    // but is not a shape a sealed writer produces.
    let mut forged = out[..HEADER_LEN].to_vec();
    forged[13] = 1;
    let check = crate::engine::checksum::wal_group_header(9, 56, forged[..17].try_into().unwrap());
    forged[17..21].copy_from_slice(&check.to_le_bytes());
    assert!(wal_frame::decode_header(&forged, 9, 56, true).is_none());
}
