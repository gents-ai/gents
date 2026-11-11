#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TriggerKind {
    Schedule,
    Event,
    Manual,
}

impl TriggerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TriggerKind::Schedule => "schedule",
            TriggerKind::Event => "event",
            TriggerKind::Manual => "manual",
        }
    }
}

pub fn validate_task_delivery_options(
    kind: TriggerKind,
    emit_outcome: bool,
    concurrency: crate::ConcurrencyMode,
    target_existing: bool,
) -> anyhow::Result<()> {
    if kind == TriggerKind::Event {
        return Ok(());
    }
    anyhow::ensure!(
        !emit_outcome,
        "emit_outcome requires document-triggered delivery or explicit CLI/desktop Task admission"
    );
    if kind == TriggerKind::Schedule {
        anyhow::ensure!(
            target_existing || concurrency != crate::ConcurrencyMode::QueuedSerial,
            "queued_serial requires an explicit session_id_template for scheduled delivery"
        );
    } else {
        anyhow::ensure!(
            concurrency != crate::ConcurrencyMode::QueuedSerial,
            "queued_serial requires a document event source"
        );
        anyhow::ensure!(
            !target_existing,
            "session_id_template requires a document event source"
        );
    }
    Ok(())
}
