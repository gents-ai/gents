use super::*;
use crate::column_family::{DEFAULT_CF_ID, prefix_key};
use crate::engine::commit::{Settled, WriteRequest, txn::TxnRequest};
use crate::engine::io::scope;
use crate::engine::{ConflictKey, DurabilityMode, ReadRule};
use crate::{Db, IoBudget, IoQueue, Options, PerfLevel};
use tempfile::TempDir;

fn key(name: &[u8]) -> Vec<u8> {
    prefix_key(DEFAULT_CF_ID, name)
}

fn cold(dir: &TempDir) -> Db {
    let options = || Options::default().block_size(256).block_cache_size(0);
    {
        let db = Db::open(dir.path(), options()).unwrap();
        db.put(b"a", &[b'x'; 4096]).unwrap();
        db.put(b"b", &[b'x'; 4096]).unwrap();
        db.flush().unwrap();
        db.close().unwrap();
    }
    Db::open(dir.path(), options()).unwrap()
}

fn checks(snapshot: u64) -> ValidationSet {
    ValidationSet {
        reads: Vec::new(),
        ranges: Vec::new(),
        writes_at: Some(snapshot),
        blind_merges_commute: false,
        exempt: Vec::new(),
    }
}

fn suspended_after_first_item(
    db: &Db,
    queue: &mut IoQueue,
    checks: &ValidationSet,
    ops: &[WriteBatchOp],
) -> Option<EarlyProgress> {
    let mut progress = None;
    for _ in 0..64 {
        let attempt = {
            let _mode = scope::cache_only(queue.id());
            db.engine()
                .check_early_with_progress(checks, ops, &mut progress)
        };
        match attempt {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if progress.as_ref().unwrap().at >= 1 {
                    return progress;
                }
                queue.poll(IoBudget::ALL);
            }
            _ => panic!("expected a cold miss after the first item"),
        }
    }
    panic!("the check did not reach its second item");
}

fn finish(
    db: &Db,
    queue: &mut IoQueue,
    checks: &ValidationSet,
    ops: &[WriteBatchOp],
    progress: &mut Option<EarlyProgress>,
) -> Early {
    for _ in 0..64 {
        queue.poll(IoBudget::ALL);
        let attempt = {
            let _mode = scope::cache_only(queue.id());
            db.engine().check_early_with_progress(checks, ops, progress)
        };
        match attempt {
            Ok(EarlyVerdict::Marks(early)) => return early,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            _ => panic!("the resumed check did not retain its marker"),
        }
    }
    panic!("the remaining items did not finish");
}

fn decide(
    db: &Db,
    checks: ValidationSet,
    ops: Vec<WriteBatchOp>,
    early: Early,
) -> io::Result<Settled> {
    db.engine()
        .commit_through_pipeline(WriteRequest::Txn(Box::new(TxnRequest {
            checks,
            ops,
            appends: Vec::new(),
            durability: DurabilityMode::Eventual,
            perf: PerfLevel::Disable,
            early,
            read_queue: None,
        })))
}

#[test]
fn a_write_to_the_completed_prefix_is_checked_above_the_original_horizon() {
    let dir = TempDir::new().unwrap();
    let db = cold(&dir);
    let snapshot = db.latest_sequence();
    let mut checks = checks(snapshot);
    checks.reads = [b"a", b"b"]
        .into_iter()
        .map(|name| ConflictKey {
            key: key(name),
            observed_seq: snapshot,
            found: true,
            access: Access::Read,
            rule: ReadRule::Seq,
        })
        .collect();
    let ops = vec![WriteBatchOp::Put {
        key: key(b"result"),
        value: b"mine".to_vec(),
    }];
    db.put(b"other", b"x").unwrap();
    db.flush().unwrap();
    let horizon = db.latest_sequence();
    let mut queue = db.io_queue();
    let mut progress = suspended_after_first_item(&db, &mut queue, &checks, &ops);
    db.put(b"a", b"newer").unwrap();
    db.flush().unwrap();
    let early = finish(&db, &mut queue, &checks, &ops, &mut progress);
    assert_eq!(
        early.horizon, horizon,
        "the completed prefix's horizon advanced"
    );
    match decide(&db, checks, ops, early).unwrap() {
        Settled::Conflict { conflict, .. } => assert_eq!(conflict.key(), key(b"a")),
        other => panic!("the completed-prefix write escaped validation: {other:?}"),
    }
    assert_eq!(db.get(b"result").unwrap(), None);
}

#[test]
fn a_resumed_check_keeps_the_content_contract_before_a_later_conflict() {
    let dir = TempDir::new().unwrap();
    let db = cold(&dir);
    let snapshot = db.latest_sequence();
    let mut checks = checks(snapshot);
    checks.blind_merges_commute = true;
    checks.exempt = vec![key(b"a")];
    let ops = vec![
        WriteBatchOp::Put {
            key: key(b"a"),
            value: vec![b'x'; 4096],
        },
        WriteBatchOp::Merge {
            key: key(b"b"),
            operand: b"mine".to_vec(),
        },
    ];
    db.put(b"b", &[b'y'; 4096]).unwrap();
    db.flush().unwrap();
    let mut queue = db.io_queue();
    let mut progress = suspended_after_first_item(&db, &mut queue, &checks, &ops);
    assert!(progress.as_ref().unwrap().marker.dirty_before);
    db.put(b"a", b"different").unwrap();
    db.flush().unwrap();
    let early = finish(&db, &mut queue, &checks, &ops, &mut progress);
    let error =
        decide(&db, checks, ops, early).expect_err("the content contract became a later conflict");
    assert!(
        matches!(
            error
                .get_ref()
                .and_then(|cause| cause.downcast_ref::<crate::Error>()),
            Some(crate::Error::ContentMismatch)
        ),
        "wrong refusal: {error}"
    );
    assert_eq!(db.get(b"a").unwrap(), Some(b"different".to_vec()));
}
