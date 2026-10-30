use super::*;
use gents_protocol::request_input::RequestInput;

#[test]
fn captured_input_compatibility_matches_the_executable_owner() {
    for case in &lean_contract_snapshot().request_input_cases {
        let input: RequestInput = serde_json::from_value(case["input"].clone()).unwrap();
        assert_eq!(
            input.captured_input_compatible(case["runtime_input_version"].as_u64()),
            case["expected_captured_input_compatible"]
                .as_bool()
                .unwrap(),
            "{}: runtime input compatibility",
            case["name"],
        );
    }
}

#[test]
fn goal_assignment_compatibility_matches_the_executable_owner() {
    for case in &lean_contract_snapshot().request_input_cases {
        let input: RequestInput = serde_json::from_value(case["input"].clone()).unwrap();
        assert_eq!(
            input.goal_assignment_compatible(case["runtime_goal_version"].as_u64()),
            case["expected_goal_assignment_compatible"]
                .as_bool()
                .unwrap(),
            "{}",
            case["name"]
        );
    }
}

#[test]
fn runtime_continuation_inputs_match_the_lean_owner() {
    for case in &lean_contract_snapshot().request_input_cases {
        let name = case["name"].as_str().unwrap();
        let parent: RequestInput = serde_json::from_value(case["input"].clone()).unwrap();
        let queue = serde_json::from_value(case["continuation_queue"].clone()).unwrap();
        let goal = serde_json::from_value(case["continuation_goal"].clone()).unwrap();
        let expected: RequestInput =
            serde_json::from_value(case["expected_continuation"].clone()).unwrap();
        assert_eq!(
            parent.continuation(
                queue,
                goal,
                case["continuation_parent_doc_id"].as_str().unwrap()
            ),
            expected,
            "{name}"
        );
        let expected_fresh: RequestInput =
            serde_json::from_value(case["expected_fresh_input"].clone()).unwrap();
        assert_eq!(
            parent.for_fresh_session(),
            expected_fresh,
            "{name}: fresh-session input"
        );
        let expected_reissued: RequestInput =
            serde_json::from_value(case["expected_reissued_input"].clone()).unwrap();
        assert_eq!(
            parent.reissued(case["reissued_source_doc_id"].as_str().unwrap()),
            expected_reissued,
            "{name}: same-session reissuance"
        );
    }
}

#[test]
fn lean_request_inputs_decode_without_losing_explicit_issuance_facts() {
    let cases = &lean_contract_snapshot().request_input_cases;
    assert!(!cases.is_empty());
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let input: RequestInput = serde_json::from_value(case["input"].clone())
            .unwrap_or_else(|error| panic!("{name}: typed input: {error}"));
        let encoded = serde_json::to_value(&input).unwrap();
        for (field, value) in case["input"].as_object().unwrap() {
            if !value.is_null() && value.as_array().is_none_or(|xs| !xs.is_empty()) {
                if field == "queue" {
                    let modeled: gents_protocol::request_input::RequestQueue =
                        serde_json::from_value(value.clone()).unwrap();
                    let decoded = input.queue.as_ref().expect("modeled queue survives decode");
                    assert_eq!(decoded.delivery, modeled.delivery, "{name}: queue.delivery");
                    assert_eq!(decoded.position, modeled.position, "{name}: queue.position");
                    for (key, value) in value.as_object().unwrap() {
                        if key == "delivery" && modeled.delivery.is_queue() {
                            assert!(
                                encoded[field].get(key).is_none(),
                                "{name}: compact default delivery"
                            );
                        } else if !value.is_null() {
                            assert_eq!(&encoded[field][key], value, "{name}: queue.{key}");
                        }
                    }
                } else {
                    assert_eq!(&encoded[field], value, "{name}: {field}");
                }
            }
        }
        if let Some(goal) = input.goal_continuation {
            assert_eq!(
                encoded["goal_continuation"]["wrapup"], goal.wrapup,
                "{name}: false is an explicit original issuance fact"
            );
        }
    }
}

#[test]
fn request_input_rejects_legacy_controls_and_incomplete_goal_facts() {
    for value in [
        json!({"temperature": 0.7}),
        json!({"metadata": {"queue": {"source": "goal"}}}),
        json!({"skill_excludes": ["configured-skill"]}),
        json!({"goal_continuation": {"sequence": 1}}),
        json!({"goal_continuation": {"wrapup": false}}),
        json!({"goal_continuation": {"sequence": 1, "wrapup": false, "goal_id": "forged"}}),
    ] {
        assert!(
            serde_json::from_value::<RequestInput>(value.clone()).is_err(),
            "{value}"
        );
    }
    let compact = serde_json::to_value(RequestInput::default()).unwrap();
    assert_eq!(compact, json!({}));
}

#[test]
fn signed_request_input_fields_match_lean_bytes() {
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};

    fn read_length(cursor: &mut &[u8]) -> usize {
        let length = cursor.iter().position(|byte| *byte != 0).unwrap();
        assert_eq!(cursor[length], 255, "invalid canonical length delimiter");
        *cursor = &cursor[length + 1..];
        length
    }

    for case in &lean_contract_snapshot().request_input_cases {
        let name = case["name"].as_str().unwrap();
        let mut request = AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            "request-1",
            "did:key:agent",
            "did:key:agent",
            "default",
            "session-1",
            "input-fields-follow-this-content",
            "interactive",
            "2026-08-30T00:00:00Z",
            AgentRequestAdmissionRecord::local_self("did:key:agent"),
        );
        request.input = serde_json::from_value(case["input"].clone()).unwrap();
        let expected: Vec<Vec<u8>> =
            serde_json::from_value(case["canonical_input_fields"].clone()).unwrap();
        let payload = request.signing_payload();
        let mut cursor = payload.as_slice();
        let count = read_length(&mut cursor);
        let mut fields = Vec::new();
        for _ in 0..count {
            let length = read_length(&mut cursor);
            fields.push(cursor[..length].to_vec());
            cursor = &cursor[length..];
        }
        assert!(
            cursor.is_empty(),
            "{name}: trailing signature payload bytes"
        );
        let start = fields
            .iter()
            .position(|field| field == b"input-fields-follow-this-content")
            .unwrap()
            + 1;
        assert_eq!(
            &fields[start..start + expected.len()],
            expected.as_slice(),
            "{name}: real signing owner must use Lean's typed-input framing"
        );
        assert_eq!(fields[start + expected.len()], vec![1], "{name}");
        assert_eq!(fields[start + expected.len() + 1], b"interactive", "{name}");
    }
}

#[test]
fn lean_workspace_references_preserve_exact_owner_and_attenuate_authority() {
    fn lineage(value: &serde_json::Value) -> gents::request::lifecycle::WorkspaceLineage {
        let field = |name: &str| value[name].as_str().map(str::to_owned);
        gents::request::lifecycle::WorkspaceLineage {
            workspace_id: field("workspace_id"),
            workspace_owner_node_did: field("workspace_owner_node_did"),
            workspace_authority: field("workspace_authority"),
            workspace_seal_hash: field("workspace_seal_hash"),
        }
    }
    let cases = &lean_contract_snapshot().request_input_cases;
    assert!(cases
        .iter()
        .any(|case| case["name"] == "workspace-foreign-owner-same-label-denied"));
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let request = lineage(&case["workspace"]);
        let source = lineage(&case["workspace_source"]);
        assert_eq!(
            request
                .validate_source(
                    &source,
                    case["workspace_source_authenticated"].as_bool().unwrap(),
                )
                .is_ok(),
            case["expected_workspace_accepted"].as_bool().unwrap(),
            "{name}: actual workspace source owner"
        );
    }
}

#[test]
fn captured_context_structure_matches_the_executable_input_owner() {
    let cases = &lean_contract_snapshot().request_input_cases;
    assert!(cases
        .iter()
        .any(|case| case["name"] == "captured-selection"));
    for case in cases {
        let input: RequestInput = serde_json::from_value(case["input"].clone()).unwrap();
        assert_eq!(
            input
                .captured_context
                .iter()
                .all(|context| context.is_well_formed()),
            case["expected_captures_well_formed"].as_bool().unwrap(),
            "{}: actual captured-context structural owner",
            case["name"],
        );
    }
}

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
        let native = input
            .authored_message(case["authored_text"].as_str().unwrap())
            .unwrap();
        assert_eq!(
            serde_json::to_value(&native).unwrap(),
            case["expected_native_input"],
            "{name}"
        );
        let contexts = gents_protocol::request_input::CapturedContext::from_message(&native);
        let provider = gents_loop::provider_input::captured_context_for_provider(native);
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
                gents_loop::provider_input::chat_completions_attachment_supported(&a.media_type)
            }),
            case["expected_chat_attachments_supported"]
                .as_bool()
                .unwrap()
        );
        let message = input
            .authored_message(case["authored_text"].as_str().unwrap())
            .unwrap();
        let provider = gents_loop::provider_input::captured_context_for_provider(message);
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
