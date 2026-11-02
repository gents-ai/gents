use super::control::operator_control_authorized;

#[test]
fn generated_operator_controls_stay_on_the_owning_runtime() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().operator_job_control_cases;
    assert!(!cases.is_empty());
    for case in cases {
        assert_eq!(
            operator_control_authorized(
                case["executing"].as_str().unwrap(),
                case["requested"].as_str().unwrap(),
                case["owner"].as_str().unwrap(),
                case["actor"].as_str(),
            ),
            case["expected"].as_bool().unwrap(),
            "{case}"
        );
    }
}
