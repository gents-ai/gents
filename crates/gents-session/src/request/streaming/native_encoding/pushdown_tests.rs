use gents_inference::llm::message::{AssistantContent, Message, Reasoning};
use gents_loop::stream_processor::AssistantTurnAccumulator;

#[test]
fn coalesced_reasoning_fragments_encode_as_one_native_stream() {
    use gents_inference::llm::message::ReasoningContent;
    use crate::request::streaming::native_encoding::encode_native_message;

    let mut accumulator = AssistantTurnAccumulator::default();
    for _ in 0..128 {
        accumulator.push_reasoning_delta(Some("reasoning".into()), "fragment 🦀 ");
    }
    let fragmented = Message::Assistant {
        request_id: None,
        id: None,
        content: vec![AssistantContent::Reasoning(Reasoning {
            id: Some("reasoning".into()),
            content: (0..128)
                .map(|_| ReasoningContent::Text {
                    text: "fragment 🦀 ".into(),
                    signature: None,
                })
                .collect(),
        })],
    };
    let before = encode_native_message(&fragmented).unwrap();
    let after = encode_native_message(&accumulator.take_message().unwrap()).unwrap();
    assert_eq!((before.streams.len(), after.streams.len()), (128, 1));
    let before =
        serde_json::to_vec(&before.build_blocks("closing-document", &[]).unwrap()).unwrap();
    let after = serde_json::to_vec(&after.build_blocks("closing-document", &[]).unwrap()).unwrap();
    assert!(
        after.len() * 32 < before.len(),
        "{} vs {}",
        after.len(),
        before.len()
    );
}
