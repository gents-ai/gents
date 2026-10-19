pub fn command_guard_allows(
    required: bool,
    supported: bool,
    inspected: bool,
    single_linked: bool,
) -> bool {
    !required || (supported && inspected && single_linked)
}
