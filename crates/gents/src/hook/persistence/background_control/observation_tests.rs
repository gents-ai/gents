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

#[tokio::test(start_paused = true)]
async fn closed_feed_is_polled_once_and_still_reaches_the_durable_rescan() {
    let start = tokio::time::Instant::now();
    let interval = std::time::Duration::from_secs(5);
    let polls = std::cell::Cell::new(0);
    let mut changes = futures::stream::poll_fn(|_| {
        polls.set(polls.get() + 1);
        std::task::Poll::Ready(None)
    });
    let mut observation = Observation::new(start + interval, true);
    let deadline = start + std::time::Duration::from_secs(20);
    observation
        .wait(&mut changes, deadline, interval, |_| true)
        .await;
    assert!(!observation.feed_open());
    assert_eq!(tokio::time::Instant::now(), start);
    observation
        .wait(&mut changes, deadline, interval, |_| true)
        .await;
    assert_eq!(tokio::time::Instant::now(), start + interval);
    assert_eq!(polls.get(), 1);
    assert_eq!(observation.rescan_at(), start + interval * 2);
}

#[tokio::test(start_paused = true)]
async fn ready_unrelated_traffic_cannot_postpone_the_actual_timer() {
    let start = tokio::time::Instant::now();
    let interval = std::time::Duration::from_secs(5);
    let mut changes = futures::stream::poll_fn(|_| {
        std::task::Poll::Ready(Some(events::DocumentChangeBatch {
            changes: Vec::new(),
            resync_required: false,
            updates: 1,
        }))
    });
    let mut observation = Observation::new(start + interval, true);
    let wait = async {
        observation
            .wait(&mut changes, start + interval * 4, interval, |_| false)
            .await;
        tokio::time::Instant::now()
    };
    let advance = async {
        for _ in 0..20 {
            tokio::time::advance(std::time::Duration::from_secs(1)).await;
            tokio::task::yield_now().await;
        }
    };
    let (finished, ()) = tokio::join!(wait, advance);
    assert_eq!(finished, start + interval);
}
