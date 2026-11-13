use gents_lean_vocab::lean_contract_snapshot;

#[test]
fn generated_causal_hop_cases_match_native_materializer() {
    use gents_protocol::request_lifecycle::{
        next_request_hop, request_hop_within_bound, RequestHopCapObservation, RequestHopCause,
    };

    let contract = lean_contract_snapshot();
    assert_eq!(contract.causal_hop_default_max_request_hop, None);
    assert!(contract
        .causal_hop_cap_observation_cases
        .iter()
        .any(|case| !case.visible));
    for case in &contract.causal_hop_cap_observation_cases {
        let observation = if case.visible {
            RequestHopCapObservation::Visible(case.cap)
        } else {
            RequestHopCapObservation::Unavailable
        };
        assert_eq!(observation.admit(case.hop), case.expected, "{}", case.name);
        assert_eq!(observation.bound(), case.expected_bound, "{}", case.name);
        if case.hop > 0 {
            assert_eq!(
                request_hop_within_bound(observation.bound(), case.hop),
                case.expected.unwrap_or(false),
                "{}",
                case.name
            );
        }
    }
    let cause = |name: &str, cause_hop: Option<u32>| match name {
        "root" => RequestHopCause::Root,
        "cross_session" => RequestHopCause::CrossSession {
            cause_hop: cause_hop.expect("cross-session step names its cause hop"),
        },
        "return" => RequestHopCause::Return,
        "continuation" => RequestHopCause::Continuation,
        other => panic!("unknown Lean causal-hop cause {other:?}"),
    };
    assert!(!contract.causal_hop_chain_cases.is_empty());
    for chain in &contract.causal_hop_chain_cases {
        let mut hop = 0;
        let mut hops = Vec::new();
        let mut admitted = Vec::new();
        for step in &chain.steps {
            hop = match step.kind.as_str() {
                "cross_session" => next_request_hop(
                    RequestHopCause::CrossSession { cause_hop: hop },
                    step.own_predecessor_hop
                        .expect("cross-session step names its own predecessor hop"),
                ),
                other => next_request_hop(cause(other, None), hop),
            };
            hops.push(hop);
            admitted.push(request_hop_within_bound(chain.max_request_hop, hop));
        }
        assert_eq!(hops, chain.expected_hops, "{}", chain.name);
        assert_eq!(admitted, chain.expected_admitted, "{}", chain.name);
    }
    assert!(!contract.causal_hop_call_cases.is_empty());
    for call in &contract.causal_hop_call_cases {
        let (mut a, mut b) = (0, 0);
        let mut hops = Vec::new();
        for event in &call.events {
            let (target, cause) = match event.as_str() {
                "a_sends_b" => (&mut b, RequestHopCause::CrossSession { cause_hop: a }),
                "b_sends_a" => (&mut a, RequestHopCause::CrossSession { cause_hop: b }),
                "return_to_a" => (&mut a, RequestHopCause::Return),
                "return_to_b" => (&mut b, RequestHopCause::Return),
                other => panic!("unknown Lean call event {other:?}"),
            };
            *target = next_request_hop(cause, *target);
            hops.push(*target);
        }
        let admitted = hops
            .iter()
            .map(|hop| request_hop_within_bound(call.max_request_hop, *hop))
            .collect::<Vec<_>>();
        assert_eq!(hops, call.expected_hops, "{}", call.name);
        assert_eq!(admitted, call.expected_admitted, "{}", call.name);
    }
}
