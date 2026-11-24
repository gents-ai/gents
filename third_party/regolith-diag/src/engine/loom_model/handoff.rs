//! Models of the two handoffs a write has to survive on its way out of
//! the memtable: the flush that moves a frozen memtable into a version,
//! and the read horizon that decides when a sequence becomes visible.
//!
//! The horizon model drives the production `ReadHorizon` directly. The
//! flush model is a protocol model: the real flush writes an SSTable and
//! opens a reader, neither of which loom can run, so the file is stood
//! in for by a vector while the lock discipline and the order of the two
//! publishing steps are reproduced exactly as
//! `RegolithEngine::flush_frozen_memtable` and `RegolithEngine::lookup` have
//! them - install into the version first, drop the frozen memtable
//! second, and read active, then frozen, then the version.

use loom::sync::atomic::{AtomicU64, Ordering};
use loom::sync::{Arc, Mutex, RwLock};

use super::super::internal_key::user_key_of;
use super::super::lookup_key::LookupKey;
use super::super::memtable::MemTable;
use super::super::read_horizon::ReadHorizon;
use super::{explore, memtable, probe};

/// Sequence the flush models write at.
const SEQ: u64 = 7;

/// A lookup key for `user_key` as of `snapshot_seq`.
fn snapshot(user_key: &[u8], snapshot_seq: u64) -> LookupKey {
    LookupKey::new(0, user_key, snapshot_seq)
}

/// One installed table: the frozen memtable's entries, copied out in
/// internal-key order exactly as the SSTable writer copies them.
type InstalledTable = Vec<(Vec<u8>, Vec<u8>)>;

/// Whether `table` holds a live value for `user_key`.
fn table_holds(table: &InstalledTable, user_key: &[u8]) -> Option<Vec<u8>> {
    table
        .iter()
        .find(|(key, _)| user_key_of(key) == user_key)
        .map(|(_, value)| value.clone())
}

/// Copy a frozen memtable into an installed table.
fn install(memtable: &MemTable) -> InstalledTable {
    let mut table = Vec::new();
    memtable
        .try_for_each_entry(|key, value| {
            table.push((key.to_vec(), value.to_vec()));
            Ok(())
        })
        .expect("collecting into a vector cannot fail");
    table
}

/// Where a read found its answer.
#[derive(Debug, PartialEq, Eq)]
enum Source {
    /// The active memtable.
    Active,
    /// The frozen memtable, before the flush retired it.
    Frozen,
    /// The installed version, after the flush installed it.
    Version,
}

/// The reader's walk: active memtable, then frozen memtables newest
/// first, then the installed version. Mirrors `RegolithEngine::lookup`.
fn read(
    active: &MemTable,
    frozen: &RwLock<Vec<Arc<MemTable>>>,
    version: &Mutex<Vec<InstalledTable>>,
    user_key: &[u8],
) -> Option<(Source, Vec<u8>)> {
    let lk = probe(user_key);
    if let Some((_, Some(value))) = active.get(&lk) {
        return Some((Source::Active, value.as_slice().to_vec()));
    }
    {
        let frozen = frozen.read().expect("frozen");
        for memtable in frozen.iter().rev() {
            if let Some((_, Some(value))) = memtable.get(&lk) {
                return Some((Source::Frozen, value.as_slice().to_vec()));
            }
        }
    }
    let version = version.lock().expect("version");
    version
        .iter()
        .rev()
        .find_map(|table| table_holds(table, lk.prefixed_user_key()))
        .map(|value| (Source::Version, value))
}

/// A key is reachable at every instant of a flush: it is in the frozen
/// memtable, in the installed version, or in both, but never in neither.
///
/// The flush installs the table before it drops the frozen memtable, and
/// the reader walks frozen before version, so the two orders compose:
/// a reader that misses the frozen memtable is reading after the drop,
/// which is after the install it will go on to see.
pub fn a_flush_never_hides_a_key() {
    explore("a_flush_never_hides_a_key", 32, 8, |witness| {
        let (active, frozen, version) = frozen_flush_fixture();

        let flush = {
            let frozen = Arc::clone(&frozen);
            let version = Arc::clone(&version);
            loom::thread::spawn(move || {
                let retiring = Arc::clone(&frozen.read().expect("frozen")[0]);
                version.lock().expect("version").push(install(&retiring));
                frozen.write().expect("frozen").remove(0);
            })
        };
        let reader = {
            let active = Arc::clone(&active);
            let frozen = Arc::clone(&frozen);
            let version = Arc::clone(&version);
            let witness = witness.clone();
            loom::thread::spawn(move || {
                let (source, value) = read(&active, &frozen, &version, b"k")
                    .expect("the flush handoff hid a key that was never deleted");
                assert_eq!(value, b"v");
                // The schedule the ordering exists for: the reader
                // arrived after the frozen memtable was retired.
                if source == Source::Version {
                    witness.record();
                }
            })
        };

        flush.join().expect("flush");
        reader.join().expect("reader");
    });
}

/// Calibration for [`a_flush_never_hides_a_key`]: the same flush with
/// its two publishing steps swapped.
///
/// Dropping the frozen memtable before installing the table opens a
/// window in which the key is in neither place. The model must find it;
/// if it cannot, the model above is passing vacuously and proves nothing
/// about the order the real flush uses.
pub fn a_flush_that_retires_before_it_installs_hides_a_key() {
    explore(
        "a_flush_that_retires_before_it_installs_hides_a_key",
        4,
        1,
        |witness| {
            let (active, frozen, version) = frozen_flush_fixture();

            let flush = {
                let frozen = Arc::clone(&frozen);
                let version = Arc::clone(&version);
                loom::thread::spawn(move || {
                    let retiring = frozen.write().expect("frozen").remove(0);
                    version.lock().expect("version").push(install(&retiring));
                })
            };
            let reader = {
                let active = Arc::clone(&active);
                let frozen = Arc::clone(&frozen);
                let version = Arc::clone(&version);
                let witness = witness.clone();
                loom::thread::spawn(move || {
                    witness.record();
                    let found = read(&active, &frozen, &version, b"k");
                    assert!(found.is_some(), "the key went missing");
                })
            };

            flush.join().expect("flush");
            reader.join().expect("reader");
        },
    );
}

/// A rotated engine: an empty active memtable, one frozen memtable
/// holding `k`, and an empty version.
#[allow(clippy::type_complexity)]
fn frozen_flush_fixture() -> (
    Arc<MemTable>,
    Arc<RwLock<Vec<Arc<MemTable>>>>,
    Arc<Mutex<Vec<InstalledTable>>>,
) {
    let retiring = memtable();
    retiring.put(probe(b"k").prefixed_user_key(), b"v", SEQ);
    (
        Arc::new(memtable()),
        Arc::new(RwLock::new(vec![Arc::new(retiring)])),
        Arc::new(Mutex::new(Vec::new())),
    )
}

/// Invariants H1 and H2: a snapshot that observes sequence `s` observes
/// every memtable insert the publisher of `s` had already made.
///
/// This is the ordering `RegolithEngine::snapshot_seq` and the commit
/// pipeline's publish depend on. Break it and a write that returned `Ok`
/// reads back as absent.
pub fn the_read_horizon_never_outruns_the_memtable() {
    explore(
        "the_read_horizon_never_outruns_the_memtable",
        16,
        4,
        |witness| {
            let mt = Arc::new(memtable());
            let horizon = Arc::new(ReadHorizon::new(0));

            let writer = {
                let mt = Arc::clone(&mt);
                let horizon = Arc::clone(&horizon);
                loom::thread::spawn(move || {
                    mt.put(probe(b"k").prefixed_user_key(), b"v", SEQ);
                    horizon.publish(SEQ);
                })
            };
            let reader = {
                let mt = Arc::clone(&mt);
                let horizon = Arc::clone(&horizon);
                let witness = witness.clone();
                loom::thread::spawn(move || {
                    let visible = horizon.visible();
                    if visible >= SEQ {
                        witness.record();
                        let (seq, value) = mt
                            .get(&snapshot(b"k", visible))
                            .expect("H2: a published sequence is readable");
                        assert_eq!(seq, SEQ);
                        assert_eq!(value.expect("live value").as_slice(), b"v");
                    }
                })
            };

            writer.join().expect("writer");
            reader.join().expect("reader");
        },
    );
}

/// Calibration for [`the_read_horizon_never_outruns_the_memtable`]: the
/// same protocol with both sides relaxed.
///
/// A `Relaxed` publish and a `Relaxed` observe create no happens-before
/// edge, so the reader may observe the new horizon and still read the
/// memtable's older state. The model must find that; if it cannot, it is
/// not distinguishing orderings and the model above proves nothing.
pub fn a_relaxed_read_horizon_outruns_the_memtable() {
    explore(
        "a_relaxed_read_horizon_outruns_the_memtable",
        4,
        1,
        |witness| {
            let mt = Arc::new(memtable());
            let horizon = Arc::new(AtomicU64::new(0));

            let writer = {
                let mt = Arc::clone(&mt);
                let horizon = Arc::clone(&horizon);
                loom::thread::spawn(move || {
                    mt.put(probe(b"k").prefixed_user_key(), b"v", SEQ);
                    horizon.store(SEQ, Ordering::Relaxed);
                })
            };
            let reader = {
                let mt = Arc::clone(&mt);
                let horizon = Arc::clone(&horizon);
                let witness = witness.clone();
                loom::thread::spawn(move || {
                    let visible = horizon.load(Ordering::Relaxed);
                    if visible >= SEQ {
                        witness.record();
                        assert!(
                            mt.get(&snapshot(b"k", visible)).is_some(),
                            "a relaxed horizon advertised a sequence the reader cannot see"
                        );
                    }
                })
            };

            writer.join().expect("writer");
            reader.join().expect("reader");
        },
    );
}
