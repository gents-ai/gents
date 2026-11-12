use std::collections::BTreeSet;

#[test]
fn generated_root_admission_case_inventory_is_complete() {
    let cases = &gents_lean_vocab::lean_contract_snapshot().root_admission_cases;
    assert!(!cases.is_empty());
    let mut names = BTreeSet::new();
    for case in cases {
        assert!(
            names.insert(case.name.as_str()),
            "duplicate case {}",
            case.name
        );
        assert_eq!(case.blank, case.authored.trim().is_empty(), "{}", case.name);
        assert!(
            matches!(case.operation.as_str(), "create" | "meet"),
            "unknown operation {} in {}",
            case.operation,
            case.name
        );
        assert!(
            !case.observation.trim().is_empty(),
            "{} omitted its filesystem observation",
            case.name
        );
    }
}
