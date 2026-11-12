#[test]
fn generated_provider_eof_cases_fence_production_policy() {
    gents_lean_vocab::families::request_execution_lease_contracts_provider_eof::replay(|case| {
        gents_lean_vocab::families::request_execution_lease_contracts_provider_eof::ProviderEofOutput {
            failure: crate::request::lifecycle::execution_policy::provider_eof_is_failure(
                case.saw_explicit_final,
            ),
        }
    });
}
