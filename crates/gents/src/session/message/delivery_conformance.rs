use super::*;

#[test]
fn agent_delivery_matches_lean() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().agent_delivery_cases;
    assert_eq!(cases.len(), 12);
    for case in cases {
        let mut input = serde_json::json!({
            "session_id": "target", "message": "hello",
            "delivery": case.delivery, "interrupt": case.interrupt
        });
        if case.interrupt.is_none() {
            input.as_object_mut().unwrap().remove("interrupt");
        }
        let args: AgentMessageArgs = serde_json::from_value(input).unwrap();
        let actual = args.delivery().ok().map(|mode| match mode {
            AgentMessageDelivery::Queue => "queue",
            AgentMessageDelivery::Steer => "steer",
            AgentMessageDelivery::Interrupt => "interrupt",
        });
        assert_eq!(actual, case.expected.as_deref(), "{case:?}");
    }
}
