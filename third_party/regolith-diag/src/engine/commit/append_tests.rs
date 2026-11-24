//! The ordered step numbers appends against the view plus the appends of
//! the group so far, after validation, and keeps nothing of a group that
//! failed. Each test is a configuration of `CommitOrderedAppend.tla`: the
//! green runs, and the reds a mutant of the numbering would show.

use std::collections::BTreeMap;

use tempfile::TempDir;

use super::super::{EngineOptions, ValidationSet, wal::fault};
use super::append::AppendOrder;
use super::*;
use crate::column_family::{DEFAULT_CF_ID, prefix_key};
use crate::{Access, LogLayout, WriteBatchOp};

/// Entries at `<name>/<20 decimal digits>`, head at `<name>-head`.
struct Flat {
    name: &'static str,
    head: String,
    max: usize,
}

impl Flat {
    fn named(name: &'static str) -> Arc<dyn LogLayout> {
        Arc::new(Self {
            name,
            head: format!("{name}-head"),
            max: name.len() + 1 + 20,
        })
    }
}

impl LogLayout for Flat {
    fn head_key(&self) -> &[u8] {
        self.head.as_bytes()
    }

    fn entry_key(&self, position: u64, out: &mut Vec<u8>) {
        out.extend_from_slice(format!("{}/{position:020}", self.name).as_bytes());
    }

    fn max_entry_key_len(&self) -> usize {
        self.max
    }
}

/// A layout that builds longer keys than it declared.
struct Overrun;

impl LogLayout for Overrun {
    fn head_key(&self) -> &[u8] {
        b"overrun-head"
    }

    fn entry_key(&self, position: u64, out: &mut Vec<u8>) {
        out.extend_from_slice(format!("overrun/{position:020}").as_bytes());
    }

    fn max_entry_key_len(&self) -> usize {
        4
    }
}

fn open() -> (TempDir, Arc<RegolithEngine>) {
    let dir = TempDir::new().unwrap();
    let engine = RegolithEngine::open(dir.path(), EngineOptions::default()).unwrap();
    (dir, engine)
}

fn key_of(name: &[u8]) -> Vec<u8> {
    prefix_key(DEFAULT_CF_ID, name)
}

fn entry(log: &Arc<dyn LogLayout>, bytes: &[u8], once: Option<&[u8]>) -> PendingAppend {
    PendingAppend {
        log: Arc::clone(log),
        entry: bytes.to_vec(),
        once_key: once.map(<[u8]>::to_vec),
    }
}

fn position(n: u64) -> Vec<u8> {
    n.to_be_bytes().to_vec()
}

fn put_now(engine: &RegolithEngine, name: &[u8], value: &[u8]) {
    let op = WriteBatchOp::Put {
        key: key_of(name),
        value: value.to_vec(),
    };
    engine
        .apply_batch(vec![op], DurabilityMode::Eventual, false)
        .unwrap();
}

fn read_now(engine: &RegolithEngine, name: &[u8]) -> Option<Vec<u8>> {
    engine.get(&key_of(name), u64::MAX).unwrap()
}

/// The puts in `ops`, as `(key without prefix, value)` in order.
fn puts(ops: &[WriteBatchOp]) -> Vec<(String, Vec<u8>)> {
    ops.iter()
        .map(|op| match op {
            WriteBatchOp::Put { key, value } => {
                (String::from_utf8(key[4..].to_vec()).unwrap(), value.clone())
            }
            other => panic!("an append writes only puts, got {other:?}"),
        })
        .collect()
}

/// Number one member's appends against `order` and the current view.
fn member(
    engine: &RegolithEngine,
    order: &mut AppendOrder,
    appends: Vec<PendingAppend>,
    ops: &mut Vec<WriteBatchOp>,
) -> io::Result<()> {
    order.number(engine, &engine.view.load(), appends, ops)
}

fn commit(
    engine: &RegolithEngine,
    checks: ValidationSet,
    appends: Vec<PendingAppend>,
    durability: DurabilityMode,
) -> io::Result<CommitOutcome> {
    engine.commit_optimistic(
        checks,
        BTreeMap::new(),
        Vec::new(),
        Vec::new(),
        appends,
        durability,
    )
}

fn no_checks(engine: &RegolithEngine) -> ValidationSet {
    ValidationSet {
        reads: Vec::new(),
        writes_at: Some(engine.snapshot_seq()),
        blind_merges_commute: false,
        exempt: Vec::new(),
        ranges: Vec::new(),
    }
}

#[test]
fn positions_run_on_from_the_head_in_the_view_and_the_head_key_is_written_once() {
    let (_dir, engine) = open();
    let log = Flat::named("j");
    put_now(&engine, b"j-head", &position(5));

    let mut order = AppendOrder::default();
    let mut ops = Vec::new();
    member(
        &engine,
        &mut order,
        vec![entry(&log, b"x", None), entry(&log, b"y", None)],
        &mut ops,
    )
    .unwrap();

    assert_eq!(
        puts(&ops),
        [
            ("j/00000000000000000006".to_owned(), b"x".to_vec()),
            ("j/00000000000000000007".to_owned(), b"y".to_vec()),
            ("j-head".to_owned(), position(7)),
        ]
    );
}

#[test]
fn an_empty_log_starts_at_one() {
    let (_dir, engine) = open();
    let log = Flat::named("j");
    let mut order = AppendOrder::default();
    let mut ops = Vec::new();
    member(&engine, &mut order, vec![entry(&log, b"x", None)], &mut ops).unwrap();
    assert_eq!(
        puts(&ops),
        [
            ("j/00000000000000000001".to_owned(), b"x".to_vec()),
            ("j-head".to_owned(), position(1)),
        ]
    );
}

/// RED ViewOnlyHead: a member that read the head from the view alone would
/// take the position its predecessor already took. Each member writes the
/// head it leaves in its own record.
#[test]
fn a_later_member_of_a_group_counts_from_the_head_an_earlier_member_left() {
    let (_dir, engine) = open();
    let log = Flat::named("j");
    let mut order = AppendOrder::default();
    let mut first = Vec::new();
    let mut second = Vec::new();
    // Both members number against the same view, which holds neither.
    member(
        &engine,
        &mut order,
        vec![entry(&log, b"a", None)],
        &mut first,
    )
    .unwrap();
    member(
        &engine,
        &mut order,
        vec![entry(&log, b"b", None), entry(&log, b"c", None)],
        &mut second,
    )
    .unwrap();

    assert_eq!(
        puts(&first),
        [
            ("j/00000000000000000001".to_owned(), b"a".to_vec()),
            ("j-head".to_owned(), position(1)),
        ]
    );
    assert_eq!(
        puts(&second),
        [
            ("j/00000000000000000002".to_owned(), b"b".to_vec()),
            ("j/00000000000000000003".to_owned(), b"c".to_vec()),
            ("j-head".to_owned(), position(3)),
        ],
        "the head key holds the last position the member assigned"
    );
}

/// A member that fails part way through its appends gives back every
/// position and once key it took, so the member after it numbers as if it
/// had never been there and the log has no hole.
#[test]
fn a_member_that_fails_gives_back_what_it_took() {
    let (_dir, engine) = open();
    let log = Flat::named("j");
    let overrun: Arc<dyn LogLayout> = Arc::new(Overrun);
    let mut order = AppendOrder::default();
    let err = member(
        &engine,
        &mut order,
        vec![
            entry(&log, b"a", Some(b"o")),
            entry(&log, b"b", None),
            entry(&overrun, b"too long", None),
        ],
        &mut Vec::new(),
    )
    .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

    let mut ops = Vec::new();
    member(
        &engine,
        &mut order,
        vec![entry(&log, b"c", Some(b"o"))],
        &mut ops,
    )
    .unwrap();
    assert_eq!(
        puts(&ops),
        [
            ("j/00000000000000000001".to_owned(), b"c".to_vec()),
            ("o".to_owned(), position(1)),
            ("j-head".to_owned(), position(1)),
        ],
        "the failed member's positions and once key were given back"
    );
}

/// RED OnceFromView: a once key an earlier member set is not in the view yet.
#[test]
fn a_once_key_an_earlier_member_set_is_seen_by_a_later_one() {
    let (_dir, engine) = open();
    let log = Flat::named("j");
    let mut order = AppendOrder::default();
    let (mut first, mut second) = (Vec::new(), Vec::new());
    member(
        &engine,
        &mut order,
        vec![entry(&log, b"a", Some(b"o"))],
        &mut first,
    )
    .unwrap();
    member(
        &engine,
        &mut order,
        vec![entry(&log, b"b", Some(b"o")), entry(&log, b"c", None)],
        &mut second,
    )
    .unwrap();

    assert_eq!(
        puts(&first),
        [
            ("j/00000000000000000001".to_owned(), b"a".to_vec()),
            ("o".to_owned(), position(1)),
            ("j-head".to_owned(), position(1)),
        ]
    );
    assert_eq!(
        puts(&second),
        [
            ("j/00000000000000000002".to_owned(), b"c".to_vec()),
            ("j-head".to_owned(), position(2)),
        ],
        "the shared once key appends nothing; the append beside it takes the next position"
    );
}

#[test]
fn one_once_key_appended_twice_in_one_member_yields_one_entry() {
    let (_dir, engine) = open();
    let log = Flat::named("j");
    let mut order = AppendOrder::default();
    let mut ops = Vec::new();
    member(
        &engine,
        &mut order,
        vec![
            entry(&log, b"first", Some(b"o")),
            entry(&log, b"second", Some(b"o")),
        ],
        &mut ops,
    )
    .unwrap();
    assert_eq!(
        puts(&ops),
        [
            ("j/00000000000000000001".to_owned(), b"first".to_vec()),
            ("o".to_owned(), position(1)),
            ("j-head".to_owned(), position(1)),
        ]
    );
}

#[test]
fn a_once_key_the_view_holds_appends_nothing_and_leaves_the_head_alone() {
    let (_dir, engine) = open();
    let log = Flat::named("j");
    put_now(&engine, b"o", &position(4));
    put_now(&engine, b"j-head", &position(9));

    let mut order = AppendOrder::default();
    let mut ops = Vec::new();
    member(
        &engine,
        &mut order,
        vec![entry(&log, b"x", Some(b"o"))],
        &mut ops,
    )
    .unwrap();

    assert!(
        ops.is_empty(),
        "nothing is written for a duplicate: {ops:?}"
    );
}

#[test]
fn an_order_that_assigned_nothing_writes_no_head() {
    let (_dir, engine) = open();
    let mut order = AppendOrder::default();
    let mut ops = Vec::new();
    member(&engine, &mut order, Vec::new(), &mut ops).unwrap();
    assert!(ops.is_empty());
}

#[test]
fn logs_are_numbered_apart_and_each_head_is_written_once() {
    let (_dir, engine) = open();
    let (a, b) = (Flat::named("a"), Flat::named("b"));
    put_now(&engine, b"b-head", &position(10));
    let mut order = AppendOrder::default();
    let mut ops = Vec::new();
    member(
        &engine,
        &mut order,
        vec![
            entry(&a, b"a1", None),
            entry(&b, b"b1", None),
            entry(&a, b"a2", None),
        ],
        &mut ops,
    )
    .unwrap();
    assert_eq!(
        puts(&ops),
        [
            ("a/00000000000000000001".to_owned(), b"a1".to_vec()),
            ("b/00000000000000000011".to_owned(), b"b1".to_vec()),
            ("a/00000000000000000002".to_owned(), b"a2".to_vec()),
            ("a-head".to_owned(), position(2)),
            ("b-head".to_owned(), position(11)),
        ]
    );
}

/// RED HeadCache: a new order counts from the view, so what a failed group
/// numbered leaves no hole.
#[test]
fn a_new_order_reads_the_view_again() {
    let (_dir, engine) = open();
    let log = Flat::named("j");
    for _ in 0..2 {
        let mut order = AppendOrder::default();
        let mut ops = Vec::new();
        member(&engine, &mut order, vec![entry(&log, b"x", None)], &mut ops).unwrap();
        assert_eq!(
            puts(&ops)[0].0,
            "j/00000000000000000001",
            "a group that never published took nothing"
        );
    }
}

#[test]
fn a_head_or_once_key_that_is_not_eight_bytes_is_refused_by_name_of_its_kind() {
    let (_dir, engine) = open();
    let log = Flat::named("j");
    put_now(&engine, b"j-head", b"short");
    let err = member(
        &engine,
        &mut AppendOrder::default(),
        vec![entry(&log, b"x", None)],
        &mut Vec::new(),
    )
    .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains("5 bytes"), "{err}");
    assert!(!err.to_string().contains("short"), "no key bytes: {err}");

    let (_dir, engine) = open();
    put_now(&engine, b"o", &[0u8; 9]);
    let err = member(
        &engine,
        &mut AppendOrder::default(),
        vec![entry(&log, b"x", Some(b"o"))],
        &mut Vec::new(),
    )
    .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn a_head_at_the_largest_position_leaves_no_position_to_give() {
    let (_dir, engine) = open();
    let log = Flat::named("j");
    put_now(&engine, b"j-head", &position(u64::MAX));
    let err = member(
        &engine,
        &mut AppendOrder::default(),
        vec![entry(&log, b"x", None)],
        &mut Vec::new(),
    )
    .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn an_entry_key_longer_than_the_layout_declared_fails_the_commit_and_applies_nothing() {
    let (_dir, engine) = open();
    let log: Arc<dyn LogLayout> = Arc::new(Overrun);
    let err = commit(
        &engine,
        no_checks(&engine),
        vec![entry(&log, b"x", None)],
        DurabilityMode::Eventual,
    )
    .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(read_now(&engine, b"overrun-head"), None);
}

/// RED AssignBeforeValidation: a commit that fails validation takes no
/// position, so the next commit takes the one it would have had.
#[test]
fn a_commit_that_fails_validation_takes_no_position() {
    let (_dir, engine) = open();
    let log = Flat::named("j");
    let observed = engine.snapshot_seq();
    put_now(&engine, b"stale", b"newer");
    let stale = ValidationSet {
        reads: vec![crate::engine::ConflictKey {
            key: key_of(b"stale"),
            observed_seq: observed,
            found: false,
            access: Access::Read,
            rule: crate::engine::ReadRule::Seq,
        }],
        writes_at: Some(observed),
        blind_merges_commute: false,
        exempt: Vec::new(),
        ranges: Vec::new(),
    };

    let lost = commit(
        &engine,
        stale,
        vec![entry(&log, b"lost", None)],
        DurabilityMode::Eventual,
    )
    .unwrap();
    assert!(matches!(lost, CommitOutcome::Conflict(_)));
    assert_eq!(read_now(&engine, b"j-head"), None);

    let won = commit(
        &engine,
        no_checks(&engine),
        vec![entry(&log, b"won", None)],
        DurabilityMode::Eventual,
    )
    .unwrap();
    assert!(matches!(won, CommitOutcome::Ok { .. }));
    assert_eq!(read_now(&engine, b"j-head"), Some(position(1)));
    assert_eq!(
        read_now(&engine, "j/00000000000000000001".as_bytes()),
        Some(b"won".to_vec())
    );
}

/// A WAL error after the positions were assigned: the group fails, its
/// positions are gone, and the next group reassigns from the view.
#[test]
fn a_group_that_fails_after_assignment_leaves_no_trace_and_the_next_group_reassigns() {
    let dir = TempDir::new().unwrap();
    let engine = RegolithEngine::open(dir.path(), EngineOptions::default()).unwrap();
    let log = Flat::named("j");
    let ok = commit(
        &engine,
        no_checks(&engine),
        vec![entry(&log, b"one", None)],
        DurabilityMode::Immediate,
    )
    .unwrap();
    assert!(matches!(ok, CommitOutcome::Ok { .. }));

    fault::arm_sync_failure(dir.path());
    let failed = commit(
        &engine,
        no_checks(&engine),
        vec![entry(&log, b"lost-a", None), entry(&log, b"lost-b", None)],
        DurabilityMode::Immediate,
    );
    fault::disarm_sync_failure(dir.path());
    assert!(failed.is_err(), "the injected sync failure fails the group");

    assert_eq!(read_now(&engine, b"j-head"), Some(position(1)));
    assert_eq!(read_now(&engine, "j/00000000000000000002".as_bytes()), None);

    let next = commit(
        &engine,
        no_checks(&engine),
        vec![entry(&log, b"two", None)],
        DurabilityMode::Immediate,
    )
    .unwrap();
    assert!(matches!(next, CommitOutcome::Ok { .. }));
    assert_eq!(read_now(&engine, b"j-head"), Some(position(2)));
    assert_eq!(
        read_now(&engine, "j/00000000000000000002".as_bytes()),
        Some(b"two".to_vec())
    );
}

#[test]
fn a_commit_of_appends_alone_publishes_its_writes_at_one_sequence() {
    let (_dir, engine) = open();
    let log = Flat::named("j");
    let before = engine.snapshot_seq();
    let outcome = commit(
        &engine,
        no_checks(&engine),
        vec![entry(&log, b"a", Some(b"o")), entry(&log, b"b", None)],
        DurabilityMode::Eventual,
    )
    .unwrap();
    // entry a, once key, entry b, head.
    assert!(matches!(outcome, CommitOutcome::Ok { seq: Some(seq) } if seq == before + 4));
    assert_eq!(engine.snapshot_seq(), before + 4);
}

#[test]
fn a_commit_without_an_append_takes_no_head_read_and_no_extra_write() {
    let (_dir, engine) = open();
    let before = engine.snapshot_seq();
    let mut point_ops = BTreeMap::new();
    point_ops.insert(key_of(b"k"), Some(b"v".to_vec()));
    let outcome = engine
        .commit_optimistic(
            no_checks(&engine),
            point_ops,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            DurabilityMode::Eventual,
        )
        .unwrap();
    assert!(matches!(outcome, CommitOutcome::Ok { seq: Some(seq) } if seq == before + 1));
}

#[test]
fn appends_beyond_the_value_limit_are_refused_before_the_pipeline() {
    let dir = TempDir::new().unwrap();
    let engine = RegolithEngine::open(
        dir.path(),
        EngineOptions {
            max_value_size: 8,
            ..EngineOptions::default()
        },
    )
    .unwrap();
    let log = Flat::named("j");
    let err = commit(
        &engine,
        no_checks(&engine),
        vec![entry(&log, &[0u8; 9], None)],
        DurabilityMode::Eventual,
    )
    .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(engine.snapshot_seq(), 0, "nothing reached the pipeline");
}

#[test]
fn a_layout_whose_keys_may_pass_the_key_limit_is_refused_before_the_pipeline() {
    let dir = TempDir::new().unwrap();
    let engine = RegolithEngine::open(
        dir.path(),
        EngineOptions {
            max_key_size: 16,
            ..EngineOptions::default()
        },
    )
    .unwrap();
    // Declares 22 bytes of entry key, over the limit of 16.
    let log = Flat::named("j");
    let err = commit(
        &engine,
        no_checks(&engine),
        vec![entry(&log, b"x", None)],
        DurabilityMode::Eventual,
    )
    .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains("max_key_size"), "{err}");
}

/// Declares an entry key far longer than any it builds, as a bound.
struct Wide;

impl LogLayout for Wide {
    fn head_key(&self) -> &[u8] {
        b"wide-head"
    }

    fn entry_key(&self, position: u64, out: &mut Vec<u8>) {
        out.extend_from_slice(format!("wide/{position}").as_bytes());
    }

    fn max_entry_key_len(&self) -> usize {
        700 * 1024 * 1024
    }
}

#[test]
fn the_appended_record_is_counted_against_the_wal_limit_before_the_commit_waits() {
    let dir = TempDir::new().unwrap();
    let engine = RegolithEngine::open(
        dir.path(),
        EngineOptions {
            max_key_size: u32::MAX as usize,
            ..EngineOptions::default()
        },
    )
    .unwrap();
    let log: Arc<dyn LogLayout> = Arc::new(Wide);
    // Each entry key counts at its declared bound of 700 MiB, so two do not
    // fit one record of at most 1 GiB, and nothing is allocated for them.
    let one = vec![entry(&log, b"a", None)];
    engine.validate_append_sizes(&[], &one).unwrap();
    let two = vec![entry(&log, b"a", None), entry(&log, b"b", None)];
    let err = engine.validate_append_sizes(&[], &two).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains("too large"), "{err}");
}
