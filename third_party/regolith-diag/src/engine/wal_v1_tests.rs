//! Tests of the format 1 reader: logs as 0.1.x wrote them still replay,
//! by the rules they always replayed by.

use super::*;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

use crate::engine::wal::{WAL_STAMP_LEN, Wal, WalEntry, validate_wal_stamp};
use crate::engine::wal_replay::{WalPosition, WalReplayIter};

fn flip_byte(path: &Path, offset: usize) {
    let mut bytes = fs::read(path).unwrap();
    bytes[offset] ^= 0xFF;
    fs::write(path, &bytes).unwrap();
}

/// The payload of a format 1 put record.
fn put_data(key: &[u8], value: &[u8], seq: u64) -> Vec<u8> {
    let mut d = Vec::new();
    d.extend_from_slice(&(key.len() as u32).to_le_bytes());
    d.extend_from_slice(key);
    d.extend_from_slice(&(value.len() as u32).to_le_bytes());
    d.extend_from_slice(value);
    d.extend_from_slice(&seq.to_le_bytes());
    d
}

/// A format 1 record of `record_type` around `data`.
fn raw_record(record_type: u8, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    encode_record(&mut out, record_type, data);
    out
}

/// A format 1 log of puts `k0`, `k1`, ... at `path`.
fn log_of_puts(path: &Path, n: u64) {
    let records: Vec<Vec<u8>> = (0..n)
        .map(|i| put(format!("k{i}").as_bytes(), b"v", i))
        .collect();
    write_log(path, &records);
}

fn replay_at(
    path: &Path,
    position: WalPosition,
) -> io::Result<(Vec<WalEntry>, Option<TailVerdict>)> {
    let mut iter = WalReplayIter::open(&crate::env::std_env(), path, position, None)?;
    let mut entries = Vec::new();
    while let Some(entry) = iter.next_entry()? {
        entries.push(entry);
    }
    Ok((entries, iter.discarded_tail()))
}

#[test]
fn a_format_1_log_replays_every_record_type_and_batches() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("v1.wal");
    let ops = vec![
        WriteBatchOp::Put {
            key: b"p".to_vec(),
            value: b"1".to_vec(),
        },
        WriteBatchOp::DeleteRange {
            start: b"ra".to_vec(),
            end: b"rb".to_vec(),
        },
        WriteBatchOp::Merge {
            key: b"m".to_vec(),
            operand: b"op".to_vec(),
        },
    ];
    write_log(
        &path,
        &[put(b"a", b"1", 1), delete(b"b", 2), batch(&ops, 3)],
    );
    assert_eq!(
        Wal::replay(&path).unwrap(),
        vec![
            WalEntry::Put {
                key: b"a".to_vec(),
                value: b"1".to_vec(),
                seq: 1,
            },
            WalEntry::Delete {
                key: b"b".to_vec(),
                seq: 2,
            },
            WalEntry::Put {
                key: b"p".to_vec(),
                value: b"1".to_vec(),
                seq: 3,
            },
            WalEntry::DeleteRange {
                start: b"ra".to_vec(),
                end: b"rb".to_vec(),
                seq: 4,
            },
            WalEntry::Merge {
                key: b"m".to_vec(),
                operand: b"op".to_vec(),
                seq: 5,
            },
        ]
    );
}

#[test]
fn a_format_1_stamp_validates_as_format_1() {
    assert_eq!(validate_wal_stamp(&stamp()).unwrap(), Some(1));
    let mut stamp = stamp();
    stamp[8] ^= 0xFF;
    assert_eq!(
        validate_wal_stamp(&stamp).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn replay_errors_on_trailing_checksum_flip() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.wal");
    log_of_puts(&path, 2);
    let len = fs::metadata(&path).unwrap().len() as usize;
    flip_byte(&path, len - 1);
    assert_eq!(
        Wal::replay(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn replay_errors_on_trailing_data_byte_flip() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.wal");
    log_of_puts(&path, 2);
    let len = fs::metadata(&path).unwrap().len() as usize;
    flip_byte(&path, len - 6);
    assert_eq!(
        Wal::replay(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn record_checksum_covers_header_fields() {
    let data = put_data(b"k", b"v", 1);
    let len = data.len() as u32;
    let baseline = checksum::wal_record(len, RECORD_PUT, &data);
    assert_ne!(baseline, checksum::wal_record(len + 1, RECORD_PUT, &data));
    assert_ne!(baseline, checksum::wal_record(len, RECORD_DELETE, &data));
}

#[test]
fn replay_errors_on_record_type_flip() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.wal");
    log_of_puts(&path, 1);
    flip_byte(&path, WAL_STAMP_LEN + 4);
    assert_eq!(
        Wal::replay(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn replay_stops_at_a_truncated_trailing_header() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.wal");
    log_of_puts(&path, 1);
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend_from_slice(&[0xFF, 0xFF]);
    fs::write(&path, &bytes).unwrap();
    let (entries, tail) = replay_at(&path, WalPosition::Newest).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(tail.unwrap().discarded_bytes, 2);
}

#[test]
fn replay_treats_a_length_beyond_the_file_as_a_torn_tail() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bad.wal");
    let mut bytes = stamp().to_vec();
    bytes.extend_from_slice(&1000u32.to_le_bytes());
    bytes.push(RECORD_PUT);
    fs::write(&path, &bytes).unwrap();
    let (entries, tail) = replay_at(&path, WalPosition::Newest).unwrap();
    assert!(entries.is_empty());
    assert_eq!(tail.unwrap().offset, WAL_STAMP_LEN as u64);
}

#[test]
fn replay_rejects_a_length_beyond_the_file_when_whole_records_follow() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.wal");
    log_of_puts(&path, 4);
    let mut bytes = fs::read(&path).unwrap();
    let second = frame_at(&bytes, WAL_STAMP_LEN).unwrap().end;
    bytes[second..second + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    fs::write(&path, &bytes).unwrap();

    let err = Wal::replay(&path).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    let message = err.to_string();
    assert!(message.contains("test.wal"), "{message}");
    assert!(message.contains(&second.to_string()), "{message}");
}

/// Every fifth offset of this tail frames as a record claiming a 1 MiB
/// payload that fits, so checking each candidate on its own would put
/// about 600 GiB through the checksum for a 4 MiB tail. Requiring the
/// remainder to tile collapses it to one pass. The bound is wall clock
/// because the defect is asymptotic; the margin is wide enough that a
/// loaded machine cannot trip it.
#[test]
fn replay_of_a_self_similar_torn_tail_stays_linear() {
    const TAIL: usize = 4 << 20;
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.wal");
    log_of_puts(&path, 1);
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend_from_slice(&u32::MAX.to_le_bytes());
    bytes.push(RECORD_PUT);
    bytes.extend(
        [0x00, 0x00, 0x10, 0x00, RECORD_PUT]
            .iter()
            .cycle()
            .take(TAIL),
    );
    fs::write(&path, &bytes).unwrap();

    let start = std::time::Instant::now();
    let entries = Wal::replay(&path).unwrap();
    let elapsed = start.elapsed();
    assert_eq!(entries.len(), 1);
    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "replaying a {TAIL}-byte torn tail took {elapsed:?}"
    );
}

#[test]
fn replay_keeps_every_whole_record_before_a_cut_at_any_offset() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.wal");
    log_of_puts(&path, 4);
    let full = fs::read(&path).unwrap();
    let mut boundaries = vec![WAL_STAMP_LEN];
    while let Some(frame) = frame_at(&full, *boundaries.last().unwrap()) {
        boundaries.push(frame.end);
    }
    assert_eq!(boundaries.len(), 5, "four records tile the file");

    for cut in WAL_STAMP_LEN..=full.len() {
        fs::write(&path, &full[..cut]).unwrap();
        let whole = boundaries.iter().filter(|b| **b <= cut).count() - 1;
        let entries = Wal::replay(&path)
            .unwrap_or_else(|e| panic!("a cut at {cut} is a torn tail, not corruption: {e}"));
        assert_eq!(entries.len(), whole, "cut at {cut}");
    }
}

#[test]
fn replay_errors_on_unknown_record_type() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("mixed.wal");
    write_log(
        &path,
        &[
            raw_record(0xEF, b"opaque bytes"),
            raw_record(RECORD_PUT, &put_data(b"after", b"ok", 42)),
        ],
    );
    assert_eq!(
        Wal::replay(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn replay_rejects_malformed_batch_entry_payload() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bad_batch.wal");
    let mut data = Vec::new();
    data.extend_from_slice(&1u32.to_le_bytes());
    data.push(RECORD_PUT);
    data.extend_from_slice(&100u32.to_le_bytes());
    data.extend_from_slice(&[0xAA, 0xBB]);
    write_log(&path, &[raw_record(RECORD_BATCH, &data)]);
    assert_eq!(
        Wal::replay(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn replay_rejects_batch_trailing_bytes() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("trailing_batch.wal");
    let mut data = Vec::new();
    data.extend_from_slice(&0u32.to_le_bytes());
    data.push(0xFF);
    write_log(&path, &[raw_record(RECORD_BATCH, &data)]);
    assert_eq!(
        Wal::replay(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

/// Format 1 is read as it always was, including its known limit
/// (`WalRecovery.tla`, RED `Format1`): a whole final record that fails its
/// checksum with non-zero bytes after it refuses, even though a crash can
/// leave exactly that. Format 2 opens on the same state.
#[test]
fn format_1_still_refuses_a_whole_bad_final_record_with_garbage_after_it() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.wal");
    log_of_puts(&path, 2);
    let mut bytes = fs::read(&path).unwrap();
    let second = frame_at(&bytes, WAL_STAMP_LEN).unwrap().end;
    for b in &mut bytes[second + 5..] {
        *b ^= 0x5A;
    }
    bytes.extend_from_slice(&[0x77; 64]);
    fs::write(&path, &bytes).unwrap();
    assert!(replay_at(&path, WalPosition::Newest).is_err());
}

#[test]
fn a_zeroed_tail_ends_a_format_1_log() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.wal");
    log_of_puts(&path, 3);
    let mut bytes = fs::read(&path).unwrap();
    let second = frame_at(&bytes, WAL_STAMP_LEN).unwrap().end;
    for b in &mut bytes[second..] {
        *b = 0;
    }
    fs::write(&path, &bytes).unwrap();
    let (entries, tail) = replay_at(&path, WalPosition::Newest).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(tail.unwrap().offset, second as u64);
}

#[test]
fn any_damage_in_an_earlier_format_1_log_refuses() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.wal");
    log_of_puts(&path, 2);
    let bytes = fs::read(&path).unwrap();
    fs::write(&path, &bytes[..bytes.len() - 2]).unwrap();
    assert!(replay_at(&path, WalPosition::Earlier).is_err());
    fs::write(&path, &bytes).unwrap();
    assert_eq!(replay_at(&path, WalPosition::Earlier).unwrap().0.len(), 2);
}

#[test]
fn a_format_1_stamp_cut_short_reads_as_an_empty_log() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("torn-stamp.wal");
    for cut in 0..WAL_STAMP_LEN {
        fs::write(&path, &stamp()[..cut]).unwrap();
        assert!(Wal::replay(&path).unwrap().is_empty(), "cut at {cut}");
    }
}
