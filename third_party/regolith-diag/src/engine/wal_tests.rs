//! Tests of the format 2 writer and of format 2 replay.
//!
//! The replay rule is the one `proofs/tla/WalRecovery.tla` checks. Each RED
//! configuration there names a defect; the tests named after one build the
//! crash state its counterexample uses and pin the answer the design gives.

use super::*;
use std::fs;
use tempfile::TempDir;

use crate::engine::wal_frame::{self, HEADER_LEN, KIND_GROUP, STAMP_LEN};
use crate::engine::wal_replay::{WalPosition, WalReplayIter};
use crate::engine::wal_seal;

// -- helpers ------------------------------------------------------

fn new_wal(dir: &TempDir) -> (Wal, PathBuf) {
    let path = dir.path().join("test.wal");
    let wal = Wal::create(&path).unwrap();
    (wal, path)
}

fn flip_byte(path: &Path, offset: usize) {
    let mut bytes = fs::read(path).unwrap();
    bytes[offset] ^= 0xFF;
    fs::write(path, &bytes).unwrap();
}

/// Replay `path` at `position`, returning what it yielded and the tail it
/// dropped.
fn replay_at(
    path: &Path,
    position: WalPosition,
) -> io::Result<(Vec<WalEntry>, Option<TailVerdict>)> {
    replay_with(path, position, None)
}

/// [`replay_at`] through `keyring`, for a sealed log.
fn replay_with(
    path: &Path,
    position: WalPosition,
    keyring: Option<&Keyring>,
) -> io::Result<(Vec<WalEntry>, Option<TailVerdict>)> {
    let mut iter = WalReplayIter::open(&crate::env::std_env(), path, position, keyring)?;
    let mut entries = Vec::new();
    while let Some(entry) = iter.next_entry()? {
        entries.push(entry);
    }
    Ok((entries, iter.discarded_tail()))
}

fn keys(entries: &[WalEntry]) -> Vec<Vec<u8>> {
    entries
        .iter()
        .map(|e| match e {
            WalEntry::Put { key, .. } | WalEntry::Delete { key, .. } => key.clone(),
            WalEntry::DeleteRange { start, .. } => start.clone(),
            WalEntry::Merge { key, .. } => key.clone(),
        })
        .collect()
}

fn seq(entry: &WalEntry) -> u64 {
    match entry {
        WalEntry::Put { seq, .. }
        | WalEntry::Delete { seq, .. }
        | WalEntry::DeleteRange { seq, .. }
        | WalEntry::Merge { seq, .. } => *seq,
    }
}

/// Whether a format 2 log's stamp says its records are sealed.
fn is_sealed(bytes: &[u8]) -> bool {
    u16::from_le_bytes([bytes[6], bytes[7]]) == wal_seal::FLAG_SEALED
}

/// Bytes of a format 2 log's stamp, sealed or not.
fn stamp_len_of(bytes: &[u8]) -> usize {
    if is_sealed(bytes) {
        wal_seal::SEALED_STAMP_LEN
    } else {
        STAMP_LEN
    }
}

/// The nonce in a format 2 log's stamp, checked when the stamp is not
/// sealed (a sealed stamp's checks take its key).
fn nonce_of(bytes: &[u8]) -> u64 {
    if is_sealed(bytes) {
        return u64::from_le_bytes(bytes[12..20].try_into().unwrap());
    }
    wal_frame::stamp_nonce(bytes[..STAMP_LEN].try_into().unwrap()).unwrap()
}

/// Every record's offset in an undamaged format 2 log, and the end.
fn record_offsets(bytes: &[u8]) -> Vec<u64> {
    let nonce = nonce_of(bytes);
    let sealed = is_sealed(bytes);
    let mut offsets = vec![stamp_len_of(bytes) as u64];
    loop {
        let at = *offsets.last().unwrap();
        if at as usize == bytes.len() {
            return offsets;
        }
        let header = wal_frame::decode_header(&bytes[at as usize..], nonce, at, sealed)
            .unwrap_or_else(|| panic!("no record at {at}"));
        offsets.push(at + header.record_len());
    }
}

/// A put group's operations.
fn put_entries(key: &[u8], seq: u64) -> Vec<u8> {
    let mut group = Vec::new();
    encode_put_record(&mut group, key, b"value", seq);
    group
}

// -- round trips --------------------------------------------------

#[test]
fn test_wal_write_and_replay() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.wal");

    {
        let mut wal = Wal::create(&path).unwrap();
        wal.append_put(b"key1", b"value1", 1).unwrap();
        wal.append_delete(b"key2", 2).unwrap();
        wal.append_put(b"key3", b"value3", 3).unwrap();
    }

    let entries = Wal::replay(&path).unwrap();
    assert_eq!(
        entries,
        vec![
            WalEntry::Put {
                key: b"key1".to_vec(),
                value: b"value1".to_vec(),
                seq: 1,
            },
            WalEntry::Delete {
                key: b"key2".to_vec(),
                seq: 2,
            },
            WalEntry::Put {
                key: b"key3".to_vec(),
                value: b"value3".to_vec(),
                seq: 3,
            },
        ]
    );
}

#[test]
fn test_wal_filename() {
    assert_eq!(wal_filename(1), "wal_000001.log");
    assert_eq!(wal_filename(42), "wal_000042.log");
}

#[test]
fn every_operation_type_round_trips_in_order() {
    let dir = TempDir::new().unwrap();
    let (mut wal, path) = new_wal(&dir);
    wal.append_put(b"p", b"1", 1).unwrap();
    wal.append_delete(b"d", 2).unwrap();
    wal.append_delete_range(b"ra", b"rb", 3).unwrap();
    wal.append_merge(b"m", b"op", 4).unwrap();
    drop(wal);

    assert_eq!(
        Wal::replay(&path).unwrap(),
        vec![
            WalEntry::Put {
                key: b"p".to_vec(),
                value: b"1".to_vec(),
                seq: 1,
            },
            WalEntry::Delete {
                key: b"d".to_vec(),
                seq: 2,
            },
            WalEntry::DeleteRange {
                start: b"ra".to_vec(),
                end: b"rb".to_vec(),
                seq: 3,
            },
            WalEntry::Merge {
                key: b"m".to_vec(),
                operand: b"op".to_vec(),
                seq: 4,
            },
        ]
    );
}

#[test]
fn a_group_of_several_writes_replays_every_operation_in_order() {
    let dir = TempDir::new().unwrap();
    let (mut wal, path) = new_wal(&dir);
    let ops = vec![
        WriteBatchOp::Put {
            key: b"p".to_vec(),
            value: b"1".to_vec(),
        },
        WriteBatchOp::Delete { key: b"d".to_vec() },
        WriteBatchOp::DeleteRange {
            start: b"ra".to_vec(),
            end: b"rb".to_vec(),
        },
        WriteBatchOp::Merge {
            key: b"m".to_vec(),
            operand: b"op".to_vec(),
        },
    ];
    let mut group = Vec::new();
    encode_put_record(&mut group, b"solo", b"v", 9);
    encode_ops_record(&mut group, &ops, 10);
    wal.append_group(&group).unwrap();
    drop(wal);

    let entries = Wal::replay(&path).unwrap();
    assert_eq!(
        entries.iter().map(seq).collect::<Vec<_>>(),
        vec![9, 10, 11, 12, 13]
    );
    assert_eq!(
        keys(&entries),
        vec![
            b"solo".to_vec(),
            b"p".to_vec(),
            b"d".to_vec(),
            b"ra".to_vec(),
            b"m".to_vec()
        ]
    );
    assert_eq!(
        record_offsets(&fs::read(&path).unwrap()).len(),
        2,
        "a group is one record"
    );
}

#[test]
fn an_empty_group_appends_nothing() {
    let dir = TempDir::new().unwrap();
    let (mut wal, path) = new_wal(&dir);
    let mut group = Vec::new();
    encode_ops_record(&mut group, &[], 1);
    assert!(group.is_empty());
    wal.append_group(&group).unwrap();
    assert_eq!(wal.offset(), STAMP_LEN as u64);
    drop(wal);
    assert!(Wal::replay(&path).unwrap().is_empty());
}

#[test]
fn round_trip_empty_key_and_value_large_value_and_boundary_seqs() {
    let dir = TempDir::new().unwrap();
    let (mut wal, path) = new_wal(&dir);
    let big = vec![0xAB; 1 << 20];
    wal.append_put(b"", b"", 0).unwrap();
    wal.append_put(b"k", &big, 1).unwrap();
    wal.append_put(b"b", b"2", u64::MAX).unwrap();
    drop(wal);

    assert_eq!(
        Wal::replay(&path).unwrap(),
        vec![
            WalEntry::Put {
                key: vec![],
                value: vec![],
                seq: 0,
            },
            WalEntry::Put {
                key: b"k".to_vec(),
                value: big,
                seq: 1,
            },
            WalEntry::Put {
                key: b"b".to_vec(),
                value: b"2".to_vec(),
                seq: u64::MAX,
            },
        ]
    );
}

#[test]
fn replay_many_records() {
    let dir = TempDir::new().unwrap();
    let (mut wal, path) = new_wal(&dir);
    for i in 0..1000u64 {
        wal.append_put(format!("k{i:06}").as_bytes(), b"v", i)
            .unwrap();
    }
    drop(wal);

    let entries = Wal::replay(&path).unwrap();
    assert_eq!(entries.len(), 1000);
    for (i, entry) in entries.iter().enumerate() {
        assert_eq!(
            keys(std::slice::from_ref(entry))[0],
            format!("k{i:06}").as_bytes()
        );
        assert_eq!(seq(entry), i as u64);
    }
}

#[test]
fn replay_of_nonexistent_path_errors() {
    let dir = TempDir::new().unwrap();
    let err = Wal::replay(&dir.path().join("never_created.wal")).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::NotFound);
}

// -- the writer ---------------------------------------------------

#[test]
fn sync_persists_records_across_drop() {
    let dir = TempDir::new().unwrap();
    let (mut wal, path) = new_wal(&dir);
    wal.append_put(b"k", b"v", 1).unwrap();
    wal.sync_data().unwrap();
    drop(wal);
    assert_eq!(Wal::replay(&path).unwrap().len(), 1);
}

#[test]
fn sync_syncs_parent_dir_once() {
    let dir = TempDir::new().unwrap();
    let (mut wal, path) = new_wal(&dir);
    wal.append_put(b"k", b"v", 1).unwrap();

    let mut sync_count = 0;
    wal.sync_with_parent_sync(|sync_path| {
        assert_eq!(sync_path, path.as_path());
        sync_count += 1;
        Ok(())
    })
    .unwrap();
    wal.sync_with_parent_sync(|_| {
        sync_count += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(sync_count, 1);
}

#[test]
fn sync_retries_parent_dir_sync_after_error() {
    let dir = TempDir::new().unwrap();
    let (mut wal, path) = new_wal(&dir);
    wal.append_put(b"k", b"v", 1).unwrap();
    let mut sync_count = 0;

    let err = wal
        .sync_with_parent_sync(|_| {
            sync_count += 1;
            Err(io::Error::other("injected parent sync failure"))
        })
        .unwrap_err();
    assert_eq!(err.to_string(), "injected parent sync failure");
    assert_eq!(
        wal.synced_through, 0,
        "a sync that did not complete proves nothing"
    );

    wal.sync_with_parent_sync(|sync_path| {
        assert_eq!(sync_path, path.as_path());
        sync_count += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(sync_count, 2);
}

#[test]
fn create_truncates_prior_contents() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.wal");
    Wal::create(&path)
        .unwrap()
        .append_put(b"old", b"v", 1)
        .unwrap();
    Wal::create(&path)
        .unwrap()
        .append_put(b"new", b"v", 2)
        .unwrap();
    assert_eq!(keys(&Wal::replay(&path).unwrap()), vec![b"new".to_vec()]);
}

#[test]
fn remove_deletes_underlying_file_and_path_is_the_creation_path() {
    let dir = TempDir::new().unwrap();
    let (wal, path) = new_wal(&dir);
    assert_eq!(wal.path(), path);
    drop(wal);
    Wal::remove(&path).unwrap();
    assert!(!path.exists());
}

#[test]
fn offset_tracks_every_appended_byte() {
    let dir = TempDir::new().unwrap();
    let (mut wal, path) = new_wal(&dir);
    assert_eq!(
        wal.offset(),
        STAMP_LEN as u64,
        "a fresh log holds its stamp"
    );

    wal.append_put(b"k", b"v", 1).unwrap();
    let after_one = wal.offset();
    assert_eq!(
        after_one as usize,
        STAMP_LEN + HEADER_LEN + put_record_len(b"k", b"v")
    );
    wal.append_put(b"k2", b"v2", 2).unwrap();
    assert_eq!(
        wal.offset() as usize,
        after_one as usize + HEADER_LEN + put_record_len(b"k2", b"v2")
    );
    wal.sync_data().unwrap();
    assert_eq!(fs::metadata(&path).unwrap().len(), wal.offset());
}

/// Each record claims exactly what the last completed sync covered, and a
/// sync advances the claim to where it began, never past it
/// (`WalRecovery.tla`, `AppendRecord` and `Sync`).
#[test]
fn every_record_carries_the_offset_the_last_completed_sync_covered() {
    let dir = TempDir::new().unwrap();
    let (mut wal, path) = new_wal(&dir);
    wal.append_put(b"a", b"v", 1).unwrap();
    wal.append_put(b"b", b"v", 2).unwrap();
    wal.sync_data().unwrap();
    let synced = wal.offset();
    wal.append_put(b"c", b"v", 3).unwrap();
    wal.append_put(b"d", b"v", 4).unwrap();
    drop(wal);

    let bytes = fs::read(&path).unwrap();
    let nonce = nonce_of(&bytes);
    let offsets = record_offsets(&bytes);
    let claims: Vec<u64> = offsets[..4]
        .iter()
        .map(|&at| {
            wal_frame::decode_header(&bytes[at as usize..], nonce, at, false)
                .unwrap()
                .synced_through
        })
        .collect();
    assert_eq!(claims, vec![0, 0, synced, synced]);
}

#[test]
fn rollback_discards_a_group_and_leaves_the_log_replayable() {
    let dir = TempDir::new().unwrap();
    let (mut wal, path) = new_wal(&dir);
    wal.append_put(b"keep", b"v", 1).unwrap();
    let good = wal.offset();
    wal.append_put(b"discard", b"v", 2).unwrap();
    assert!(wal.offset() > good);

    wal.rollback_to(good).unwrap();
    assert_eq!(wal.offset(), good);
    // The cursor moved back with the length, so the next append lands at
    // the boundary rather than past a hole.
    wal.append_put(b"after", b"v", 3).unwrap();
    wal.sync_data().unwrap();
    assert_eq!(fs::metadata(&path).unwrap().len(), wal.offset());
    assert_eq!(
        keys(&Wal::replay(&path).unwrap()),
        vec![b"keep".to_vec(), b"after".to_vec()]
    );
}

#[test]
fn a_partly_written_group_never_replays_and_rollback_removes_it() {
    let dir = TempDir::new().unwrap();
    let (mut wal, path) = new_wal(&dir);
    let good = wal.offset();
    let header = wal_frame::encode_header(KIND_GROUP, &put_entries(b"torn", 7), 0, wal.nonce, good);
    let mut partial = header.to_vec();
    partial.extend_from_slice(&put_entries(b"torn", 7)[..10]);
    // What a write cut short leaves: the header and part of the payload.
    wal.file.write_all(&partial).unwrap();
    assert!(Wal::replay(&path).unwrap().is_empty());

    wal.rollback_to(good).unwrap();
    assert_eq!(fs::metadata(&path).unwrap().len(), good);
    assert!(Wal::replay(&path).unwrap().is_empty());
}

#[test]
fn record_len_matches_the_bytes_each_encoder_emits() {
    let mut out = Vec::new();
    encode_put_record(&mut out, b"key", b"value", 4);
    assert_eq!(out.len(), put_record_len(b"key", b"value"));

    let ops = vec![
        WriteBatchOp::Merge {
            key: b"m".to_vec(),
            operand: b"o".to_vec(),
        },
        WriteBatchOp::DeleteRange {
            start: b"s".to_vec(),
            end: b"e".to_vec(),
        },
        WriteBatchOp::Delete { key: b"d".to_vec() },
    ];
    let mut out = Vec::new();
    encode_ops_record(&mut out, &ops, 9);
    assert_eq!(out.len(), ops_record_len(&ops));
    for (op, len) in [
        (&ops[0], merge_record_len(b"m", b"o")),
        (&ops[1], delete_range_record_len(b"s", b"e")),
        (&ops[2], delete_record_len(b"d")),
    ] {
        let mut out = Vec::new();
        encode_op_record(&mut out, op, 1);
        assert_eq!(out.len(), len);
    }
}

#[test]
fn a_record_is_refused_one_byte_past_the_limit_and_accepted_at_it() {
    fn check(accounted: usize, limit: usize) {
        assert!(check_record_len(accounted, limit).is_ok());
        let err = check_record_len(accounted, limit - 1).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            err.to_string(),
            format!(
                "write is too large: it would log {limit} bytes and one write can \
                 log at most {}; split it into smaller writes",
                limit - 1
            )
        );
    }
    let mut out = Vec::new();
    encode_put_record(&mut out, b"key", b"value", 1);
    check(put_record_len(b"key", b"value"), out.len());

    let batch = vec![
        WriteBatchOp::Put {
            key: b"a".to_vec(),
            value: b"1".to_vec(),
        },
        WriteBatchOp::Delete { key: b"b".to_vec() },
    ];
    let mut out = Vec::new();
    encode_ops_record(&mut out, &batch, 1);
    check(ops_record_len(&batch), out.len());

    assert!(check_write_len(MAX_RECORD_LEN as usize).is_ok());
    assert!(check_write_len(MAX_RECORD_LEN as usize + 1).is_err());
}

#[test]
fn record_len_saturates_instead_of_wrapping() {
    let mut len = RecordLen::default();
    len.push(usize::MAX / 2);
    len.push(usize::MAX / 2);
    assert_eq!(len.framed(), usize::MAX);
    assert!(check_record_len(len.framed(), MAX_RECORD_LEN as usize).is_err());
}

// -- the stamp ----------------------------------------------------

#[test]
fn a_fresh_log_begins_with_a_format_2_stamp_and_a_fresh_nonce() {
    let dir = TempDir::new().unwrap();
    let (_wal, path) = new_wal(&dir);
    let (_other, other_path) = {
        let p = dir.path().join("other.wal");
        (Wal::create(&p).unwrap(), p)
    };
    let bytes = fs::read(&path).unwrap();
    assert_eq!(bytes.len(), STAMP_LEN);
    assert_eq!(&bytes[0..4], b"REGO");
    assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), 2);
    assert_eq!(validate_wal_stamp(&bytes).unwrap(), Some(2));
    assert_ne!(
        nonce_of(&bytes),
        nonce_of(&fs::read(&other_path).unwrap()),
        "two logs share a nonce"
    );
}

/// 0.1.x reads the first 12 bytes the way format 1 lays them out, so it
/// sees format 2 named and refuses it as a newer format, never as damage.
#[test]
fn a_format_2_stamp_reads_to_format_1_code_as_a_newer_format() {
    let stamp = wal_frame::encode_stamp(7);
    let format = u16::from_le_bytes([stamp[4], stamp[5]]);
    let reserved = u16::from_le_bytes([stamp[6], stamp[7]]);
    let stored = u32::from_le_bytes(stamp[8..12].try_into().unwrap());
    assert_eq!(stored, checksum::wal_stamp(&WAL_MAGIC, format, reserved));
    assert!(format > 1);
}

#[test]
fn the_stamp_cannot_be_confused_with_a_record_length() {
    let as_len = u32::from_le_bytes(WAL_MAGIC);
    assert!(as_len > MAX_RECORD_LEN);
}

#[test]
fn a_log_from_a_newer_format_is_refused_rather_than_guessed_at() {
    let mut stamp = wal_frame::encode_stamp(1);
    stamp[4..6].copy_from_slice(&3u16.to_le_bytes());
    let head = checksum::wal_stamp(&WAL_MAGIC, 3, 0);
    stamp[8..12].copy_from_slice(&head.to_le_bytes());
    let err = validate_wal_stamp(&stamp).expect_err("a newer format must not be parsed");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("format 3"), "{err}");
}

#[test]
fn a_damaged_but_present_stamp_is_refused() {
    let dir = TempDir::new().unwrap();
    for byte in 4..STAMP_LEN {
        let (wal, path) = new_wal(&dir);
        drop(wal);
        flip_byte(&path, byte);
        let err = Wal::replay(&path).expect_err("a damaged stamp must not pass");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "byte {byte}");
        assert!(err.to_string().contains("test.wal"), "{err}");
    }
}

#[test]
fn bytes_that_are_not_a_stamped_log_name_no_format() {
    for bytes in [
        &b"this is not a write-ahead log at all"[..],
        &[0xFFu8; 64][..],
        &[0x00u8; 64][..],
    ] {
        assert_eq!(validate_wal_stamp(bytes).unwrap(), None);
    }
}

/// A stamp a crash cut short holds nothing a sync covered, since a sync
/// would have made the whole stamp durable. The newest log in that state
/// yields nothing and reports every byte it holds as discarded.
#[test]
fn a_stamp_cut_short_reads_as_an_empty_log_whose_bytes_are_reported() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("torn-stamp.wal");
    let stamp = wal_frame::encode_stamp(5);
    for cut in 0..STAMP_LEN {
        fs::write(&path, &stamp[..cut]).unwrap();
        let (entries, tail) = replay_at(&path, WalPosition::Newest).unwrap();
        assert!(entries.is_empty(), "cut at {cut}");
        let expected = (cut > 0).then_some(TailVerdict {
            offset: 0,
            discarded_bytes: cut as u64,
        });
        assert_eq!(tail, expected, "cut at {cut}");
    }
}

/// An unstamped earlier log is the leftover of a crash at its creation
/// that a later recovery already replaced: it yields nothing, whatever the
/// crash left in it, unless what is left shows a stamp written whole.
#[test]
fn an_unstamped_earlier_log_yields_nothing_unless_its_stamp_was_written() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("earlier.wal");
    for leftover in [&[0u8; 300][..], b"garbage a crash left at creation"] {
        fs::write(&path, leftover).unwrap();
        let (entries, tail) = replay_at(&path, WalPosition::Earlier).unwrap();
        assert!(entries.is_empty() && tail.is_none());
    }
    let (wal, written) = new_wal(&dir);
    drop(wal);
    flip_byte(&written, 2);
    let err = replay_at(&written, WalPosition::Earlier).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[path = "wal_rule_tests.rs"]
mod rule;

#[path = "wal_seal_tests.rs"]
mod sealed;
