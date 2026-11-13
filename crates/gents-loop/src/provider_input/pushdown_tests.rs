use gents_protocol::message::{Message, UserContent};
use gents_protocol::request_input::RequestInput;

fn chat_attachments_supported(input: &RequestInput) -> bool {
    input
        .attachments
        .iter()
        .all(|a| crate::provider_input::chat_completions_attachment_supported(&a.media_type))
}

fn attachment_part(name: &str, part: UserContent) -> serde_json::Value {
    let UserContent::Text(text) = part else {
        return serde_json::to_value(part).unwrap();
    };
    use base64::Engine;
    let (header, body) = text
        .text
        .split_once("\n\n")
        .unwrap_or_else(|| panic!("{name}: text attachment has no header"));
    serde_json::json!({
        "type": "text",
        "prefix": format!("{header}\n\n"),
        "encoded_text": base64::engine::general_purpose::STANDARD.encode(body),
    })
}

#[test]
fn accepted_inputs_cross_the_provider_boundary_as_the_model_formats_them() {
    gents_lean_vocab::families::request_input_contracts_provider::replay(|case| {
        let input: RequestInput = gents_lean_vocab::production_request_input(&case.input);
        let native = input.authored_message(&case.authored_text).unwrap();
        let Message::User { content } =
            crate::provider_input::captured_context_for_provider(native)
                .unwrap_or_else(|error| panic!("{}: provider refused: {error}", case.name))
        else {
            panic!("{}: provider changed the role", case.name)
        };
        let prose = content.len() - input.attachments.len();
        let mut parts = content.into_iter();
        let provider_parts = parts
            .by_ref()
            .take(prose)
            .filter_map(|part| match part {
                UserContent::Text(text) => Some(text.text),
                _ => None,
            })
            .collect();
        gents_lean_vocab::families::request_input_contracts_provider::ProviderParts {
            chat_attachments_supported: chat_attachments_supported(&input),
            provider_parts,
            attachment_parts: gents_lean_vocab::family::Json(serde_json::Value::Array(
                parts
                    .map(|part| attachment_part(&case.name, part))
                    .collect(),
            )),
        }
    });
}

#[test]
fn refused_inputs_do_not_cross_the_provider_boundary() {
    gents_lean_vocab::families::request_input_contracts_provider_refusal::replay(|case| {
        let input: RequestInput = gents_lean_vocab::production_request_input(&case.input);
        let native = input.authored_message(&case.authored_text).unwrap();
        gents_lean_vocab::families::request_input_contracts_provider_refusal::ProviderRefused {
            chat_attachments_supported: chat_attachments_supported(&input),
            provider_accepted: crate::provider_input::captured_context_for_provider(native).is_ok(),
        }
    });
}
