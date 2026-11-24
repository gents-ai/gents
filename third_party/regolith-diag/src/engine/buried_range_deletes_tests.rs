//! The level 0 index of the check against the definition it answers: a
//! table added so far whose range `[smallest, largest]` meets `[start, end)`.
//! The check itself, over real tables, is tested in
//! `manifest/buried_delete_tests.rs`.

use proptest::prelude::*;

use super::*;

/// The definition, one table at a time.
fn meets_by_hand(added: &[(Vec<u8>, Vec<u8>)], start: &[u8], end: &[u8]) -> bool {
    added
        .iter()
        .any(|(smallest, largest)| smallest.as_slice() < end && largest.as_slice() >= start)
}

#[test]
fn a_range_meets_a_table_it_shares_a_key_with_and_not_one_starting_at_its_end() {
    let ranges: Vec<(&[u8], &[u8])> = vec![(b"c", b"f"), (b"m", b"p"), (b"x", b"x")];
    let mut index = LevelZero::new(ranges);
    assert!(!index.meets(b"a", b"z"), "nothing added yet");
    index.add(1);
    assert!(index.meets(b"a", b"n"));
    assert!(index.meets(b"p", b"q"), "the largest key is in the table");
    assert!(!index.meets(b"a", b"m"), "the end is not in the range");
    assert!(!index.meets(b"q", b"z"));
    index.add(2);
    assert!(index.meets(b"q", b"z"));
    assert!(!index.meets(b"d", b"e"), "the table holding d is not added");
    index.add(0);
    assert!(index.meets(b"d", b"e"));
}

proptest! {
    /// Added in any order, asked at every step, the index answers what the
    /// definition answers.
    #[test]
    fn the_index_answers_what_one_table_at_a_time_answers(
        tables in prop::collection::vec((0u8..20, 0u8..20), 0..24),
        order in prop::collection::vec(any::<prop::sample::Index>(), 0..24),
        probes in prop::collection::vec((0u8..21, 0u8..21), 1..12),
    ) {
        let tables: Vec<(Vec<u8>, Vec<u8>)> = tables
            .into_iter()
            .map(|(a, b)| (vec![a.min(b)], vec![a.max(b)]))
            .collect();
        let mut index = LevelZero::new(
            tables
                .iter()
                .map(|(smallest, largest)| (smallest.as_slice(), largest.as_slice()))
                .collect(),
        );
        let mut added: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        let mut left: Vec<usize> = (0..tables.len()).collect();
        for pick in order {
            if left.is_empty() {
                break;
            }
            let position = left.swap_remove(pick.index(left.len()));
            index.add(position);
            added.push(tables[position].clone());
            for (a, b) in &probes {
                let (start, end) = (vec![*a.min(b)], vec![*a.max(b)]);
                prop_assert_eq!(
                    index.meets(&start, &end),
                    meets_by_hand(&added, &start, &end)
                );
            }
        }
    }
}
