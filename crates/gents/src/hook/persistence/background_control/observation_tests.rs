use super::*;

#[test]
fn generated_observation_cases_preserve_deadlines_and_closed_feed_recovery() {
    #[derive(serde::Deserialize)]
    struct Case {
        rescan_at: u64,
        feed_open: bool,
        now: u64,
        cancelled: bool,
        deadline_at: u64,
        change: Change,
        expected_wake: Wake,
        expected_feed_open: bool,
        expected_rescan_at: u64,
    }
    #[derive(serde::Deserialize)]
    struct Contracts {
        durable_observation_cases: Vec<Case>,
    }
    let contracts: Contracts = gents_lean_contract::load_contract_snapshot().unwrap();
    assert!(!contracts.durable_observation_cases.is_empty());
    let origin = tokio::time::Instant::now();
    for case in contracts.durable_observation_cases {
        let at = |offset| origin + std::time::Duration::from_millis(offset);
        let mut state = Observation::new(at(case.rescan_at), case.feed_open);
        assert_eq!(
            state.wake(
                at(case.now),
                at(case.deadline_at),
                case.cancelled,
                case.change
            ),
            case.expected_wake,
        );
        state.observe_change(case.change);
        assert_eq!(state.feed_open(), case.expected_feed_open);
        assert_eq!(state.rescan_at(), at(case.expected_rescan_at));
    }
}
