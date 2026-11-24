//! What a transactional scan records about the keys it walked.
//!
//! A scan does not add a read-set cell per key it yields: that would hold
//! memory and cost commit work per key. It records each unbroken stretch
//! of snapshot keys it yields as one [`ScanRun`], the first and the last
//! key of the stretch, CF-prefixed and inclusive. A stretch ends where the
//! walk passes an entry of the transaction's own write buffer, or a key
//! the transaction already reads past its begin snapshot, since neither
//! was read from the snapshot.
//!
//! A cursor extends its stretch as it goes and publishes how far it got at
//! the end of every page, so a commit between pages sees the stretch as read
//! so far, not as reaching the end of the keyspace.
//!
//! Invariant: every key inside a closed run was observed at the begin
//! snapshot, as a value the scan yielded or as an absence the walk passed
//! over. A run whose cursor published nothing yet has no last key and
//! reaches the end of the keyspace in the direction it walked, so it still
//! covers at least what was walked.
//!
//! A run may name the parts of the values the scan used
//! ([`crate::ScanCheck::Parts`]). A key the commit merges into inside such a
//! run is then validated over those parts only; any other write of a key
//! inside it, and a key inside a run that names no parts, is validated in
//! whole.
//!
//! At commit the runs are merged into disjoint intervals (sorted by lower
//! bound, overlapping or touching ones coalesced, runs of one parts set
//! together), and every key the commit validates that lies inside one is
//! validated as a read made no later than the begin snapshot.

use std::collections::BTreeMap;
use std::sync::Arc;

use kovan::Atom;

use crate::Access;
use crate::engine::{ConflictKey, ReadRule};

/// One stretch of snapshot keys a scan yielded, recorded once however many
/// keys it holds.
pub(super) struct ScanRun {
    /// The first key the stretch yielded, CF-prefixed: its lowest key
    /// walking forward, its highest walking in reverse.
    first: Vec<u8>,
    /// The last key it yielded, published when a page ends and when the
    /// stretch closes. `None` means nothing was published, so the stretch is
    /// taken to reach the end of the keyspace in the direction it walked.
    ///
    /// The cursor that owns the stretch's [`OpenRun`] is the one writer; a
    /// commit reads it, possibly from another thread while the cursor is
    /// between pages. Each publication swaps a whole key in, so a reader
    /// sees one the cursor published, never part of one, and neither waits.
    last: Atom<Option<Vec<u8>>>,
    reverse: bool,
    /// The parts the scan used, sorted, for a stretch of `ScanCheck::Parts`.
    parts: Option<Arc<[u32]>>,
}

/// The stretch a cursor is extending. Dropping it closes the stretch at the
/// last key it was extended to.
///
/// It holds no borrow of the transaction, so closing a stretch never
/// touches the transaction: the record was registered when the stretch
/// began, and a stream dropped after its transaction resolved reaches only
/// this `Arc`.
pub(super) struct OpenRun {
    run: Arc<ScanRun>,
    last: Vec<u8>,
}

impl ScanRun {
    /// The stretch's first key and, once published, its last, CF-prefixed.
    pub(super) fn bounds(&self) -> (&[u8], Option<Vec<u8>>) {
        (&self.first, self.last.load_clone())
    }
}

impl OpenRun {
    /// Begin a stretch at `key` (CF-prefixed). Returns the record for the
    /// transaction to register and the handle that extends and closes it.
    pub(super) fn start(
        key: &[u8],
        reverse: bool,
        parts: Option<Arc<[u32]>>,
    ) -> (Arc<ScanRun>, Self) {
        let run = Arc::new(ScanRun {
            first: key.to_vec(),
            last: Atom::new(None),
            reverse,
            parts,
        });
        (
            Arc::clone(&run),
            Self {
                run,
                last: key.to_vec(),
            },
        )
    }

    /// Extend the stretch to `key` (CF-prefixed), the next key in its
    /// direction.
    pub(super) fn extend(&mut self, key: &[u8]) {
        self.last.clear();
        self.last.extend_from_slice(key);
    }

    /// Let a commit see how far the stretch has been extended.
    pub(super) fn publish(&self) {
        // A page that did not extend the stretch has nothing new to say.
        let last = self.last.as_slice();
        if self
            .run
            .last
            .peek(|published| published.as_deref() == Some(last))
        {
            return;
        }
        self.run.last.store(Some(self.last.clone()));
    }
}

impl Drop for OpenRun {
    fn drop(&mut self) {
        self.publish();
    }
}

/// A key interval, inclusive at both ends; `None` leaves that side
/// unbounded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Walked<'a> {
    lo: Option<&'a [u8]>,
    hi: Option<&'a [u8]>,
}

/// The interval `run` covers, given the last key it published.
fn span<'a>(run: &'a ScanRun, last: Option<&'a [u8]>) -> Walked<'a> {
    let first = Some(run.first.as_slice());
    if run.reverse {
        Walked {
            lo: last,
            hi: first,
        }
    } else {
        Walked {
            lo: first,
            hi: last,
        }
    }
}

/// The intervals as disjoint ones sorted by lower bound.
fn merge(mut spans: Vec<Walked<'_>>) -> Vec<Walked<'_>> {
    // `None < Some`, so a span unbounded below sorts first.
    spans.sort_unstable_by(|a, b| a.lo.cmp(&b.lo));
    let mut merged: Vec<Walked<'_>> = Vec::with_capacity(spans.len());
    for span in spans {
        match merged.last_mut() {
            Some(prev) if reaches(prev.hi, span.lo) => {
                prev.hi = match (prev.hi, span.hi) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    _ => None,
                };
            }
            _ => merged.push(span),
        }
    }
    merged
}

/// Whether an interval ending at `hi` overlaps or touches one starting at
/// `lo` that sorts at or after it.
fn reaches(hi: Option<&[u8]>, lo: Option<&[u8]>) -> bool {
    match (hi, lo) {
        (Some(hi), Some(lo)) => lo <= hi,
        _ => true,
    }
}

/// Whether `key` lies inside one of `walked`, which [`merge`] built.
fn covers(walked: &[Walked<'_>], key: &[u8]) -> bool {
    let after = walked.partition_point(|span| span.lo.is_none_or(|lo| lo <= key));
    after > 0 && walked[after - 1].hi.is_none_or(|hi| key <= hi)
}

/// The intervals the runs walked, split by what they use of a value: whole
/// values, or one parts set each. A run that names no parts reads nothing it
/// decided on and is left out.
struct Walks<'a> {
    whole: Vec<Walked<'a>>,
    projected: Vec<(&'a [u32], Vec<Walked<'a>>)>,
}

impl<'a> Walks<'a> {
    fn of(runs: &'a [Arc<ScanRun>], lasts: &'a [Option<Vec<u8>>]) -> Self {
        let mut whole = Vec::new();
        let mut projected: Vec<(&[u32], Vec<Walked<'_>>)> = Vec::new();
        for (run, last) in runs.iter().zip(lasts) {
            let span = span(run, last.as_deref());
            match run.parts.as_deref() {
                None => whole.push(span),
                Some([]) => {}
                Some(parts) => match projected.iter_mut().find(|(set, _)| *set == parts) {
                    Some((_, spans)) => spans.push(span),
                    None => projected.push((parts, vec![span])),
                },
            }
        }
        Self {
            whole: merge(whole),
            projected: projected
                .into_iter()
                .map(|(parts, spans)| (parts, merge(spans)))
                .collect(),
        }
    }

    /// Whether any scan walked `key`.
    fn covers(&self, key: &[u8]) -> bool {
        covers(&self.whole, key) || self.projected.iter().any(|(_, spans)| covers(spans, key))
    }

    /// The parts the scans that walked `key` used, or `None` when one used
    /// the whole value.
    fn parts_of(&self, key: &[u8]) -> Option<Vec<u32>> {
        if covers(&self.whole, key) {
            return None;
        }
        let mut parts: Vec<u32> = self
            .projected
            .iter()
            .filter(|(_, spans)| covers(spans, key))
            .flat_map(|(parts, _)| parts.iter().copied())
            .collect();
        parts.sort_unstable();
        parts.dedup();
        Some(parts)
    }
}

/// A key read by parts and merged into, which a scan also walked, was read
/// by whatever the scan used of it too: the whole value if any scan used that,
/// else the union of the parts. A key that is not merged into is not widened:
/// the yielded keys of a scan are not validated on their own.
fn widen_projected_reads(
    reads: &mut [ConflictKey],
    walks: &Walks<'_>,
    merges: &[(Vec<u8>, Vec<u8>)],
    full: &ReadRule,
) {
    if !reads
        .iter()
        .any(|read| matches!(read.rule, ReadRule::Parts(_)))
    {
        return;
    }
    let mut merged: Vec<&[u8]> = merges.iter().map(|(key, _)| key.as_slice()).collect();
    merged.sort_unstable();
    for read in reads.iter_mut() {
        let ReadRule::Parts(own) = &read.rule else {
            continue;
        };
        if !walks.covers(&read.key) || merged.binary_search(&read.key.as_slice()).is_err() {
            continue;
        }
        read.rule = match walks.parts_of(&read.key) {
            None => full.clone(),
            Some(mut parts) => {
                parts.extend_from_slice(own);
                parts.sort_unstable();
                parts.dedup();
                ReadRule::Parts(parts.into())
            }
        };
    }
}

/// Anchor every key in `reads` a scan walked no later than `begin_seq`, and
/// append every written or merged key a scan walked that `reads` does not
/// already hold, as a read at `begin_seq`.
///
/// This is what [`crate::Transaction::get`] at the begin snapshot followed
/// by the write would have left: a read that is never elided as an
/// idempotent write, anchored at the earliest sequence the key was
/// observed at. It is validated by `full` (the rule of a read of a whole
/// value), except a key only merged into that every scan walked by parts:
/// that one is validated over the union of those parts. A key the
/// transaction puts or deletes is replaced whole, so it takes `full`
/// whatever the scans used. `reads` must be sorted by key with no
/// duplicates on entry, and is again on return. A no-op when `runs` is empty,
/// so a transaction that ran no scan pays nothing here.
pub(super) fn cover(
    reads: &mut Vec<ConflictKey>,
    runs: &[Arc<ScanRun>],
    writes: &BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    merges: &[(Vec<u8>, Vec<u8>)],
    begin_seq: u64,
    full: &ReadRule,
) {
    if runs.is_empty() {
        return;
    }
    let lasts: Vec<Option<Vec<u8>>> = runs.iter().map(|run| run.last.load_clone()).collect();
    let walks = Walks::of(runs, &lasts);
    for read in reads.iter_mut() {
        if walks.covers(&read.key) {
            read.observed_seq = read.observed_seq.min(begin_seq);
        }
    }
    widen_projected_reads(reads, &walks, merges, full);
    let mut added: Vec<ConflictKey> = writes
        .keys()
        .chain(merges.iter().map(|(key, _)| key))
        .filter(|key| {
            walks.covers(key)
                && reads
                    .binary_search_by(|read| read.key.as_slice().cmp(key))
                    .is_err()
        })
        .map(|key| ConflictKey {
            key: key.clone(),
            observed_seq: begin_seq,
            found: false,
            access: Access::ScannedThenWrote,
            rule: match walks.parts_of(key) {
                Some(parts) if !writes.contains_key(key) => ReadRule::Parts(parts.into()),
                _ => full.clone(),
            },
        })
        .collect();
    if added.is_empty() {
        return;
    }
    added.sort_unstable_by(|a, b| a.key.cmp(&b.key));
    added.dedup_by(|later, first| later.key == first.key);
    reads.append(&mut added);
    // Two sorted runs back to back: the stable sort merges them in one pass.
    reads.sort_by(|a, b| a.key.cmp(&b.key));
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use super::*;
    use proptest::prelude::*;
    use tempfile::TempDir;

    /// A run as `(first, last, reverse)` with the CF prefix stripped.
    type Span<'a> = (&'a [u8], Option<&'a [u8]>, bool);

    fn run(first: &[u8], last: Option<&[u8]>, reverse: bool) -> Arc<ScanRun> {
        run_by(first, last, reverse, None)
    }

    fn run_by(
        first: &[u8],
        last: Option<&[u8]>,
        reverse: bool,
        parts: Option<&[u32]>,
    ) -> Arc<ScanRun> {
        Arc::new(ScanRun {
            first: first.to_vec(),
            last: Atom::new(last.map(<[u8]>::to_vec)),
            reverse,
            parts: parts.map(Arc::from),
        })
    }

    fn lasts_of(runs: &[Arc<ScanRun>]) -> Vec<Option<Vec<u8>>> {
        runs.iter().map(|run| run.bounds().1).collect()
    }

    fn read(key: &[u8], observed_seq: u64, rule: ReadRule) -> ConflictKey {
        ConflictKey {
            key: key.to_vec(),
            observed_seq,
            found: false,
            access: Access::Read,
            rule,
        }
    }

    /// A commit reading a stretch while its cursor extends and publishes it
    /// on another thread: every last key read is one the cursor published,
    /// whole, and never an earlier one than was read before.
    #[test]
    fn a_stretch_read_while_its_cursor_publishes_is_never_torn() {
        // The `i`th key: `i + 1` bytes, each of them `i`.
        let key = |i: u8| vec![i; usize::from(i) + 1];
        let pages: u8 = if cfg!(miri) { 40 } else { 200 };
        for _ in 0..if cfg!(miri) { 2 } else { 50 } {
            let (run, mut open) = OpenRun::start(&key(0), false, None);
            let done = std::sync::atomic::AtomicBool::new(false);
            std::thread::scope(|scope| {
                let done = &done;
                scope.spawn(move || {
                    for i in 1..=pages {
                        open.extend(&key(i));
                        open.publish();
                    }
                    done.store(true, std::sync::atomic::Ordering::Release);
                });
                let mut newest = 0u8;
                loop {
                    let finished = done.load(std::sync::atomic::Ordering::Acquire);
                    let (first, last) = run.bounds();
                    assert_eq!(first, key(0));
                    if let Some(last) = last {
                        let i = last[0];
                        assert_eq!(last, key(i), "a key the cursor never published");
                        assert!(i >= newest, "the stretch went back from {newest} to {i}");
                        newest = i;
                    }
                    if finished {
                        break;
                    }
                }
                assert_eq!(newest, pages, "the last read sees the last page");
            });
        }
    }

    #[test]
    fn walked_merges_overlapping_and_touching_runs_and_keeps_gaps() {
        let runs = [
            run(b"m", Some(b"p"), false),
            run(b"d", Some(b"b"), true),
            run(b"p", Some(b"q"), false),
            run(b"c", Some(b"e"), false),
            run(b"x", Some(b"x"), false),
        ];
        let lasts = lasts_of(&runs);
        let got = Walks::of(&runs, &lasts).whole;
        let span = |lo: &'static [u8], hi: &'static [u8]| Walked {
            lo: Some(lo),
            hi: Some(hi),
        };
        assert_eq!(got, [span(b"b", b"e"), span(b"m", b"q"), span(b"x", b"x")]);
        assert!(covers(&got, b"b") && covers(&got, b"e") && covers(&got, b"x"));
        assert!(!covers(&got, b"a") && !covers(&got, b"f") && !covers(&got, b"r"));
        assert!(
            !covers(&got, b"xa"),
            "a key just above an inclusive end is outside"
        );
    }

    #[test]
    fn an_unclosed_run_reaches_the_end_of_the_keyspace_in_its_direction() {
        let forward = [run(b"m", None, false), run(b"x", Some(b"y"), false)];
        let lasts = lasts_of(&forward);
        let got = Walks::of(&forward, &lasts).whole;
        assert_eq!(got.len(), 1, "the unbounded run swallows the later one");
        assert!(covers(&got, b"m") && covers(&got, b"\xff\xff"));
        assert!(!covers(&got, b"l"));

        let reverse = [run(b"m", None, true)];
        let lasts = lasts_of(&reverse);
        let got = Walks::of(&reverse, &lasts).whole;
        assert!(covers(&got, b"") && covers(&got, b"m"));
        assert!(!covers(&got, b"n"));
    }

    #[test]
    fn cover_anchors_walked_reads_and_adds_walked_writes_once() {
        let runs = [run(b"b", Some(b"d"), false)];
        let mut reads = vec![read(b"a", 9, ReadRule::Seq), read(b"c", 9, ReadRule::Seq)];
        let writes: BTreeMap<Vec<u8>, Option<Vec<u8>>> = [
            (b"b".to_vec(), Some(b"v".to_vec())),
            (b"c".to_vec(), None),
            (b"e".to_vec(), None),
        ]
        .into_iter()
        .collect();
        let merges = vec![
            (b"d".to_vec(), b"op".to_vec()),
            (b"d".to_vec(), b"op".to_vec()),
        ];
        cover(&mut reads, &runs, &writes, &merges, 4, &ReadRule::Seq);
        let got: Vec<(Vec<u8>, u64)> = reads
            .into_iter()
            .map(|read| (read.key, read.observed_seq))
            .collect();
        assert_eq!(
            got,
            [
                (b"a".to_vec(), 9),
                (b"b".to_vec(), 4),
                (b"c".to_vec(), 4),
                (b"d".to_vec(), 4),
            ]
        );
    }

    /// The rules `cover` gives the keys it adds, by key.
    fn added_rules(
        runs: &[Arc<ScanRun>],
        puts: &[&[u8]],
        merged: &[&[u8]],
    ) -> BTreeMap<Vec<u8>, ReadRule> {
        let writes: BTreeMap<Vec<u8>, Option<Vec<u8>>> = puts
            .iter()
            .map(|key| (key.to_vec(), Some(b"v".to_vec())))
            .collect();
        let merges: Vec<(Vec<u8>, Vec<u8>)> = merged
            .iter()
            .map(|key| (key.to_vec(), b"op".to_vec()))
            .collect();
        let mut reads = Vec::new();
        cover(&mut reads, runs, &writes, &merges, 4, &ReadRule::Value);
        reads
            .into_iter()
            .map(|read| (read.key, read.rule))
            .collect()
    }

    #[test]
    fn a_key_merged_inside_a_parts_scan_is_validated_over_its_parts() {
        let runs = [run_by(b"a", Some(b"z"), false, Some(&[2, 5]))];
        let rules = added_rules(&runs, &[], &[b"m"]);
        assert_eq!(rules[&b"m".to_vec()], ReadRule::Parts(Arc::from([2, 5])));
    }

    #[test]
    fn a_key_put_inside_a_parts_scan_is_validated_in_whole() {
        let runs = [run_by(b"a", Some(b"z"), false, Some(&[2]))];
        let rules = added_rules(&runs, &[b"m"], &[b"n"]);
        assert_eq!(
            rules[&b"m".to_vec()],
            ReadRule::Value,
            "a put replaces every part"
        );
        assert_eq!(rules[&b"n".to_vec()], ReadRule::Parts(Arc::from([2])));
    }

    #[test]
    fn scans_by_different_parts_union_and_a_whole_scan_wins() {
        let runs = [
            run_by(b"a", Some(b"m"), false, Some(&[3])),
            run_by(b"f", Some(b"t"), false, Some(&[1, 3])),
            run(b"p", Some(b"z"), false),
        ];
        let rules = added_rules(&runs, &[], &[b"c", b"g", b"q"]);
        assert_eq!(rules[&b"c".to_vec()], ReadRule::Parts(Arc::from([3])));
        assert_eq!(rules[&b"g".to_vec()], ReadRule::Parts(Arc::from([1, 3])));
        assert_eq!(
            rules[&b"q".to_vec()],
            ReadRule::Value,
            "a scan that used the whole value"
        );
    }

    #[test]
    fn a_scan_by_no_parts_validates_nothing() {
        let runs = [run_by(b"a", Some(b"z"), false, Some(&[]))];
        let mut reads = vec![read(b"m", 9, ReadRule::Seq)];
        let merges = vec![
            (b"m".to_vec(), b"op".to_vec()),
            (b"n".to_vec(), b"op".to_vec()),
        ];
        cover(
            &mut reads,
            &runs,
            &BTreeMap::new(),
            &merges,
            4,
            &ReadRule::Value,
        );
        assert_eq!(reads.len(), 1, "no key is added");
        assert_eq!(reads[0].observed_seq, 9, "and no read is anchored back");
    }

    #[test]
    fn a_projected_read_of_a_merged_key_widens_to_what_a_scan_used() {
        let merges = vec![(b"m".to_vec(), b"op".to_vec())];
        let projected = || vec![read(b"m", 4, ReadRule::Parts(Arc::from([1])))];

        let mut reads = projected();
        let runs = [run(b"a", Some(b"z"), false)];
        cover(
            &mut reads,
            &runs,
            &BTreeMap::new(),
            &merges,
            4,
            &ReadRule::Value,
        );
        assert_eq!(
            reads[0].rule,
            ReadRule::Value,
            "a whole-value scan covered it"
        );

        let mut reads = projected();
        let runs = [run_by(b"a", Some(b"z"), false, Some(&[2]))];
        cover(
            &mut reads,
            &runs,
            &BTreeMap::new(),
            &merges,
            4,
            &ReadRule::Value,
        );
        assert_eq!(reads[0].rule, ReadRule::Parts(Arc::from([1, 2])));

        let mut reads = projected();
        cover(
            &mut reads,
            &runs,
            &BTreeMap::new(),
            &[],
            4,
            &ReadRule::Value,
        );
        assert_eq!(
            reads[0].rule,
            ReadRule::Parts(Arc::from([1])),
            "a key the transaction does not merge into is not widened"
        );
    }

    #[test]
    fn a_published_run_covers_what_was_published_and_no_more() {
        let (record, open) = OpenRun::start(b"b", false, None);
        let covered = |key: &[u8]| {
            let runs = [Arc::clone(&record)];
            let lasts = lasts_of(&runs);
            covers(&Walks::of(&runs, &lasts).whole, key)
        };
        assert!(
            covered(b"zzz"),
            "nothing published: the run reaches the end"
        );
        let mut open = open;
        open.extend(b"d");
        open.publish();
        assert!(covered(b"d") && !covered(b"e"));
        open.extend(b"g");
        assert!(
            !covered(b"g"),
            "an extension is not seen until it is published"
        );
        drop(open);
        assert!(covered(b"g") && !covered(b"h"), "dropping closes the run");
    }

    #[test]
    fn a_scan_below_serializable_records_one_run_per_stretch_and_no_key() {
        let dir = TempDir::new().unwrap();
        let db = OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap();
        for key in [b"a", b"b", b"c"] {
            db.db().put(key, b"0").unwrap();
        }
        let mut tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::SnapshotIsolation));
        assert_eq!(tx.scan_stream(None, None).count(), 3);
        assert_eq!(tx.tracked.len(), 0, "no key is tracked below Serializable");
        tx.put(b"b", b"pending").unwrap();
        assert_eq!(
            tx.scan_stream_in(None, None, ScanDirection::Reverse)
                .count(),
            3
        );
        let runs = tx.scan_runs.take().map(|runs| drain(&runs)).unwrap();
        let bounds: Vec<(Vec<u8>, Option<Vec<u8>>, bool)> = runs
            .iter()
            .map(|run| {
                let (first, last) = run.bounds();
                (
                    first[4..].to_vec(),
                    last.map(|last| last[4..].to_vec()),
                    run.reverse,
                )
            })
            .collect();
        let spans: Vec<Span<'_>> = bounds
            .iter()
            .map(|(first, last, reverse)| (first.as_slice(), last.as_deref(), *reverse))
            .collect();
        assert_eq!(
            spans,
            [
                (&b"a"[..], Some(&b"c"[..]), false),
                (&b"c"[..], Some(&b"c"[..]), true),
                (&b"a"[..], Some(&b"a"[..]), true),
            ],
            "one run per stretch, split at the buffered b"
        );
        let tracked = tx.tracked.drain();
        let mut checks = tx.validation_set(tracked, &BTreeMap::new(), &[]);
        cover(
            &mut checks.reads,
            &runs,
            &BTreeMap::new(),
            &[],
            tx.snapshot_seq,
            &tx.full_read_rule(),
        );
        assert!(
            checks.reads.is_empty(),
            "a read-only commit validates nothing"
        );
    }

    proptest! {
        #[test]
        fn covers_agrees_with_checking_every_run(
            specs in proptest::collection::vec((0u8..8, proptest::option::of(0u8..8), any::<bool>()), 0..8),
            probe in 0u8..9,
        ) {
            let runs: Vec<Arc<ScanRun>> = specs
                .iter()
                .map(|(first, last, reverse)| {
                    // A walk yields in its direction, so `last` is never behind `first`.
                    let last = last.map(|last| if *reverse { last.min(*first) } else { last.max(*first) });
                    run(&[*first], last.as_ref().map(std::slice::from_ref), *reverse)
                })
                .collect();
            let key = [probe];
            let naive = runs.iter().any(|run| {
                let first = run.first.as_slice();
                let last = run.bounds().1;
                if run.reverse {
                    key.as_slice() <= first && last.as_deref().is_none_or(|last| last <= key.as_slice())
                } else {
                    first <= key.as_slice() && last.as_deref().is_none_or(|last| key.as_slice() <= last)
                }
            });
            let lasts = lasts_of(&runs);
            let merged = Walks::of(&runs, &lasts).whole;
            prop_assert!(merged.windows(2).all(|pair| !reaches(pair[0].hi, pair[1].lo)));
            prop_assert_eq!(covers(&merged, &key), naive);
        }
    }
}
