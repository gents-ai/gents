//! Range tombstones: `(start, end, seq)` triples that delete every
//! user key in `[start, end)` as of `seq`.
//!
//! Memtables and SSTables each hold a collection of range tombstones
//! alongside their point entries. A read at `snapshot_seq` treats a
//! user key `k` as deleted when the newest covering range tombstone
//! visible at that snapshot has a `seq` strictly greater than the
//! newest point entry for `k` visible at that snapshot.
//!
//! Tombstones are indexed by start key with a prefix maximum of end
//! keys. A point coverage check first binary-searches the last
//! tombstone whose start is `<= user_key`, then walks backward only
//! while some earlier tombstone can still reach the key.

/// A single range tombstone.
#[derive(Debug, Clone)]
pub(crate) struct RangeTombstone {
    pub(crate) start: Vec<u8>,
    pub(crate) end: Vec<u8>,
    pub(crate) seq: u64,
}

impl RangeTombstone {
    pub(crate) fn new(start: Vec<u8>, end: Vec<u8>, seq: u64) -> Self {
        Self { start, end, seq }
    }

    /// Does this tombstone cover `user_key`? The range is half-open:
    /// `start <= user_key < end`.
    pub(crate) fn covers(&self, user_key: &[u8]) -> bool {
        self.start.as_slice() <= user_key && user_key < self.end.as_slice()
    }

    pub(crate) fn overlaps(&self, start: &[u8], end: &[u8]) -> bool {
        self.start.as_slice() < end && start < self.end.as_slice()
    }

    pub(crate) fn clip_to(&self, start: &[u8], end: &[u8]) -> Option<Self> {
        if !self.overlaps(start, end) {
            return None;
        }

        let clipped_start = if self.start.as_slice() < start {
            start.to_vec()
        } else {
            self.start.clone()
        };
        let clipped_end = if self.end.as_slice() > end {
            end.to_vec()
        } else {
            self.end.clone()
        };

        (clipped_start < clipped_end).then(|| Self::new(clipped_start, clipped_end, self.seq))
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RangeTombstoneSet {
    tombstones: Vec<RangeTombstone>,
    prefix_max_end: Vec<Vec<u8>>,
}

impl RangeTombstoneSet {
    pub(crate) fn from_vec(mut tombstones: Vec<RangeTombstone>) -> Self {
        sort_dedup_tombstones(&mut tombstones);
        let prefix_max_end = build_prefix_max_end(&tombstones);
        Self {
            tombstones,
            prefix_max_end,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.tombstones.is_empty()
    }

    pub(crate) fn as_slice(&self) -> &[RangeTombstone] {
        &self.tombstones
    }

    pub(crate) fn iter(&self) -> std::slice::Iter<'_, RangeTombstone> {
        self.tombstones.iter()
    }

    /// Return the largest `seq` of any range tombstone that covers
    /// `user_key` and is visible at `snapshot_seq`.
    pub(crate) fn max_covering_seq(&self, user_key: &[u8], snapshot_seq: u64) -> u64 {
        if self.tombstones.is_empty() {
            return 0;
        }

        let mut idx = self
            .tombstones
            .partition_point(|rt| rt.start.as_slice() <= user_key);
        let mut best = 0;

        while idx > 0 {
            idx -= 1;
            if self.prefix_max_end[idx].as_slice() <= user_key {
                break;
            }

            let rt = &self.tombstones[idx];
            if rt.seq <= snapshot_seq && rt.covers(user_key) && rt.seq > best {
                best = rt.seq;
            }
        }

        best
    }

    /// Whether one tombstone of the set covers every key of the closed
    /// range `[first, last]`. Two tombstones that only cover it between them
    /// do not count.
    pub(crate) fn covers_whole(&self, first: &[u8], last: &[u8]) -> bool {
        let mut idx = self
            .tombstones
            .partition_point(|rt| rt.start.as_slice() <= first);
        while idx > 0 {
            idx -= 1;
            // No tombstone at or before `idx` ends past `last`.
            if self.prefix_max_end[idx].as_slice() <= last {
                break;
            }
            if self.tombstones[idx].end.as_slice() > last {
                return true;
            }
        }
        false
    }

    pub(crate) fn clipped_overlaps(&self, start: &[u8], end: &[u8]) -> Vec<RangeTombstone> {
        let mut result = Vec::new();
        if start >= end {
            return result;
        }

        let mut idx = self
            .tombstones
            .partition_point(|rt| rt.start.as_slice() < end);

        while idx > 0 {
            idx -= 1;
            if self.prefix_max_end[idx].as_slice() <= start {
                break;
            }

            if let Some(clipped) = self.tombstones[idx].clip_to(start, end) {
                result.push(clipped);
            }
        }

        sort_dedup_tombstones(&mut result);
        result
    }
}

/// Convenience wrapper for one-shot checks over an unindexed slice.
#[cfg(test)]
pub(crate) fn max_covering_seq(
    tombstones: &[RangeTombstone],
    user_key: &[u8],
    snapshot_seq: u64,
) -> u64 {
    RangeTombstoneSet::from_vec(tombstones.to_vec()).max_covering_seq(user_key, snapshot_seq)
}

/// Return an exclusive upper bound that contains exactly `key` among
/// byte strings sharing `key` as a prefix.
pub(crate) fn exclusive_successor(key: &[u8]) -> Vec<u8> {
    let mut end = Vec::with_capacity(key.len() + 1);
    end.extend_from_slice(key);
    end.push(0);
    end
}

/// The user-key range a table covers: its point keys `points`, widened to take
/// in the start and end of every tombstone in `tombstones`, which cover keys
/// the points do not. `None` for a table holding neither.
///
/// The one place this is decided, so the range a table is placed by and the
/// range it is recorded with cannot differ.
pub(crate) fn table_key_range<'tombstone>(
    points: Option<(Vec<u8>, Vec<u8>)>,
    tombstones: impl IntoIterator<Item = &'tombstone RangeTombstone>,
) -> Option<(Vec<u8>, Vec<u8>)> {
    tombstones
        .into_iter()
        .fold(points, |range, rt| match range {
            None => Some((rt.start.clone(), rt.end.clone())),
            Some((smallest, largest)) => Some((
                if rt.start < smallest {
                    rt.start.clone()
                } else {
                    smallest
                },
                if rt.end > largest {
                    rt.end.clone()
                } else {
                    largest
                },
            )),
        })
}

/// `tombstones` with every run of ranges that share a sequence and touch
/// or overlap joined into one, oldest sequence first.
///
/// A compaction clips a tombstone to each table it writes, so one
/// `delete_range` can reach here as a piece per table. Joined, the pieces
/// delete exactly what they deleted apart: the same keys, at the same
/// sequence.
pub(crate) fn coalesced(mut tombstones: Vec<RangeTombstone>) -> Vec<RangeTombstone> {
    sort_dedup_tombstones(&mut tombstones);
    // Stable, so each sequence keeps its ranges in start order.
    tombstones.sort_by_key(|tombstone| tombstone.seq);
    let mut joined: Vec<RangeTombstone> = Vec::with_capacity(tombstones.len());
    for tombstone in tombstones {
        match joined.last_mut() {
            Some(last) if last.seq == tombstone.seq && tombstone.start <= last.end => {
                if tombstone.end > last.end {
                    last.end = tombstone.end;
                }
            }
            _ => joined.push(tombstone),
        }
    }
    joined
}

pub(crate) fn sort_dedup_tombstones(tombstones: &mut Vec<RangeTombstone>) {
    tombstones.sort_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then_with(|| a.end.cmp(&b.end))
            .then_with(|| b.seq.cmp(&a.seq))
    });
    tombstones.dedup_by(|a, b| a.start == b.start && a.end == b.end && a.seq == b.seq);
}

fn build_prefix_max_end(tombstones: &[RangeTombstone]) -> Vec<Vec<u8>> {
    let mut prefix = Vec::with_capacity(tombstones.len());
    let mut max_end: Option<Vec<u8>> = None;
    for rt in tombstones {
        if max_end
            .as_ref()
            .is_none_or(|current| current.as_slice() < rt.end.as_slice())
        {
            max_end = Some(rt.end.clone());
        }
        prefix.push(max_end.as_ref().expect("just initialized").clone());
    }
    prefix
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn covers_half_open() {
        let rt = RangeTombstone::new(b"b".to_vec(), b"d".to_vec(), 10);
        assert!(!rt.covers(b"a"));
        assert!(rt.covers(b"b"));
        assert!(rt.covers(b"c"));
        assert!(!rt.covers(b"d")); // end is exclusive
        assert!(!rt.covers(b"e"));
    }

    #[test]
    fn max_covering_seq_picks_largest_visible() {
        let tombstones = vec![
            RangeTombstone::new(b"a".to_vec(), b"z".to_vec(), 5),
            RangeTombstone::new(b"a".to_vec(), b"z".to_vec(), 20), // invisible at snap=10
            RangeTombstone::new(b"c".to_vec(), b"e".to_vec(), 8),
        ];
        assert_eq!(max_covering_seq(&tombstones, b"d", 10), 8);
        assert_eq!(max_covering_seq(&tombstones, b"b", 10), 5);
        assert_eq!(max_covering_seq(&tombstones, b"m", 10), 5);
        // At snap=100, the seq=20 tombstone becomes visible.
        assert_eq!(max_covering_seq(&tombstones, b"m", 100), 20);
        // No coverage.
        assert_eq!(max_covering_seq(&tombstones, b"0", 100), 0);
    }

    #[test]
    fn max_covering_seq_empty_list_returns_zero() {
        assert_eq!(max_covering_seq(&[], b"k", u64::MAX), 0);
    }

    #[test]
    fn empty_range_covers_nothing() {
        // A degenerate `[x, x)` range has no keys in it.
        let rt = RangeTombstone::new(b"m".to_vec(), b"m".to_vec(), 1);
        assert!(!rt.covers(b"m"));
        assert!(!rt.covers(b"n"));
        assert!(!rt.covers(b"l"));
    }

    #[test]
    fn snapshot_below_all_tombstone_seqs_sees_no_coverage() {
        let rts = vec![
            RangeTombstone::new(b"a".to_vec(), b"z".to_vec(), 10),
            RangeTombstone::new(b"a".to_vec(), b"z".to_vec(), 20),
        ];
        assert_eq!(max_covering_seq(&rts, b"m", 5), 0);
        assert_eq!(max_covering_seq(&rts, b"m", 9), 0);
        assert_eq!(max_covering_seq(&rts, b"m", 10), 10);
    }

    #[test]
    fn tombstone_set_prunes_disjoint_ranges() {
        let set = RangeTombstoneSet::from_vec(vec![
            RangeTombstone::new(b"a".to_vec(), b"b".to_vec(), 1),
            RangeTombstone::new(b"d".to_vec(), b"e".to_vec(), 2),
            RangeTombstone::new(b"g".to_vec(), b"h".to_vec(), 3),
        ]);
        assert_eq!(set.max_covering_seq(b"f", u64::MAX), 0);
        assert_eq!(set.max_covering_seq(b"g", u64::MAX), 3);
    }

    #[test]
    fn a_whole_range_is_covered_only_by_one_tombstone_over_all_of_it() {
        let set = RangeTombstoneSet::from_vec(vec![
            RangeTombstone::new(b"a".to_vec(), b"c".to_vec(), 1),
            RangeTombstone::new(b"b".to_vec(), b"k".to_vec(), 2),
            RangeTombstone::new(b"k".to_vec(), b"p".to_vec(), 3),
        ]);
        assert!(set.covers_whole(b"b", b"j"));
        assert!(set.covers_whole(b"a", b"b"));
        assert!(set.covers_whole(b"k", b"o"));
        // The end is exclusive.
        assert!(!set.covers_whole(b"b", b"k"));
        // Covered between two tombstones, by neither alone.
        assert!(!set.covers_whole(b"c", b"m"));
        assert!(!set.covers_whole(b"0", b"b"));
        assert!(!set.covers_whole(b"q", b"r"));
        assert!(!RangeTombstoneSet::default().covers_whole(b"a", b"b"));
    }

    #[test]
    fn clipped_overlaps_returns_fragments_in_order() {
        let set = RangeTombstoneSet::from_vec(vec![
            RangeTombstone::new(b"a".to_vec(), b"z".to_vec(), 5),
            RangeTombstone::new(b"c".to_vec(), b"e".to_vec(), 7),
        ]);
        let clipped = set.clipped_overlaps(b"b", b"f");
        assert_eq!(clipped.len(), 2);
        assert_eq!(clipped[0].start, b"b");
        assert_eq!(clipped[0].end, b"f");
        assert_eq!(clipped[0].seq, 5);
        assert_eq!(clipped[1].start, b"c");
        assert_eq!(clipped[1].end, b"e");
        assert_eq!(clipped[1].seq, 7);
    }

    fn range_of(
        points: Option<(&[u8], &[u8])>,
        tombstones: &[(&[u8], &[u8])],
    ) -> Option<(Vec<u8>, Vec<u8>)> {
        let tombstones: Vec<RangeTombstone> = tombstones
            .iter()
            .map(|&(start, end)| RangeTombstone::new(start.to_vec(), end.to_vec(), 1))
            .collect();
        let points = points.map(|(first, last)| (first.to_vec(), last.to_vec()));
        table_key_range(points, &tombstones)
    }

    #[test]
    fn a_table_range_is_its_points_widened_by_every_tombstone() {
        let range = |points, tombstones: &[(&[u8], &[u8])]| range_of(points, tombstones);
        let pair = |a: &[u8], b: &[u8]| Some((a.to_vec(), b.to_vec()));

        assert_eq!(range(None, &[]), None);
        assert_eq!(range(Some((b"m", b"p")), &[]), pair(b"m", b"p"));
        // Tombstones alone: their hull.
        assert_eq!(range(None, &[(b"c", b"f"), (b"b", b"d")]), pair(b"b", b"f"));
        // A tombstone inside the points changes nothing.
        assert_eq!(range(Some((b"a", b"z")), &[(b"c", b"f")]), pair(b"a", b"z"));
        // One reaching past either end, or both, widens that end.
        assert_eq!(range(Some((b"m", b"p")), &[(b"a", b"n")]), pair(b"a", b"p"));
        assert_eq!(range(Some((b"m", b"p")), &[(b"n", b"z")]), pair(b"m", b"z"));
        assert_eq!(
            range(Some((b"m", b"p")), &[(b"a", b"b"), (b"y", b"z")]),
            pair(b"a", b"z")
        );
    }

    #[test]
    fn exclusive_successor_contains_exact_key_only() {
        assert_eq!(exclusive_successor(b"a"), b"a\0");
        assert!(b"a".as_slice() < exclusive_successor(b"a").as_slice());
        assert!(exclusive_successor(b"a").as_slice() < b"aa".as_slice());
    }

    fn tombstone(start: &[u8], end: &[u8], seq: u64) -> RangeTombstone {
        RangeTombstone::new(start.to_vec(), end.to_vec(), seq)
    }

    fn as_tuples(tombstones: &[RangeTombstone]) -> Vec<(&[u8], &[u8], u64)> {
        tombstones
            .iter()
            .map(|t| (t.start.as_slice(), t.end.as_slice(), t.seq))
            .collect()
    }

    #[test]
    fn pieces_of_one_range_delete_are_joined_and_others_are_not() {
        let joined = coalesced(vec![
            // One delete at sequence 9, clipped to three tables, one piece twice.
            tombstone(b"c", b"f", 9),
            tombstone(b"a", b"c", 9),
            tombstone(b"e", b"h", 9),
            tombstone(b"a", b"c", 9),
            // The same range at another sequence is another delete.
            tombstone(b"a", b"h", 4),
            // A gap: `h` itself is not deleted at 9.
            tombstone(b"i", b"k", 9),
        ]);
        let expected: [(&[u8], &[u8], u64); 3] =
            [(b"a", b"h", 4), (b"a", b"h", 9), (b"i", b"k", 9)];
        assert_eq!(as_tuples(&joined), expected);
        assert!(coalesced(Vec::new()).is_empty());
    }

    proptest! {
        /// Joined, the tombstones delete what they deleted apart: for every key
        /// and every snapshot, the newest covering sequence is the same.
        #[test]
        fn joined_range_deletes_cover_exactly_what_the_pieces_covered(
            pieces in prop::collection::vec((0u8..12, 0u8..12, 1u64..5), 0..12),
            key in 0u8..12,
            snapshot in 0u64..6,
        ) {
            let pieces: Vec<RangeTombstone> = pieces
                .into_iter()
                .map(|(a, b, seq)| tombstone(&[a.min(b)], &[a.max(b)], seq))
                .collect();
            let joined = coalesced(pieces.clone());
            prop_assert_eq!(
                max_covering_seq(&joined, &[key], snapshot),
                max_covering_seq(&pieces, &[key], snapshot)
            );
            prop_assert!(joined.windows(2).all(|pair| pair[0].seq <= pair[1].seq));
            prop_assert!(joined.len() <= pieces.len());
        }
    }
}
