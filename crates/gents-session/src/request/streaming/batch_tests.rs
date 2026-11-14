use super::*;

const BATCH: Duration = Duration::from_secs(10);

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Growth {
    None,
    FirstVisible,
    Visible,
    FirstReasoning,
    Reasoning,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum BatchAge {
    Fresh,
    Batch,
    ReasoningBatch,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum Timer {
    Idle,
    Batch,
    ReasoningBatch,
}

/// A buffer whose growth since its last flush is the modeled `growth`.
fn buffer(growth: &Growth, age: &BatchAge) -> StreamBuffer {
    let mut flushed = StreamBufferSnapshot::default();
    if matches!(growth, Growth::None | Growth::Visible) {
        flushed.record_content("earlier");
    }
    if matches!(growth, Growth::Reasoning) {
        flushed.record_reasoning("earlier");
    }
    let mut current = flushed.clone();
    match growth {
        Growth::None => {}
        Growth::FirstVisible | Growth::Visible => current.record_content("chunk"),
        Growth::FirstReasoning | Growth::Reasoning => current.record_reasoning("thought"),
    }
    let since = match age {
        BatchAge::Fresh => Duration::ZERO,
        BatchAge::Batch => BATCH,
        BatchAge::ReasoningBatch => BATCH.saturating_mul(REASONING_BATCH_FACTOR),
    };
    StreamBuffer {
        current,
        last_flush_at: Instant::now()
            .checked_sub(since)
            .expect("batch clock moved before process start"),
        persisted: flushed,
    }
}

#[test]
fn stream_batch_follows_the_lean_model() {
    use gents_lean_vocab::families::stream_batch_plan as family;

    family::replay::<BatchAge, Growth, Timer>(|input| {
        let buffer = buffer(&input.growth, &input.age);
        let timer = match buffer.flush_deadline(BATCH) {
            None => Timer::Idle,
            Some(deadline) if deadline - buffer.last_flush_at == BATCH => Timer::Batch,
            Some(deadline)
                if deadline - buffer.last_flush_at
                    == BATCH.saturating_mul(REASONING_BATCH_FACTOR) =>
            {
                Timer::ReasoningBatch
            }
            Some(deadline) => panic!(
                "unmodeled flush deadline {:?} after the last flush",
                deadline - buffer.last_flush_at
            ),
        };
        family::Plan {
            flush_on_write: buffer.due_snapshot(BATCH, false).is_some(),
            timer,
        }
    });
}
