use gents::Collection;
use std::collections::BTreeSet;

fn config_apply_order_from_source() -> Vec<Collection> {
    let src = include_str!("../src/config_import.rs");
    let body_start = src.find("const CONFIG_APPLY_ORDER").unwrap();
    let body_end = src[body_start..].find("];").unwrap() + body_start;
    let body = &src[body_start..body_end];

    let re = regex::Regex::new(r"Collection::([A-Za-z]+)").unwrap();
    re.captures_iter(body)
        .map(|capture| {
            let variant_name = capture.get(1).unwrap().as_str();
            Collection::ALL
                .into_iter()
                .find(|collection| collection.graphql_type() == variant_name)
                .unwrap_or_else(|| {
                    panic!("unknown Collection variant in CONFIG_APPLY_ORDER: {variant_name}")
                })
        })
        .collect()
}

/// Anchors the centralized `CONFIG_APPLY_ORDER` table to the closed collection
/// set without carrying a second hardcoded ordered array in this test.
#[test]
fn apply_desired_state_changes_order_contains_each_collection_once() {
    let found = config_apply_order_from_source();
    let actual = found.iter().copied().collect::<BTreeSet<_>>();
    let expected = Collection::ALL.into_iter().collect::<BTreeSet<_>>();

    assert_eq!(actual, expected);
    assert_eq!(found.len(), Collection::ALL.len());
}

/// Guards the retry-safety comment on `CONFIG_APPLY_ORDER`: every split point is
/// a durable prefix whose already-written referrers are at ranks no greater
/// than any later dependency-writing rank. This directly invokes
/// `Collection::apply_order()`, so rank drift in the Rust enum surfaces here.
#[test]
fn apply_desired_state_changes_order_has_retry_safe_prefixes() {
    let order = config_apply_order_from_source();

    for prefix_len in 0..=order.len() {
        let prefix = &order[..prefix_len];
        let suffix = &order[prefix_len..];
        for written in prefix {
            for pending in suffix {
                assert!(
                    written.apply_order() <= pending.apply_order(),
                    "prefix {prefix_len} writes {:?} before lower-rank {:?}",
                    written,
                    pending,
                );
            }
        }
    }
}
