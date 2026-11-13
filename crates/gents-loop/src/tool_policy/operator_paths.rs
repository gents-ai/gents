pub fn command_guard_allows(
    required: bool,
    supported: bool,
    inspected: bool,
    single_linked: bool,
) -> bool {
    !required || (supported && inspected && single_linked)
}

#[cfg(test)]
mod conformance {
    use super::command_guard_allows;

    #[test]
    fn command_guard_matches_the_lean_model() {
        gents_lean_vocab::families::tool_policy_operator_paths_command_guard_allows::replay(|i| {
            command_guard_allows(i.required, i.supported, i.inspected, i.single_linked)
        });
    }

    #[test]
    fn operator_command_guard_matches_the_lean_model() {
        gents_lean_vocab::families::operator_paths_command_guard::replay::<String>(|i| {
            command_guard_allows(
                i.command.required,
                i.command.supported,
                i.command.inspected,
                i.command.single_linked,
            )
        });
    }
}
