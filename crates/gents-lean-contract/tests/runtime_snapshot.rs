// Compile and decode the runtime's actual fixture DTOs independently of gents.
// This checks the contract transport boundary even while a stacked runtime
// migration prevents the full runtime crate from compiling.
#[path = "../../gents/src/lean_vocab_test/support.rs"]
mod runtime_contract;

#[path = "../../gents/tests/support/conformance_consumers.rs"]
mod consumer_registry;

#[test]
fn generated_contract_decodes_into_runtime_snapshot() {
    let json = gents_lean_contract::load_contract_json().expect("generate Lean contracts");
    let snapshot: runtime_contract::LeanContractSnapshot =
        serde_json::from_str(&json).expect("decode current contracts with runtime fixture types");
    assert!(!snapshot.generated_by.is_empty());
    assert!(!snapshot.vocabularies.is_empty());
    assert!(!snapshot.coverage_ledger.is_empty());
}

#[test]
fn generated_consumers_resolve_to_registered_tests() {
    let snapshot: runtime_contract::LeanContractSnapshot =
        gents_lean_contract::load_contract_snapshot().expect("decode Lean contracts");
    let declared = snapshot
        .coverage_ledger
        .iter()
        .filter(|entry| !entry.consumer.is_empty())
        .map(|entry| entry.consumer.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        consumer_registry::assert_registered_conformance_consumers_resolve(),
        declared
    );
}

#[test]
fn generated_snapshot_rejects_missing_or_unknown_categories() {
    let json = gents_lean_contract::load_contract_json().expect("generate Lean contracts");
    let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let object = value.as_object_mut().expect("contract object");
    let keys: Vec<_> = object.keys().cloned().collect();
    for key in keys {
        let removed = object.remove(&key).unwrap();
        let result = serde_json::from_value::<runtime_contract::LeanContractSnapshot>(
            serde_json::Value::Object(object.clone()),
        );
        assert!(
            result.is_err(),
            "missing generated category {key} was silently defaulted"
        );
        object.insert(key, removed);
    }
    object.insert(
        "unrecognized_contract_category".into(),
        serde_json::json!([]),
    );
    assert!(serde_json::from_value::<runtime_contract::LeanContractSnapshot>(value).is_err());
}
