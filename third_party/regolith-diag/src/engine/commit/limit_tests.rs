//! Coverage for the record-length limit: the fold in the three
//! validation loops, the refusal before a write takes a slot of the commit
//! ring, and the last-resort guard when a slot is decided.

use std::collections::BTreeMap;

use tempfile::TempDir;

use super::super::wal::{ops_record_len, put_record_len};
use super::super::{EngineOptions, ValidationSet};
use super::*;
use crate::WriteBatchOp;
use crate::options::DEFAULT_MAX_VALUE_SIZE;

// -- helpers, reusing the shapes `commit::tests` already uses ---------

fn open_engine(dir: &TempDir) -> Arc<RegolithEngine> {
    RegolithEngine::open(dir.path(), EngineOptions::default()).expect("engine open")
}

/// Default-column-family prefix, the shape every engine key carries.
fn key(name: &[u8]) -> Vec<u8> {
    let mut k = vec![0u8; 4];
    k.extend_from_slice(name);
    k
}

fn durable_put(name: &[u8], value: &[u8]) -> WriteRequest {
    WriteRequest::Put {
        key: key(name),
        value: value.to_vec(),
        durability: DurabilityMode::Immediate,
        disable_wal: false,
    }
}

/// Puts under `count` keys whose values are zeroed and sized so the write
/// frames to exactly `framed` bytes. Uses the fact that a record's length
/// is its length with empty values plus the sum of the value lengths.
fn puts_framing_to(framed: usize, count: usize) -> Vec<WriteBatchOp> {
    let mut ops: Vec<WriteBatchOp> = (0..count)
        .map(|i| WriteBatchOp::Put {
            key: key(format!("k{i:02}").as_bytes()),
            value: Vec::new(),
        })
        .collect();
    let base = ops_record_len(&ops);
    let mut remaining = framed - base;
    for op in &mut ops {
        let WriteBatchOp::Put { value, .. } = op else {
            unreachable!("puts_framing_to only builds Put ops")
        };
        let take = remaining.min(DEFAULT_MAX_VALUE_SIZE);
        *value = vec![0u8; take];
        remaining -= take;
    }
    assert_eq!(ops_record_len(&ops), framed);
    ops
}

/// 16 zeroed puts plus one delete, one merge and one range delete, framed
/// to exactly `framed` bytes: every arm `validate_ops_sizes` folds gets
/// exercised, not only the put one.
fn mixed_ops_framing_to(framed: usize) -> Vec<WriteBatchOp> {
    let mut ops: Vec<WriteBatchOp> = (0..16)
        .map(|i| WriteBatchOp::Put {
            key: key(format!("k{i:02}").as_bytes()),
            value: Vec::new(),
        })
        .collect();
    ops.push(WriteBatchOp::Delete { key: key(b"del") });
    ops.push(WriteBatchOp::Merge {
        key: key(b"m"),
        operand: b"op".to_vec(),
    });
    ops.push(WriteBatchOp::DeleteRange {
        start: key(b"ra"),
        end: key(b"rb"),
    });
    let base = ops_record_len(&ops);
    let mut remaining = framed - base;
    for op in &mut ops {
        if let WriteBatchOp::Put { value, .. } = op {
            let take = remaining.min(DEFAULT_MAX_VALUE_SIZE);
            *value = vec![0u8; take];
            remaining -= take;
        }
    }
    assert_eq!(ops_record_len(&ops), framed);
    ops
}

/// Buffers shaped like a transaction commit's: 17 zeroed `Some` values,
/// one `None`, one range delete, one merge, framed to exactly `framed`
/// bytes through `grouped_batch_ops`, the same mapping
/// `commit_optimistic` validates against.
#[allow(clippy::type_complexity)]
fn commit_buffers_framing_to(
    framed: usize,
) -> (
    BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    Vec<(Vec<u8>, Vec<u8>)>,
    Vec<(Vec<u8>, Vec<u8>)>,
) {
    let mut point_ops: BTreeMap<Vec<u8>, Option<Vec<u8>>> = (0..17)
        .map(|i| (key(format!("k{i:02}").as_bytes()), Some(Vec::new())))
        .collect();
    point_ops.insert(key(b"del"), None);
    let range_deletes = vec![(key(b"ra"), key(b"rb"))];
    let merges = vec![(key(b"m"), b"op".to_vec())];

    // Values are still empty here, so cloning to measure the base length
    // costs nothing resident.
    let base = ops_record_len(&grouped_batch_ops(
        point_ops.clone(),
        range_deletes.clone(),
        merges.clone(),
    ));
    let mut remaining = framed - base;
    for v in point_ops.values_mut().flatten() {
        let take = remaining.min(DEFAULT_MAX_VALUE_SIZE);
        *v = vec![0u8; take];
        remaining -= take;
    }

    // From here the values are zeroed and can run to a gigabyte, so the
    // buffers are moved through `grouped_batch_ops` rather than cloned:
    // cloning a filled `Vec<u8>` copies its bytes and faults the pages
    // resident, which is exactly the cost these tests exist to avoid.
    let ops = grouped_batch_ops(point_ops, range_deletes, merges);
    assert_eq!(ops_record_len(&ops), framed);

    let mut point_ops = BTreeMap::new();
    let mut range_deletes = Vec::new();
    let mut merges = Vec::new();
    for op in ops {
        match op {
            WriteBatchOp::Put { key, value } => {
                point_ops.insert(key, Some(value));
            }
            WriteBatchOp::Delete { key } => {
                point_ops.insert(key, None);
            }
            WriteBatchOp::DeleteRange { start, end } => range_deletes.push((start, end)),
            WriteBatchOp::Merge { key, operand } => merges.push((key, operand)),
        }
    }
    (point_ops, range_deletes, merges)
}

/// Run `call` and assert it refuses the write before the write takes a slot
/// of the commit ring: the ring hands out no index, and every trace of the
/// attempt (sequence, WAL offset, the key itself) is untouched.
fn assert_refused_before_the_pipeline(
    engine: &Arc<RegolithEngine>,
    probe_key: &[u8],
    call: impl FnOnce(&Arc<RegolithEngine>) -> io::Result<u64>,
) {
    let before_seq = engine.last_decided_seq();
    let before_wal = engine.log_end();
    let before_slots = engine.pipeline_slots_taken();

    let err = call(engine).expect_err("an oversized write must be refused");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

    assert_eq!(
        engine.pipeline_slots_taken(),
        before_slots,
        "the record-limit check must refuse a write before it takes a slot"
    );
    assert_eq!(engine.last_decided_seq(), before_seq);
    assert_eq!(engine.log_end(), before_wal);
    assert_eq!(engine.get(probe_key, u64::MAX).unwrap(), None);
}

// -- T3.1, T3.2: the validation loops fold the record length ----------

#[test]
fn validate_ops_sizes_refuses_one_byte_past_the_limit() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);

    let ops = puts_framing_to(MAX_RECORD_LEN as usize, 17);
    assert!(engine.validate_ops_sizes(&ops, false).is_ok());

    let ops = puts_framing_to(MAX_RECORD_LEN as usize + 1, 17);
    let err = engine
        .validate_ops_sizes(&ops, false)
        .expect_err("a record one byte past the limit must be refused");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        err.to_string(),
        format!(
            "write is too large: it would log {} bytes and one write can log at most {}; \
             split it into smaller writes",
            MAX_RECORD_LEN as usize + 1,
            MAX_RECORD_LEN
        )
    );
    assert!(engine.validate_ops_sizes(&ops, true).is_ok());

    // Every arm of the fold must contribute, not only the put one.
    let mixed = mixed_ops_framing_to(MAX_RECORD_LEN as usize);
    assert!(engine.validate_ops_sizes(&mixed, false).is_ok());
    let mixed = mixed_ops_framing_to(MAX_RECORD_LEN as usize + 1);
    assert!(engine.validate_ops_sizes(&mixed, false).is_err());
}

// -- T3.3: refused before the pipeline, at every producer -------------

#[test]
fn an_oversized_write_is_refused_before_it_reaches_the_pipeline() {
    {
        let dir = TempDir::new().unwrap();
        let engine = open_engine(&dir);
        let probe = key(b"k00");
        assert_refused_before_the_pipeline(&engine, &probe, move |engine| {
            engine.apply_batch(
                puts_framing_to(MAX_RECORD_LEN as usize + 1, 17),
                DurabilityMode::Eventual,
                false,
            )
        });
    }

    {
        let dir = TempDir::new().unwrap();
        let engine = open_engine(&dir);
        let (point_ops, range_deletes, merges) =
            commit_buffers_framing_to(MAX_RECORD_LEN as usize + 1);
        let probe = key(b"k00");
        assert_refused_before_the_pipeline(&engine, &probe, move |engine| {
            let checks = ValidationSet {
                reads: Vec::new(),
                writes_at: None,
                blind_merges_commute: false,
                exempt: Vec::new(),
                ranges: Vec::new(),
            };
            engine
                .commit_optimistic(
                    checks,
                    point_ops,
                    range_deletes,
                    merges,
                    Vec::new(),
                    DurabilityMode::Eventual,
                )
                .map(|_outcome| 0)
        });
    }

    {
        let dir = TempDir::new().unwrap();
        let engine = RegolithEngine::open(
            dir.path(),
            EngineOptions {
                max_value_size: u32::MAX as usize,
                ..EngineOptions::default()
            },
        )
        .expect("engine open");
        let probe = key(b"big");
        let base = put_record_len(&probe, &[]);
        let v = MAX_RECORD_LEN as usize + 1 - base;
        assert_eq!(
            put_record_len(&probe, &vec![0u8; v]),
            MAX_RECORD_LEN as usize + 1
        );
        let put_key = probe.clone();
        assert_refused_before_the_pipeline(&engine, &probe, move |engine| {
            engine.apply_single_put(put_key, vec![0u8; v], DurabilityMode::Eventual, false)
        });
    }
}

// -- T3.4: a write over the limit that reaches the pipeline ------------

/// The last line of defence, for a request that reached the pipeline without
/// its producer's check: deciding its slot refuses it before it takes a
/// sequence, a byte of the log or a memtable entry, and the pipeline goes
/// on.
#[test]
fn a_write_past_the_limit_is_refused_when_decided_before_it_takes_a_sequence() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    engine
        .commit_through_pipeline(durable_put(b"before", b"v"))
        .expect("a write under the limit commits");

    let before_seq = engine.last_decided_seq();
    let before_wal = engine.log_end();
    let err = engine
        .commit_through_pipeline(WriteRequest::Batch {
            ops: puts_framing_to(MAX_RECORD_LEN as usize + 1, 17),
            durability: DurabilityMode::Eventual,
            disable_wal: false,
        })
        .expect_err("a record past the limit must be refused");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        engine.last_decided_seq(),
        before_seq,
        "a refused write must not take a sequence"
    );
    assert_eq!(engine.log_end(), before_wal, "nor a byte of the log");
    assert_eq!(engine.get(&key(b"k00"), u64::MAX).unwrap(), None);

    // The same batch with the log skipped has no record to be too long.
    engine
        .commit_through_pipeline(durable_put(b"after", b"v"))
        .expect("the pipeline goes on after a refusal");
    assert_eq!(engine.last_decided_seq(), before_seq + 1);
    assert!(engine.pipeline_is_idle());
}
