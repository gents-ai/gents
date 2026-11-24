//! A prefix seek over tables of every prefix-bloom form
//! (`engine::prefix_call`): the named form every table is written in, and
//! the two older forms that name no extractor, which only this crate's tests
//! can still write; and over tables an extractor other than the database's
//! built, after a reopen with another one or with none.
//!
//! The rule under test: a prefix seek returns what a full scan of the family
//! returns for that prefix, whichever form each table it meets is in and
//! whichever extractor built it, in level 0 and below it. A table's prefix
//! bloom is asked only by a seek under the extractor it names, so a table
//! is never ruled out for a key it holds.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use proptest::prelude::*;

use super::*;
use crate::engine::block_cache::BlockCache;
use crate::engine::prefix_call::{PrefixForm, writing_form};

/// Through the first `:` of a key, the colon included; a key without one has
/// no prefix. It keeps the trait's `extract_query`, which lets a seek ask
/// only for a prefix that is a whole extracted one.
struct ThroughColon;

impl PrefixExtractor for ThroughColon {
    fn extract<'a>(&self, key: &'a [u8]) -> Option<&'a [u8]> {
        let colon = key.iter().position(|byte| *byte == b':')?;
        Some(&key[..=colon])
    }

    fn name(&self) -> &'static str {
        "ThroughColon"
    }
}

/// The form of each live table's prefix bloom, level by level.
fn forms(db: &Db) -> Vec<Vec<Option<PrefixForm>>> {
    let cache = BlockCache::new(0);
    db.engine
        .current_version()
        .levels
        .iter()
        .map(|level| {
            level
                .iter()
                .map(|table| table.reader.prefix_form(&cache).unwrap())
                .collect()
        })
        .collect()
}

/// Every form among the live tables that have a prefix bloom.
fn forms_in_use(db: &Db) -> Vec<PrefixForm> {
    let mut forms: Vec<_> = forms(db).into_iter().flatten().flatten().collect();
    forms.sort_by_key(|form| form.tag());
    forms.dedup();
    forms
}

/// What `iter` yields from where it stands to where it stops.
fn rest(mut iter: CfIter<'_>) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut rows = Vec::new();
    while iter.valid() {
        rows.push((iter.key().unwrap().to_vec(), iter.value().unwrap().to_vec()));
        iter.next();
    }
    iter.status().unwrap();
    rows
}

/// The rows of `family` (`None` for the default one) a prefix seek yields.
fn seek(db: &Db, family: Option<&ColumnFamilyHandle>, prefix: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut iter = family.map_or_else(|| db.iter(), |cf| db.iter_cf(cf));
    iter.seek_prefix(prefix);
    rest(iter)
}

/// The rows of `family` a full scan yields that start with `prefix`.
fn scan(db: &Db, family: Option<&ColumnFamilyHandle>, prefix: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut iter = family.map_or_else(|| db.iter(), |cf| db.iter_cf(cf));
    iter.seek_to_first();
    let mut rows = rest(iter);
    rows.retain(|(key, _)| key.starts_with(prefix));
    rows
}

/// How many tables a prefix seek in `family` ruled out by their prefix
/// bloom. The seek must also find what a full scan finds.
fn ruled_out(
    db: &Db,
    stats: &Statistics,
    family: Option<&ColumnFamilyHandle>,
    prefix: &[u8],
) -> u64 {
    let before = stats.get_ticker(Ticker::BloomFilterUseful);
    let found = seek(db, family, prefix);
    let ruled_out = stats.get_ticker(Ticker::BloomFilterUseful) - before;
    assert_eq!(found, scan(db, family, prefix));
    ruled_out
}

/// Options for an in-memory database with `extractor`, no compaction worker,
/// and a compaction due once level 0 holds `trigger` tables, and the
/// statistics it reports to.
fn options(
    extractor: Option<Arc<dyn PrefixExtractor>>,
    trigger: usize,
) -> (Options, Arc<Statistics>) {
    let stats = Arc::new(Statistics::new());
    let options = Options::memory()
        .l0_compaction_trigger(trigger)
        .prefix_extractor(extractor)
        .statistics(Some(Arc::clone(&stats)));
    (options, stats)
}

/// A flush writes the named form; only the test switch writes an older one,
/// which a seek reads whole, and a compaction that rewrites it writes the
/// named form.
#[test]
fn only_the_test_switch_writes_an_older_form_and_a_compaction_rewrites_it() {
    let (options, stats) = options(Some(Arc::new(FixedLengthPrefix(4))), 1_000);
    let db = Db::open("prefix-forms", options).unwrap();
    db.put(b"aaaa:1", b"new").unwrap();
    db.put(b"cccc:1", b"new").unwrap();
    db.flush().unwrap();
    assert_eq!(forms(&db)[0], [Some(PrefixForm::Named)]);

    db.put(b"aaaa:2", b"old").unwrap();
    writing_form(PrefixForm::EngineKey, || db.flush()).unwrap();
    db.put(b"aaaa:3", b"older").unwrap();
    writing_form(PrefixForm::CallerKey, || db.flush()).unwrap();
    assert_eq!(
        forms(&db)[0],
        [
            Some(PrefixForm::Named),
            Some(PrefixForm::EngineKey),
            Some(PrefixForm::CallerKey)
        ]
    );

    // The older tables name no extractor, so a seek reads them whatever
    // their blooms hold; the named one rules out what it lacks.
    assert_eq!(ruled_out(&db, &stats, None, b"bbbb"), 1);
    assert_eq!(ruled_out(&db, &stats, None, b"aaaa"), 0);

    // The tables overlap, so the compaction rewrites them into one, named.
    db.compact_range(None, None).wait().unwrap();
    assert!(forms(&db)[0].is_empty());
    assert_eq!(forms_in_use(&db), [PrefixForm::Named]);
    let row = |key: &[u8], value: &[u8]| (key.to_vec(), value.to_vec());
    assert_eq!(
        seek(&db, None, b"aaaa"),
        [
            row(b"aaaa:1", b"new"),
            row(b"aaaa:2", b"old"),
            row(b"aaaa:3", b"older")
        ]
    );
    assert_eq!(ruled_out(&db, &stats, None, b"bbbb"), 1);
    assert_eq!(ruled_out(&db, &stats, None, b"cccc"), 0);
}

/// The database regolith 0.1.9 wrote with an eight-byte extractor
/// (`tests/fixtures/v0_1_9_prefix`; `tests/upgrade_from_0_1_9.rs` says what
/// it holds), copied into memory.
fn written_by_0_1_9() -> Arc<MemEnv> {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v0_1_9_prefix");
    let env = Arc::new(MemEnv::new());
    for file in [
        "MANIFEST",
        "sst/000003.sst",
        "sst/000005.sst",
        "sst/000007.sst",
        "wal/wal_000006.log",
    ] {
        let to = Path::new("/db").join(file);
        env.create_dir_all(to.parent().unwrap()).unwrap();
        env.write(&to, &std::fs::read(fixture.join(file)).unwrap())
            .unwrap();
    }
    env
}

/// The tables a release from before the forms wrote read as the engine-key
/// form, are read whole, and are in the named form once a compaction has
/// rewritten them.
#[test]
fn tables_written_by_0_1_9_are_read_whole_until_a_compaction_rewrites_them() {
    let (options, stats) = options(Some(Arc::new(FixedLengthPrefix(8))), 2);
    let db = Db::open("/db", options.env(written_by_0_1_9())).unwrap();
    let named = db.column_family("named").expect("the family 0.1.9 created");
    assert_eq!(forms(&db)[0], [Some(PrefixForm::EngineKey); 3]);

    assert_eq!(ruled_out(&db, &stats, None, b"tenA0009"), 0);
    assert_eq!(ruled_out(&db, &stats, None, b"tenZ0001"), 0);
    assert_eq!(ruled_out(&db, &stats, Some(&named), b"tenA0009"), 0);
    assert_eq!(seek(&db, None, b"tenA0001").len(), 2);
    assert_eq!(seek(&db, Some(&named), b"tenA0001").len(), 1);

    assert_eq!(db.compact_step().unwrap(), CompactionOutcome::DidWork);
    assert_eq!(forms_in_use(&db), [PrefixForm::Named]);
    assert_eq!(seek(&db, None, b"tenA0001").len(), 2);
    assert_eq!(seek(&db, Some(&named), b"tenA0001").len(), 1);
    // Now the filter is on eight bytes of the caller's key.
    assert_eq!(ruled_out(&db, &stats, None, b"tenA0009"), 1);
    assert_eq!(ruled_out(&db, &stats, None, b"tenA0001"), 0);
}

/// The extractor a case runs with.
#[derive(Clone, Debug)]
enum Extractor {
    Fixed(usize),
    ThroughColon,
}

impl Extractor {
    fn build(&self) -> Arc<dyn PrefixExtractor> {
        match self {
            Self::Fixed(width) => Arc::new(FixedLengthPrefix(*width)),
            Self::ThroughColon => Arc::new(ThroughColon),
        }
    }
}

fn extractor() -> impl Strategy<Value = Extractor> {
    prop_oneof![
        (0usize..6).prop_map(Extractor::Fixed),
        Just(Extractor::ThroughColon),
    ]
}

/// A key, or a seek's prefix, over an alphabet small enough that keys share
/// prefixes and seeks find them.
fn bytes(len: std::ops::Range<usize>) -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(prop_oneof![Just(b'a'), Just(b'b'), Just(b':')], len)
}

/// A write: the family (0 is the default one), the key, and the value, or
/// `None` for a delete.
type Write = (usize, Vec<u8>, Option<u8>);

fn write() -> impl Strategy<Value = Write> {
    (
        0usize..3,
        bytes(1..5),
        prop::option::weighted(0.8, any::<u8>()),
    )
}

/// What happens to the memtable after a batch of writes.
#[derive(Clone, Debug)]
enum After {
    /// Flushed to a table of the form tables are written in.
    Flush,
    /// Flushed to a table of an older form, which names no extractor.
    FlushOlderForm(PrefixForm),
    /// Flushed, then one compaction pass, which may move tables down.
    FlushAndCompact,
    /// Reopened with this extractor, or with none.
    Reopen(Option<Extractor>),
    /// Left in the memtable.
    Nothing,
}

fn after() -> impl Strategy<Value = After> {
    prop_oneof![
        3 => Just(After::Flush),
        1 => Just(After::FlushOlderForm(PrefixForm::EngineKey)),
        1 => Just(After::FlushOlderForm(PrefixForm::CallerKey)),
        2 => Just(After::FlushAndCompact),
        2 => prop::option::of(extractor()).prop_map(After::Reopen),
        1 => Just(After::Nothing),
    ]
}

/// The two named families of `db`.
fn named_families(db: &Db) -> [ColumnFamilyHandle; 2] {
    ["one", "two"].map(|name| db.column_family(name).expect("created at the start"))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// Over random keys, families, extractors, a random split of tables
    /// between the forms, reopens under another extractor or none, and some
    /// tables compacted below level 0 (into one table, or into a table for
    /// each key), a prefix seek returns exactly the rows a full scan has
    /// under that prefix, and they are the rows a model of the writes holds.
    #[test]
    fn a_prefix_seek_over_every_form_and_extractor_returns_what_a_full_scan_returns(
        extractor in extractor(),
        trigger in prop_oneof![Just(2usize), Just(4), Just(1_000)],
        table_size in prop_oneof![Just(1u64), Just(64 << 20)],
        batches in prop::collection::vec((prop::collection::vec(write(), 1..8), after()), 1..7),
        seeks in prop::collection::vec((0usize..3, bytes(0..5)), 1..12),
    ) {
        let env = Arc::new(MemEnv::new());
        let open = |extractor: Option<&Extractor>| {
            let (options, _stats) = options(extractor.map(Extractor::build), trigger);
            let options = options.env(env.clone()).target_file_size(table_size);
            Db::open("prefix-forms", options).unwrap()
        };
        let mut current = Some(extractor);
        let mut db = open(current.as_ref());
        db.create_column_family("one").unwrap();
        db.create_column_family("two").unwrap();
        let mut named = named_families(&db);
        let mut model: BTreeMap<(usize, Vec<u8>), Vec<u8>> = BTreeMap::new();

        for (writes, after) in &batches {
            for (index, key, value) in writes {
                let family = index.checked_sub(1).map(|named_index| &named[named_index]);
                match (family, value) {
                    (None, Some(value)) => db.put(key, &[*value]),
                    (None, None) => db.delete(key),
                    (Some(cf), Some(value)) => db.put_cf(cf, key, &[*value]),
                    (Some(cf), None) => db.delete_cf(cf, key),
                }
                .unwrap();
                match value {
                    Some(value) => model.insert((*index, key.clone()), vec![*value]),
                    None => model.remove(&(*index, key.clone())),
                };
            }
            match after {
                After::Flush => db.flush().unwrap(),
                After::FlushOlderForm(form) => writing_form(*form, || db.flush()).unwrap(),
                After::FlushAndCompact => {
                    db.flush().unwrap();
                    db.compact_step().unwrap();
                }
                After::Reopen(extractor) => {
                    db.close().unwrap();
                    drop(db);
                    current = extractor.clone();
                    db = open(current.as_ref());
                    named = named_families(&db);
                }
                After::Nothing => {}
            }
        }

        for (index, prefix) in &seeks {
            let family = index.checked_sub(1).map(|named_index| &named[named_index]);
            let expected: Vec<(Vec<u8>, Vec<u8>)> = model
                .range((*index, prefix.clone())..)
                .take_while(|((held, key), _)| held == index && key.starts_with(prefix))
                .map(|((_, key), value)| (key.clone(), value.clone()))
                .collect();
            prop_assert_eq!(
                &seek(&db, family, prefix),
                &expected,
                "seek for {:?} in family {} with {:?}; tables {:?}",
                prefix, index, current, forms(&db)
            );
            prop_assert_eq!(&scan(&db, family, prefix), &expected);
        }
    }
}
