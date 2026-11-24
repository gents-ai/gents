use super::*;
use crate::ttl::{TTL_FORMAT_VERSION, stamp};

/// Concatenates, base first.
struct Concat;

impl MergeOperator for Concat {
    fn full_merge(&self, _: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut out = base.map(<[u8]>::to_vec).unwrap_or_default();
        out.extend(operands.iter().flat_map(|o| o.iter().copied()));
        Some(out)
    }

    fn name(&self) -> &'static str {
        "concat"
    }
}

/// Puts each operand in front, so the base, stamp and all, stays at the end.
struct Prepend;

impl MergeOperator for Prepend {
    fn full_merge(&self, _: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut out = base.map(<[u8]>::to_vec).unwrap_or_default();
        for operand in operands {
            out.splice(0..0, operand.iter().copied());
        }
        Some(out)
    }

    fn name(&self) -> &'static str {
        "prepend"
    }
}

/// Declines every fold.
struct Declines;

impl MergeOperator for Declines {
    fn full_merge(&self, _: &[u8], _: Option<&[u8]>, _: &[&[u8]]) -> Option<Vec<u8>> {
        None
    }

    fn name(&self) -> &'static str {
        "declines"
    }
}

fn piece(base: bool, value: &[u8], at: Option<u64>) -> Piece<'_> {
    Piece { base, value, at }
}

fn ttl_merge(callers: impl MergeOperator) -> TtlMergeOperator {
    TtlMergeOperator {
        callers: Arc::new(callers),
    }
}

#[test]
fn a_bundle_round_trips_every_write_and_its_time() {
    let long = vec![7u8; 300];
    let pieces = [
        piece(true, b"base", Some(7)),
        piece(false, b"", Some(9)),
        piece(false, b"raw", None),
        piece(false, &long, Some(u64::MAX)),
    ];
    let bundle = encode(pieces);
    assert_eq!(decode(&bundle).unwrap(), pieces);
    // It is not a plain stamped value, and a plain one is not a bundle.
    assert!(decode_ttl_suffix(&bundle).is_none());
    assert!(decode(&stamp(b"v", 3)).is_none());
    assert_eq!(decode(&encode([])).unwrap(), []);
}

#[test]
fn a_damaged_bundle_is_not_read_as_one() {
    let bundle = encode([piece(false, b"op", Some(1))]);
    let body = bundle.len() - TTL_SUFFIX_LEN;
    let suffix = &bundle[body..];
    // A byte of the body missing: the last write runs past it.
    let mut short = bundle[..body - 1].to_vec();
    short.extend_from_slice(suffix);
    assert!(decode(&short).is_none());
    // A flag no write has.
    let mut flagged = bundle.clone();
    flagged[0] = 0x80;
    assert!(decode(&flagged).is_none());
    // A length past the body.
    let mut long_length = vec![FLAG_TIMED];
    encode_varint(&mut long_length, 1 << 40);
    long_length.extend_from_slice(b"op");
    long_length.extend_from_slice(suffix);
    assert!(decode(&long_length).is_none());
    // A length that never ends.
    let mut endless = vec![0, 0x80];
    endless.extend_from_slice(suffix);
    assert!(decode(&endless).is_none());
}

#[test]
fn a_merge_keeps_every_write_apart_with_its_time() {
    let op = ttl_merge(Concat);
    let merged = op
        .full_merge(b"k", Some(&stamp(b"a", 1)), &[&stamp(b"b", 2), b"raw"])
        .unwrap();
    assert_eq!(
        decode(&merged).unwrap(),
        [
            piece(true, b"a", Some(1)),
            piece(false, b"b", Some(2)),
            piece(false, b"raw", None)
        ]
    );
    // Folded again as a base, and two operands folded ahead of it.
    let ops = op
        .partial_merge(b"k", &stamp(b"c", 3), &stamp(b"d", 4))
        .unwrap();
    let again = op.full_merge(b"k", Some(&merged), &[&ops]).unwrap();
    let values: Vec<&[u8]> = decode(&again).unwrap().iter().map(|p| p.value).collect();
    assert_eq!(values, [&b"a"[..], b"b", b"raw", b"c", b"d"]);
}

#[test]
fn a_read_folds_the_live_merges_onto_the_live_put() {
    let pieces = [piece(true, b"a", Some(1)), piece(false, b"b", Some(2))];
    assert_eq!(
        read(Some(&Concat), b"k", &pieces, 0).unwrap(),
        Some((b"ab".to_vec(), 2))
    );
    // The put expired: the merge alone.
    assert_eq!(
        read(Some(&Concat), b"k", &pieces, 2).unwrap(),
        Some((b"b".to_vec(), 2))
    );
    assert_eq!(read(Some(&Concat), b"k", &pieces, 3).unwrap(), None);
    // Only the put: no operator is asked.
    assert_eq!(
        read(None, b"k", &pieces[..1], 0).unwrap(),
        Some((b"a".to_vec(), 1))
    );
    assert_eq!(read(None, b"k", &[], 0).unwrap(), None);
    assert!(matches!(
        read(None, b"k", &pieces, 0),
        Err(crate::Error::NoMergeOperator)
    ));
    // A put with no time is not seen; the merges after it are.
    let unstamped = [piece(true, b"raw", None), piece(false, b"b", Some(2))];
    assert_eq!(
        read(Some(&Concat), b"k", &unstamped, 0).unwrap(),
        Some((b"b".to_vec(), 2))
    );
}

#[test]
fn a_merge_with_no_time_folds_as_stored_and_reads_as_a_stamped_value() {
    // As 0.1.9 stored a merge onto a put: the operand without a stamp.
    let pieces = [piece(true, b"a", Some(5)), piece(false, b"x", None)];
    // The operator is shown the put with its stamp. Concatenated, the stamp
    // is no longer at the end, so the read returns nothing, as in 0.1.9.
    assert_eq!(read(Some(&Concat), b"k", &pieces, 0).unwrap(), None);
    // In front of the put, it keeps the stamp, and lives as long as the put.
    assert_eq!(
        read(Some(&Prepend), b"k", &pieces, 5).unwrap(),
        Some((b"xa".to_vec(), 5))
    );
    // A put that has expired is left out of the fold, so it does not matter
    // whether compaction has dropped it yet.
    assert_eq!(read(Some(&Prepend), b"k", &pieces, 6).unwrap(), None);
    assert_eq!(
        read(Some(&Prepend), b"k", &pieces, 6).unwrap(),
        read(Some(&Prepend), b"k", &pieces[1..], 6).unwrap()
    );
    // Timed merges after it apply to what it reads as.
    let then = [
        piece(true, b"a", Some(5)),
        piece(false, b"x", None),
        piece(false, b"c", Some(9)),
    ];
    assert_eq!(
        read(Some(&Prepend), b"k", &then, 5).unwrap(),
        Some((b"cxa".to_vec(), 9))
    );
    assert_eq!(
        read(Some(&Prepend), b"k", &then, 6).unwrap(),
        Some((b"c".to_vec(), 9))
    );
    // A fold the operator declines fails the read.
    assert!(matches!(
        read(Some(&Declines), b"k", &pieces, 0),
        Err(crate::Error::MergeFailed(key)) if key == b"k"
    ));
}

#[test]
fn a_run_with_no_time_is_folded_where_it_is_stored() {
    let op = ttl_merge(Concat);
    let unfolded = [b"raw".as_slice(), b"x", b"y"];
    let merged = op
        .full_merge(
            b"k",
            None,
            &[unfolded[0], unfolded[1], &stamp(b"c", 3), unfolded[2]],
        )
        .unwrap();
    // The front run, up to its last merge, is one put now; a merge with no
    // time behind a timed one waits for that one to expire.
    assert_eq!(
        decode(&merged).unwrap(),
        [
            piece(true, b"rawx", None),
            piece(false, b"c", Some(3)),
            piece(false, b"y", None)
        ]
    );
    // A read gives what it gives on the writes unfolded.
    let apart = [
        piece(false, b"raw", None),
        piece(false, b"x", None),
        piece(false, b"c", Some(3)),
        piece(false, b"y", None),
    ];
    for horizon in [0, 3, 4] {
        assert_eq!(
            read(Some(&Concat), b"k", &decode(&merged).unwrap(), horizon).unwrap(),
            read(Some(&Concat), b"k", &apart, horizon).unwrap(),
            "horizon {horizon}"
        );
    }
    // With only timed merges behind the run, a read sees them alone, folded
    // or not.
    let before_timed = op
        .full_merge(b"k", None, &[b"x", b"y", &stamp(b"c", 3)])
        .unwrap();
    let apart = [
        piece(false, b"x", None),
        piece(false, b"y", None),
        piece(false, b"c", Some(3)),
    ];
    assert_eq!(
        read(Some(&Concat), b"k", &decode(&before_timed).unwrap(), 0).unwrap(),
        Some((b"c".to_vec(), 3))
    );
    assert_eq!(
        read(Some(&Concat), b"k", &apart, 0).unwrap(),
        Some((b"c".to_vec(), 3))
    );
    // Once the timed one has gone, the rest folds too.
    let pruned = encode(
        decode(&merged)
            .unwrap()
            .into_iter()
            .filter(|p| p.at.is_none()),
    );
    let refolded = op.full_merge(b"k", None, &[&pruned]).unwrap();
    assert_eq!(decode(&refolded).unwrap(), [piece(true, b"rawxy", None)]);
}

#[test]
fn a_run_whose_fold_is_stamped_or_declined_stays_apart() {
    // Two operands with no stamp of their own whose concatenation ends in
    // one: folded, the run would start to expire, so it is left apart.
    let mut tail = vec![TTL_FORMAT_VERSION];
    tail.extend_from_slice(&5u64.to_be_bytes());
    let merged = ttl_merge(Concat)
        .full_merge(b"k", None, &[b"vRTTL", &tail])
        .unwrap();
    assert_eq!(decode(&merged).unwrap().len(), 2);
    assert_eq!(
        read(Some(&Concat), b"k", &decode(&merged).unwrap(), 5).unwrap(),
        Some((b"v".to_vec(), 5))
    );
    let declined = ttl_merge(Declines)
        .full_merge(b"k", None, &[b"x", b"y"])
        .unwrap();
    assert_eq!(
        decode(&declined).unwrap(),
        [piece(false, b"x", None), piece(false, b"y", None)]
    );
}
