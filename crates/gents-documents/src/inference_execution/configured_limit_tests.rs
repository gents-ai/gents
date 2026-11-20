use super::InferenceExecution;

#[test]
fn loop_guard_limits_resolve_and_validate_as_the_lean_model() {
    for case in
        gents_lean_vocab::families::invalid_tool_progress_contracts_configured::cases::<String>()
    {
        let mut execution = InferenceExecution {
            node_did: "did:key:zNODE".into(),
            execution_id: "limits".into(),
            ..Default::default()
        };
        let resolved = match case.input.field.as_str() {
            "max_invalid_tool_calls" => {
                execution.max_invalid_tool_calls = case.input.configured;
                execution.invalid_tool_call_allowance()
            }
            "repeated_tool_failure_limit" => {
                execution.repeated_tool_failure_limit = case.input.configured;
                execution.repeated_failure_limit()
            }
            other => panic!("no InferenceExecution field for {other}"),
        };
        assert_eq!(
            execution.validation_violations().is_empty(),
            resolved.is_some(),
            "{}: {:?}",
            case.input.field,
            case.input.configured
        );
        case.check(resolved.map(|limit| u64::try_from(limit).unwrap()));
    }
}
