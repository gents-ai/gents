use super::*;

#[test]
fn generated_observation_cases_preserve_deadlines_and_closed_feed_recovery() {
    let origin = tokio::time::Instant::now();
    let at = |offset| origin + std::time::Duration::from_millis(offset);
    gents_lean_vocab::families::native_refinement_observation::replay::<Change, Wake>(|i| {
        let mut state = Observation::new(at(i.rescan_at), i.feed_open);
        let wake = state.wake(at(i.now), at(i.deadline_at), i.cancelled, i.change);
        state.observe_change(i.change);
        gents_lean_vocab::families::native_refinement_observation::ObservationOutput {
            wake,
            feed_open: state.feed_open(),
            rescan_at: u64::try_from((state.rescan_at() - origin).as_millis()).unwrap(),
        }
    });
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
