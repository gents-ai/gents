use super::{DestructiveAuthority, InventoryRead};

#[test]
fn destructive_decision_follows_the_model_for_every_available_read() {
    let cases = &crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .expect("configuration model")
        .destructive_decision_cases;
    assert_eq!(cases.len(), 8);
    let mut checked = 0;
    for case in cases {
        // Gents has no complete-inventory read to drive.
        if case["complete_inventory"].as_bool().unwrap() {
            continue;
        }
        let authority = DestructiveAuthority {
            nac_enabled: case["nac_enabled"].as_bool().unwrap(),
            admin: case["admin"].as_bool().unwrap(),
            inventory: InventoryRead::AcpFiltered,
        };
        assert_eq!(
            authority.decide().err().map(|refusal| refusal.name()),
            case["refusal"].as_str(),
            "{case}"
        );
        checked += 1;
    }
    assert_eq!(checked, 4);
}
