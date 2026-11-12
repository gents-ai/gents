use gents_lean_vocab::lean_contract_snapshot;
use gents_protocol::request_input::RequestInput;

#[test]
fn captured_native_documents_and_provider_parts_match_the_model() {
    use gents_protocol::message::{Message, UserContent};
    for case in &lean_contract_snapshot().request_input_cases {
        let name = case["name"].as_str().unwrap();
        let input: RequestInput = serde_json::from_value(case["input"].clone()).unwrap();
        assert_eq!(
            input.has_authored_input(case["authored_text"].as_str().unwrap()),
            case["expected_has_authored_input"].as_bool().unwrap(),
            "{name}: user input presence"
        );
        let authored = gents_protocol::request_input::AuthoredRequestInput {
            selected_skill_ids: input.selected_skill_ids.clone(),
            captured_context: input.captured_context.clone(),
            attachments: input.attachments.clone(),
        };
        assert_eq!(
            authored.has_authored_input(case["authored_text"].as_str().unwrap()),
            case["expected_has_authored_input"].as_bool().unwrap(),
            "{name}: replacement input presence"
        );
        let native = input
            .authored_message(case["authored_text"].as_str().unwrap())
            .unwrap();
        assert_eq!(
            serde_json::to_value(&native).unwrap(),
            case["expected_native_input"],
            "{name}"
        );
        let contexts = gents_protocol::request_input::CapturedContext::from_message(&native);
        let provider = crate::provider_input::captured_context_for_provider(native);
        if case["expected_provider_parts"].is_null() || !input.attachments_well_formed() {
            if case["expected_provider_parts"].is_null() {
                assert!(contexts.is_err(), "{name}: invalid document capture");
            }
            assert!(
                provider.is_err(),
                "{name}: invalid capture crossed provider boundary"
            );
            continue;
        }
        assert_eq!(
            contexts.unwrap(),
            input.captured_context,
            "{name}: immutable document codec"
        );
        let provider = provider.unwrap();
        let Message::User { content } = provider else {
            panic!("{name}: provider changed the role")
        };
        let prose_parts = content.len() - input.attachments.len();
        let texts = content
            .into_iter()
            .take(prose_parts)
            .filter(|part| matches!(part, UserContent::Text(_)))
            .map(|part| match part {
                UserContent::Text(text) => text.text,
                UserContent::Image(_) | UserContent::Document(_) => String::new(),
                _ => panic!("{name}: unexpected provider part"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            serde_json::to_value(texts).unwrap(),
            case["expected_provider_parts"],
            "{name}"
        );
    }
}

#[test]
fn attached_media_matches_the_executable_input_owner() {
    for case in &lean_contract_snapshot().request_input_cases {
        let input: RequestInput = serde_json::from_value(case["input"].clone()).unwrap();
        assert_eq!(
            input.attachments_well_formed(),
            case["expected_attachments_well_formed"].as_bool().unwrap()
        );
        assert_eq!(
            input.attachments.iter().all(|a| {
                crate::provider_input::chat_completions_attachment_supported(&a.media_type)
            }),
            case["expected_chat_attachments_supported"]
                .as_bool()
                .unwrap()
        );
        let message = input
            .authored_message(case["authored_text"].as_str().unwrap())
            .unwrap();
        let provider = crate::provider_input::captured_context_for_provider(message);
        if !input.attachments_well_formed() || case["expected_provider_parts"].is_null() {
            assert!(provider.is_err());
            continue;
        }
        let provider = provider.unwrap();
        let gents_protocol::message::Message::User { content } = provider else {
            panic!()
        };
        let skip = content.len() - input.attachments.len();
        let media: Vec<_> = content.into_iter().skip(skip).collect();
        let mut expected = case["expected_attachment_parts"].clone();
        for part in expected.as_array_mut().unwrap() {
            if let Some(encoded) = part["encoded_text"].as_str() {
                use base64::Engine;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .unwrap();
                *part = serde_json::json!({"type":"text", "text":format!("{}{}", part["prefix"].as_str().unwrap(), String::from_utf8(bytes).unwrap())});
            }
        }
        assert_eq!(
            serde_json::to_value(media).unwrap(),
            expected,
            "{}",
            case["name"]
        );
    }
}
