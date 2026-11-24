//! Column-family creates and drops racing writers and readers (plan 4.6).
//!
//! The rule under test: a write to a family lands only while the family is
//! live. A write racing a drop either commits before the drop, and the drop's
//! range tombstone deletes it, or is refused with
//! [`Error::InvalidColumnFamily`]. So once a drop returns, nothing is visible
//! in the family's key range, whatever the writers were doing, and every write
//! that reported success is one the tombstone covers.
//!
//! The raw range check reads the engine directly: after a drop no handle can
//! reach the family, so a write that slipped in after its tombstone would be
//! visible only there.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use proptest::prelude::*;

use super::*;

/// Every entry visible at the newest sequence in `cf`'s key range, read below
/// the column-family layer.
fn raw_entries(db: &Db, id: u32) -> Vec<(Vec<u8>, Vec<u8>)> {
    collect_range(
        db.engine.new_iter_latest(),
        Some(&cf_lower_bound(id)),
        Some(&cf_upper_bound(id)),
    )
    .unwrap()
}

fn open(dir: &std::path::Path) -> Db {
    Db::open(dir, Options::default()).unwrap()
}

/// Writers hammer a family through every write path while it is dropped
/// under them; round after round, nothing survives in the dropped range.
///
/// Each writer writes a bounded run, so the drop's turn in the commit ring
/// comes while writes are still arriving.
#[test]
fn a_write_racing_a_drop_is_deleted_or_refused() {
    const WRITES: u64 = 300;
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(open(dir.path()));
    let mut refused_total = 0;
    for round in 0..40 {
        let cf = db.create_column_family(&format!("racing{round}")).unwrap();
        let landed = Arc::new(AtomicUsize::new(0));
        let refused = Arc::new(AtomicUsize::new(0));
        let writers: Vec<_> = (0..6)
            .map(|w| {
                let (db, cf) = (Arc::clone(&db), cf.clone());
                let (landed, refused) = (Arc::clone(&landed), Arc::clone(&refused));
                std::thread::spawn(move || {
                    for i in 0..WRITES {
                        let key = format!("w{w}-{i:04}");
                        let outcome = match i % 16 {
                            0 => db.delete_range_cf(&cf, b"a", key.as_bytes()),
                            n if n % 2 == 0 => {
                                let mut batch = WriteBatch::new();
                                batch.put_cf(&cf, key.as_bytes(), b"v");
                                batch.put(key.as_bytes(), b"default");
                                db.write(batch)
                            }
                            _ => db.put_cf(&cf, key.as_bytes(), b"v"),
                        };
                        match outcome {
                            Ok(()) => landed.fetch_add(1, Ordering::Relaxed),
                            Err(Error::InvalidColumnFamily(_)) => {
                                refused.fetch_add(1, Ordering::Relaxed)
                            }
                            Err(other) => panic!("round {round}: unexpected error {other}"),
                        };
                    }
                })
            })
            .collect();
        // Drop the family part way through the writers' runs.
        while landed.load(Ordering::Relaxed) < 200 {
            std::hint::spin_loop();
        }
        db.drop_column_family(cf.clone()).unwrap();
        assert!(
            raw_entries(&db, cf.id()).is_empty(),
            "round {round}: a write landed in the family after its drop"
        );
        for writer in writers {
            writer.join().unwrap();
        }
        // Every write after the drop was refused, and still nothing landed.
        assert!(raw_entries(&db, cf.id()).is_empty(), "round {round}");
        assert_eq!(
            landed.load(Ordering::Relaxed) + refused.load(Ordering::Relaxed),
            6 * WRITES as usize
        );
        assert!(db.column_family(cf.name()).is_none());
        refused_total += refused.load(Ordering::Relaxed);
    }
    assert!(refused_total > 0, "no write ever raced a drop");
}

/// Readers watching a family see its life in order: once a read is refused
/// for the drop, no later read through that handle succeeds, and a read that
/// succeeds never sees a key written after the drop.
#[test]
fn a_reader_sees_a_family_life_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(open(dir.path()));
    for round in 0..20 {
        let cf = db.create_column_family(&format!("watched{round}")).unwrap();
        db.put_cf(&cf, b"k", b"before").unwrap();
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let (db, cf) = (Arc::clone(&db), cf.clone());
                std::thread::spawn(move || {
                    let mut dropped = false;
                    for _ in 0..2_000 {
                        match db.get_cf(&cf, b"k") {
                            Ok(value) => {
                                assert!(!dropped, "a dropped family came back to life");
                                assert!(
                                    matches!(value.as_deref(), Some(b"before") | None),
                                    "read {value:?}"
                                );
                            }
                            Err(Error::InvalidColumnFamily(_)) => dropped = true,
                            Err(other) => panic!("unexpected error {other}"),
                        }
                    }
                })
            })
            .collect();
        db.drop_column_family(cf.clone()).unwrap();
        // A put after the drop is refused and never seen by any reader.
        assert!(matches!(
            db.put_cf(&cf, b"k", b"after"),
            Err(Error::InvalidColumnFamily(_))
        ));
        for reader in readers {
            reader.join().unwrap();
        }
    }
}

/// Two threads create one name at once: both get the same family, and only
/// one id is spent.
#[test]
fn racing_creates_of_one_name_share_one_family() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(open(dir.path()));
    for round in 0..50 {
        let name = format!("twin{round}");
        let racers: Vec<_> = (0..4)
            .map(|_| {
                let (db, name) = (Arc::clone(&db), name.clone());
                std::thread::spawn(move || db.create_column_family(&name).unwrap())
            })
            .collect();
        let handles: Vec<_> = racers.into_iter().map(|r| r.join().unwrap()).collect();
        assert!(handles.iter().all(|h| h == &handles[0]), "{handles:?}");
        db.put_cf(&handles[0], b"k", b"v").unwrap();
    }
    let mut names = db.list_column_families();
    names.retain(|name| name.starts_with("twin"));
    assert_eq!(names.len(), 50);
}

/// Two drops of one family race: exactly one succeeds.
#[test]
fn racing_drops_of_one_family_drop_it_once() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(open(dir.path()));
    for round in 0..50 {
        let cf = db.create_column_family(&format!("doomed{round}")).unwrap();
        let racers: Vec<_> = (0..3)
            .map(|_| {
                let (db, cf) = (Arc::clone(&db), cf.clone());
                std::thread::spawn(move || db.drop_column_family(cf))
            })
            .collect();
        let outcomes: Vec<_> = racers.into_iter().map(|r| r.join().unwrap()).collect();
        let won = outcomes.iter().filter(|o| o.is_ok()).count();
        assert_eq!(won, 1, "round {round}: {outcomes:?}");
        assert!(
            outcomes
                .iter()
                .all(|o| matches!(o, Ok(()) | Err(Error::InvalidColumnFamily(_))))
        );
    }
}

/// One step of the sequential model.
#[derive(Clone, Debug)]
enum Op {
    Create(u8),
    Drop(u8),
    Put(u8, u8, u8),
    Get(u8, u8),
    Reopen,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0u8..4).prop_map(Op::Create),
        (0u8..4).prop_map(Op::Drop),
        (0u8..4, 0u8..4, any::<u8>()).prop_map(|(f, k, v)| Op::Put(f, k, v)),
        (0u8..4, 0u8..4).prop_map(|(f, k)| Op::Get(f, k)),
        Just(Op::Reopen),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// The column-family set and every family's contents follow a sequential
    /// model through creates, drops, writes, reads and reopens, with handles
    /// kept across drops: a stale handle is always refused, a recreated
    /// family starts empty, and the set survives a reopen.
    #[test]
    fn the_family_set_follows_a_sequential_model(ops in prop::collection::vec(op(), 1..60)) {
        let dir = tempfile::tempdir().unwrap();
        let mut db = open(dir.path());
        // name -> (handle, contents) for every live family in the model.
        let mut live: BTreeMap<u8, (ColumnFamilyHandle, BTreeMap<u8, u8>)> = BTreeMap::new();
        // Every handle ever made, live or stale.
        let mut seen: BTreeMap<u8, Vec<ColumnFamilyHandle>> = BTreeMap::new();
        for op in ops {
            match op {
                Op::Create(f) => {
                    let handle = db.create_column_family(&format!("f{f}")).unwrap();
                    match live.get(&f) {
                        Some((existing, _)) => prop_assert_eq!(&handle, existing),
                        None => {
                            prop_assert!(raw_entries(&db, handle.id).is_empty());
                            live.insert(f, (handle.clone(), BTreeMap::new()));
                        }
                    }
                    seen.entry(f).or_default().push(handle);
                }
                Op::Drop(f) => {
                    for handle in seen.get(&f).into_iter().flatten() {
                        let outcome = db.drop_column_family(handle.clone());
                        match live.get(&f) {
                            Some((current, _)) if current == handle => {
                                prop_assert!(outcome.is_ok());
                                prop_assert!(raw_entries(&db, handle.id).is_empty());
                                live.remove(&f);
                            }
                            _ => prop_assert!(
                                matches!(outcome, Err(Error::InvalidColumnFamily(_))),
                                "{outcome:?}"
                            ),
                        }
                    }
                }
                Op::Put(f, k, v) => {
                    for handle in seen.get(&f).into_iter().flatten() {
                        let outcome = db.put_cf(handle, &[k], &[v]);
                        match live.get_mut(&f) {
                            Some((current, contents)) if current == handle => {
                                prop_assert!(outcome.is_ok());
                                contents.insert(k, v);
                            }
                            _ => prop_assert!(
                                matches!(outcome, Err(Error::InvalidColumnFamily(_))),
                                "{outcome:?}"
                            ),
                        }
                    }
                }
                Op::Get(f, k) => {
                    for handle in seen.get(&f).into_iter().flatten() {
                        let outcome = db.get_cf(handle, &[k]);
                        match live.get(&f) {
                            Some((current, contents)) if current == handle => {
                                prop_assert_eq!(
                                    outcome.unwrap(),
                                    contents.get(&k).map(|v| vec![*v])
                                );
                            }
                            _ => prop_assert!(
                                matches!(outcome, Err(Error::InvalidColumnFamily(_))),
                                "{outcome:?}"
                            ),
                        }
                    }
                }
                Op::Reopen => {
                    drop(db);
                    db = open(dir.path());
                    for (f, (handle, _)) in &live {
                        let reopened = db.column_family(&format!("f{f}"));
                        prop_assert_eq!(reopened.as_ref(), Some(handle));
                    }
                }
            }
            let mut expected: Vec<String> = live.keys().map(|f| format!("f{f}")).collect();
            expected.push(DEFAULT_CF_NAME.to_string());
            expected.sort();
            prop_assert_eq!(db.list_column_families(), expected);
        }
    }
}
