//! Generated field tables and patch results checked against the existing patch owner.
//! Reference validation, nested Tools no-lockout, and transactional rejection
//! need an end-to-end ConfigApplyTxn consumer; this test does not simulate them.
use crate::lean_vocab_test::{
    lean_self_config_cases, lean_self_config_field_tables, LeanSelfConfigCase,
};
use gents::config_client::patch::{
    apply_patch, ensure_admissible, SelfConfigPatch, SelfConfigTarget, ALL_SELF_CONFIG_TARGETS,
};
use serde_json::{Map, Value};
use std::collections::BTreeSet;

fn doc_map(entries: &[crate::lean_vocab_test::LeanSelfConfigFieldValue]) -> Map<String, Value> {
    entries
        .iter()
        .map(|entry| (entry.field.clone(), Value::String(entry.value.clone())))
        .collect()
}

fn case_patch(case: &LeanSelfConfigCase) -> SelfConfigPatch {
    case.patch
        .iter()
        .map(|entry| {
            let value = match entry.action.as_str() {
                "set" => Some(Value::String(
                    entry
                        .value
                        .clone()
                        .expect("set patch entry must carry a value"),
                )),
                "clear" => None,
                other => panic!("unknown patch action {other}"),
            };
            (entry.field.clone(), value)
        })
        .collect()
}

pub(super) fn self_config_field_tables_match_lean_contract() {
    let tables = lean_self_config_field_tables();
    let actual: BTreeSet<_> = ALL_SELF_CONFIG_TARGETS
        .iter()
        .map(|t| t.collection_name())
        .collect();
    let expected: BTreeSet<_> = tables.iter().map(|t| t.collection.as_str()).collect();
    assert_eq!(actual, expected, "runtime self-config target inventory");
    for table in tables {
        let target =
            SelfConfigTarget::from_collection_name(&table.collection).expect("runtime target");
        assert_eq!(
            table.unique_field,
            target.unique_field(),
            "{}",
            table.collection
        );
        assert_eq!(table.category, target.category(), "{}", table.collection);
        assert_eq!(
            table.all_fields,
            target.all_fields(),
            "{}",
            table.collection
        );
        assert_eq!(
            table.writable_fields,
            target.writable_fields(),
            "{}",
            table.collection
        );
        assert_eq!(
            table.protected_fields,
            target.protected_fields(),
            "{}",
            table.collection
        );
    }
}

pub(super) fn generated_self_config_cases_fence_patch_merge() {
    let cases = lean_self_config_cases();
    assert!(!cases.is_empty());
    for case in cases {
        let target =
            SelfConfigTarget::from_collection_name(&case.collection).expect("runtime target");
        let stored = doc_map(&case.doc);
        let patch = case_patch(case);
        assert_eq!(
            ensure_admissible(target, &patch).is_ok(),
            case.admissible,
            "{}: runtime patch admissibility",
            case.name
        );
        // Accepted rows expose the merge result. Rejected rows expose stored
        // state, so comparing a locally selected fallback would prove nothing
        // about the real transaction or reference/no-lockout validator.
        if case.accepted {
            assert_eq!(
                apply_patch(target, &stored, &patch),
                doc_map(&case.result),
                "{}: runtime patch merge",
                case.name
            );
        }
    }
}
