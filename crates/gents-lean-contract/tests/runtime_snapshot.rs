#[test]
fn generated_contract_decodes_into_runtime_snapshot() {
    let json = gents_lean_contract::load_contract_json().expect("generate Lean contracts");
    let snapshot: gents_lean_vocab::LeanContractSnapshot =
        serde_json::from_str(&json).expect("decode current contracts with runtime fixture types");
    assert!(!snapshot.generated_by.is_empty());
    assert!(!snapshot.vocabularies.is_empty());
    assert!(!snapshot.coverage_ledger.is_empty());
}

#[test]
fn generated_snapshot_rejects_missing_or_unknown_categories() {
    let json = gents_lean_contract::load_contract_json().expect("generate Lean contracts");
    let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let object = value.as_object_mut().expect("contract object");
    let keys: Vec<_> = object.keys().cloned().collect();
    for key in keys {
        let removed = object.remove(&key).unwrap();
        let result = serde_json::from_value::<gents_lean_vocab::LeanContractSnapshot>(
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
    assert!(serde_json::from_value::<gents_lean_vocab::LeanContractSnapshot>(value).is_err());
}
