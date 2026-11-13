#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
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

#[cfg(test)]
mod conformance {
    use super::{validate_task_delivery_options, TriggerKind};
    use crate::ConcurrencyMode;
    use gents_lean_vocab::families::trigger_delivery_delivery_options as family;

    #[test]
    fn delivery_options_match_the_lean_owner() {
        family::replay::<ConcurrencyMode, TriggerKind>(|i| {
            validate_task_delivery_options(i.kind, i.emit_outcome, i.concurrency, i.target_existing)
                .is_ok()
        });
    }
}
