use super::*;

#[test]
fn gateways_conform_to_lean() {
    for case in gents_lean_vocab::families::tool_presentation_gateways::cases() {
        case.check(is_gateway(&case.input));
    }
}
