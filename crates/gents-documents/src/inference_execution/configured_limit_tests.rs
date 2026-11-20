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

#[test]
fn request_retry_budget_resolves_and_validates_as_the_lean_model() {
    use gents_lean_vocab::families::request_pinned_retry_budget_configured_budgets as family;
    for case in family::cases() {
        let execution = InferenceExecution {
            node_did: "did:key:zNODE".into(),
            execution_id: "budget".into(),
            max_request_retries: case.input.stored,
            ..Default::default()
        };
        let resolved = execution.request_retry_budget();
        assert_eq!(
            execution.validation_violations().is_empty(),
            resolved.is_ok(),
            "{:?}",
            case.input.stored
        );
        case.check(family::Outcome {
            accepted: resolved.is_ok(),
            error: resolved.as_ref().err().map(ToString::to_string),
            max_retries: resolved.ok(),
        });
    }
}
