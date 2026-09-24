#[allow(dead_code)]
#[path = "../../../gents/src/lean_vocab_test/canonical_output.rs"]
mod runtime_contract;

use super::test_protocol::message::{AssistantContent, Message, Reasoning, ReasoningContent};
use runtime_contract::{
    LeanCanonicalOutputProjectionCase, LeanCanonicalOutputView, LeanMessageBlock,
    LeanReasoningPart, LeanRenderedKind,
};

pub(crate) struct RenderedReasoningCase {
    pub(crate) name: String,
    pub(crate) message: Message,
    pub(crate) rendered_kinds: Vec<LeanRenderedKind>,
    pub(crate) expected_text: String,
}

fn utf8(bytes: &[u8], name: &str) -> String {
    String::from_utf8(bytes.to_vec())
        .unwrap_or_else(|error| panic!("{name}: modeled native payload is not UTF-8: {error}"))
}

pub(crate) fn generated_reasoning_cases() -> Vec<RenderedReasoningCase> {
    const MIXED: &str = "published_mixed_reasoning_renders_text_and_summary_only";
    const OPAQUE: &str = "published_all_opaque_reasoning_renders_nothing";

    let snapshot: serde_json::Value =
        gents_lean_contract::load_contract_snapshot().expect("generate Lean contract");
    let cases: Vec<LeanCanonicalOutputProjectionCase> =
        serde_json::from_value(snapshot["canonical_output_projection_cases"].clone())
            .expect("strict canonical output projection cases");
    let mut selected = Vec::new();
    for case in cases {
        if case.name != MIXED && case.name != OPAQUE {
            continue;
        }
        let LeanCanonicalOutputView::Published { message, native } = &case.expected else {
            panic!("{}: generated reasoning case is not published", case.name);
        };
        assert!(
            case.input
                .messages
                .iter()
                .any(|candidate| candidate == message),
            "{}: selected modeled header is not an input fact",
            case.name
        );
        assert!(
            matches!(&native.role, runtime_contract::LeanMessageRole::Assistant),
            "{}: generated reasoning message is not assistant-authored",
            case.name
        );
        let [LeanMessageBlock::Reasoning { id, parts }] = native.blocks.as_slice() else {
            panic!(
                "{}: test-only presentation boundary requires one reasoning block",
                case.name
            );
        };
        assert!(
            parts
                .iter()
                .any(|part| matches!(part, LeanReasoningPart::Encrypted { .. }))
                && parts
                    .iter()
                    .any(|part| matches!(part, LeanReasoningPart::Redacted { .. })),
            "{}: fixture must exercise both opaque native reasoning variants",
            case.name
        );

        let mut content = Vec::new();
        let mut expected_pieces = Vec::new();
        let mut next_kind = 0;
        for part in parts {
            let (kind, value, native_part) = match part {
                LeanReasoningPart::Text { payload, signature } => {
                    let value = utf8(payload, &case.name);
                    (
                        Some(LeanRenderedKind::Reasoning),
                        value.clone(),
                        ReasoningContent::Text {
                            text: value,
                            signature: signature.clone(),
                        },
                    )
                }
                LeanReasoningPart::Summary { payload } => {
                    let value = utf8(payload, &case.name);
                    (
                        Some(LeanRenderedKind::Summary),
                        value.clone(),
                        ReasoningContent::Summary(value),
                    )
                }
                LeanReasoningPart::Encrypted { payload } => {
                    let value = utf8(payload, &case.name);
                    (None, value.clone(), ReasoningContent::Encrypted(value))
                }
                LeanReasoningPart::Redacted { payload } => {
                    let value = utf8(payload, &case.name);
                    (
                        None,
                        value.clone(),
                        ReasoningContent::Redacted { data: value },
                    )
                }
            };
            if kind.as_ref() == case.rendered_kinds.get(next_kind) && kind.is_some() {
                expected_pieces.push(value);
                next_kind += 1;
            }
            content.push(native_part);
        }
        assert_eq!(
            next_kind,
            case.rendered_kinds.len(),
            "{}: rendered kind oracle has no ordered native counterpart",
            case.name
        );
        let reasoning = Reasoning {
            id: id.clone(),
            content,
        };
        selected.push(RenderedReasoningCase {
            name: case.name,
            message: Message::Assistant {
                id: native.native_id.clone(),
                content: vec![AssistantContent::Reasoning(reasoning)],
            },
            rendered_kinds: case.rendered_kinds,
            expected_text: expected_pieces.join("\n"),
        });
    }
    selected.sort_by(|left, right| left.name.cmp(&right.name));
    assert_eq!(
        selected
            .iter()
            .map(|case| case.name.as_str())
            .collect::<Vec<_>>(),
        vec![OPAQUE, MIXED],
        "both generated opaque/mixed presentation witnesses must be exercised"
    );
    assert!(
        selected[0].rendered_kinds.is_empty() && !selected[1].rendered_kinds.is_empty(),
        "generated cases must cover both all-opaque and mixed reasoning"
    );
    selected
}
