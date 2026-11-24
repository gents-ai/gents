//! The tail judgment on manifests built byte by byte, plain and sealed.

use std::path::Path;
use std::sync::Arc;

use super::super::sealed::{ManifestSeal, encode_stamp};
use super::super::{MANIFEST_MAGIC, ManifestRecord, VersionSet};
use super::{DroppedTail, Opener, batch_at, judge};
use crate::engine::seal::Keyring;
use crate::engine::seal::test_keys::{keyring, wrong_keyring};

fn stamp() -> Vec<u8> {
    VersionSet::encode_stamp().to_vec()
}

/// A plain batch: the offset it lands at binds only a sealed one.
fn batch(records: &[ManifestRecord]) -> Vec<u8> {
    VersionSet::encode_records(records, 0, None).unwrap().0
}

/// A batch that only reserves a file id: never synced on its own.
fn reservation(id: u64) -> Vec<u8> {
    batch(&[ManifestRecord::SetNextFileId(id)])
}

/// A batch that needs a sync, as a flush's does.
fn synced(seq: u64) -> Vec<u8> {
    batch(&[
        ManifestRecord::SetLastSeq(seq),
        ManifestRecord::SetMinWalId(seq),
    ])
}

fn path() -> &'static Path {
    Path::new("/db/MANIFEST")
}

/// Every batch in `parts` after a stamp, and the offset each begins at.
fn manifest(parts: &[Vec<u8>]) -> (Vec<u8>, Vec<usize>) {
    let mut data = stamp();
    let mut starts = Vec::new();
    for part in parts {
        starts.push(data.len());
        data.extend_from_slice(part);
    }
    (data, starts)
}

/// Flip a byte of the payload of the batch beginning at `at`, so its
/// checksum fails while its length still reads.
fn damage_payload(data: &mut [u8], at: usize) {
    data[at + 4] ^= 0xFF;
}

#[test]
fn a_batch_reads_back_whole_only_where_its_checksum_holds() {
    let (data, starts) = manifest(&[synced(1), reservation(9)]);
    let first = batch_at(&data, starts[0]).expect("the first batch reads back");
    assert_eq!(first.end, starts[1]);
    assert_eq!(batch_at(&data, starts[1]).unwrap().end, data.len());
    assert!(batch_at(&data, starts[0] + 1).is_none());
    assert!(batch_at(&data, data.len()).is_none());
    assert!(batch_at(&data[..data.len() - 1], starts[1]).is_none());
}

#[test]
fn a_length_past_the_end_of_the_file_is_no_batch() {
    let mut data = stamp();
    data.extend_from_slice(&u32::MAX.to_le_bytes());
    data.extend_from_slice(&[0; 16]);
    assert!(batch_at(&data, 12).is_none());
}

#[test]
fn a_torn_last_batch_is_a_tail_to_drop() {
    let (data, starts) = manifest(&[synced(1), synced(2)]);
    let cut = &data[..data.len() - 3];
    assert_eq!(
        judge(cut, starts[1], path(), None).unwrap(),
        DroppedTail {
            offset: starts[1] as u64,
            bytes: (cut.len() - starts[1]) as u64,
        }
    );
}

#[test]
fn reservations_after_the_damage_prove_nothing() {
    let (mut data, starts) = manifest(&[reservation(2), reservation(3), reservation(4)]);
    damage_payload(&mut data, starts[0]);
    let tail = judge(&data, starts[0], path(), None).expect("only reservations follow the damage");
    assert_eq!(tail.offset, starts[0] as u64);
}

#[test]
fn a_synced_batch_written_last_proves_nothing() {
    // The batch whose sync was under way at the cut: whole, but nothing was
    // written after it, so nothing shows its sync completed.
    let (mut data, starts) = manifest(&[reservation(2), synced(1)]);
    damage_payload(&mut data, starts[0]);
    assert!(judge(&data, starts[0], path(), None).is_ok());
}

#[test]
fn a_synced_batch_with_bytes_after_it_proves_the_damage_durable() {
    let (mut data, starts) = manifest(&[synced(1), synced(2), reservation(9)]);
    damage_payload(&mut data, starts[0]);
    let err = judge(&data, starts[0], path(), None).expect_err("the damage lies below a proof");
    let message = err.to_string();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(message.contains("/db/MANIFEST"), "{message}");
    assert!(
        message.contains(&format!("offset {}", starts[0])),
        "{message}"
    );
    assert!(
        message.contains(&format!("offset {}", starts[2])),
        "{message}"
    );
}

#[test]
fn a_proof_is_found_past_a_damaged_length() {
    let (mut data, starts) = manifest(&[synced(1), synced(2), synced(3)]);
    data[starts[0]..starts[0] + 4].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
    let err = judge(&data, starts[0], path(), None).expect_err("the scan finds the later batches");
    assert!(err.to_string().contains(&format!("offset {}", starts[2])));
}

#[test]
fn a_tail_of_zeros_or_unrelated_bytes_is_dropped() {
    let (data, _) = manifest(&[synced(1)]);
    let end = data.len();
    for fill in [0u8, 0xAB] {
        let mut torn = data.clone();
        torn.extend(std::iter::repeat_n(fill, 4096));
        let tail = judge(&torn, end, path(), None).expect("a crash leaves such a tail");
        assert_eq!(tail.bytes, 4096);
    }
    let mut torn = data.clone();
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    for _ in 0..4096 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        torn.push(x as u8);
    }
    assert!(judge(&torn, end, path(), None).is_ok());
}

#[test]
fn a_batch_this_build_cannot_decode_counts_as_needing_a_sync() {
    let mut unknown = vec![0u8; 4];
    unknown.extend_from_slice(&[0xEE; 9]);
    let len = (unknown.len() - 4) as u32;
    unknown[..4].copy_from_slice(&len.to_le_bytes());
    let check = crate::engine::checksum::manifest_record(len, &unknown[4..]);
    unknown.extend_from_slice(&check.to_le_bytes());

    let (mut data, starts) = manifest(&[synced(1), unknown, reservation(9)]);
    damage_payload(&mut data, starts[0]);
    assert!(judge(&data, starts[0], path(), None).is_err());
}

#[test]
fn every_record_but_reservations_and_retirements_needs_a_sync() {
    use crate::engine::sstable::SsTableMeta;
    let meta = SsTableMeta {
        file_id: 3,
        smallest_key: b"a".to_vec(),
        largest_key: b"b".to_vec(),
        file_size: 1,
        num_entries: 1,
        global_seq: None,
        deletions: None,
    };
    let needs = [
        ManifestRecord::AddFile { level: 0, meta },
        ManifestRecord::RemoveFile {
            level: 0,
            file_id: 3,
        },
        ManifestRecord::SetLastSeq(1),
        ManifestRecord::Reset {
            next_file_id: 2,
            min_wal_id: 1,
        },
    ];
    assert!(needs.iter().all(ManifestRecord::requires_sync));
    assert!(!ManifestRecord::SetNextFileId(1).requires_sync());
    assert!(!ManifestRecord::SetMinWalId(1).requires_sync());
}

/// A sealed manifest under key 1 of `ring`: its stamp, then each batch of
/// `parts` sealed at the offset it lands at. Key 1, so a batch read as plain
/// records starts with the byte of an `AddFile`, as a real sealed one does.
fn sealed_manifest(
    ring: &Arc<Keyring>,
    salt: &[u8; 16],
    parts: &[&[ManifestRecord]],
) -> (Vec<u8>, Vec<usize>) {
    let seal = ManifestSeal {
        keyring: Arc::clone(ring),
        salt: *salt,
    };
    let mut data = encode_stamp(&MANIFEST_MAGIC, salt).to_vec();
    let mut starts = Vec::new();
    for records in parts {
        starts.push(data.len());
        let (batch, _) =
            VersionSet::encode_records(records, data.len() as u64, Some(&seal)).unwrap();
        data.extend_from_slice(&batch);
    }
    (data, starts)
}

const SALT: [u8; 16] = [7; 16];
const SYNCED: &[ManifestRecord] = &[
    ManifestRecord::SetLastSeq(1),
    ManifestRecord::SetMinWalId(1),
];
const RESERVE_5: &[ManifestRecord] = &[ManifestRecord::SetNextFileId(5)];
const RESERVE_6: &[ManifestRecord] = &[ManifestRecord::SetNextFileId(6)];
const TABLE_BATCH: &[ManifestRecord] = &[
    ManifestRecord::SetLastSeq(2),
    ManifestRecord::SetMinWalId(2),
];

/// A crash tears a sealed reservation and keeps the next one and the table's
/// batch whole. Opened first, the whole reservation reads as one that needed
/// no sync, so nothing proves the damage synced and the tail is dropped.
/// Read as plain records, it reads as a sync and the open refuses: RED
/// JudgeCiphertext of `ManifestRecovery.tla`.
#[test]
fn a_torn_sealed_reservation_before_whole_ones_is_a_crash_tail() {
    let ring = keyring(&[1]);
    let (mut data, starts) =
        sealed_manifest(&ring, &SALT, &[SYNCED, RESERVE_5, RESERVE_6, TABLE_BATCH]);
    damage_payload(&mut data, starts[1]);
    let opener = Opener {
        keyring: &ring,
        salt: &SALT,
    };
    assert_eq!(
        judge(&data, starts[1], path(), Some(opener)).expect("a crash leaves this tail"),
        DroppedTail {
            offset: starts[1] as u64,
            bytes: (data.len() - starts[1]) as u64,
        }
    );
    assert!(
        judge(&data, starts[1], path(), None).is_err(),
        "sealed bytes read as plain records count as a sync"
    );
}

/// A sealed batch that needed a sync, whole and with a batch after it,
/// still proves the damage before it synced.
#[test]
fn a_whole_sealed_batch_that_needed_a_sync_still_proves_the_damage() {
    let ring = keyring(&[1]);
    let (mut data, starts) =
        sealed_manifest(&ring, &SALT, &[SYNCED, RESERVE_5, TABLE_BATCH, RESERVE_6]);
    damage_payload(&mut data, starts[1]);
    let opener = Opener {
        keyring: &ring,
        salt: &SALT,
    };
    let err = judge(&data, starts[1], path(), Some(opener)).expect_err("a later sync proves it");
    assert!(
        err.to_string().contains(&format!("offset {}", starts[1])),
        "{err}"
    );
}

/// A whole batch after the damage that does not open under the keyring (a
/// wrong key here) cannot show it needed no sync, so it counts as one.
#[test]
fn a_sealed_batch_that_does_not_open_counts_as_a_sync() {
    let ring = keyring(&[1]);
    let (mut data, starts) =
        sealed_manifest(&ring, &SALT, &[SYNCED, RESERVE_5, RESERVE_6, TABLE_BATCH]);
    damage_payload(&mut data, starts[1]);
    let wrong = wrong_keyring(&[1]);
    let opener = Opener {
        keyring: &wrong,
        salt: &SALT,
    };
    assert!(judge(&data, starts[1], path(), Some(opener)).is_err());
}
