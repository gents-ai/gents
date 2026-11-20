#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
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

/// Lean `Triggers.CommandRun.deliveryAllowed`: what a command Task's fire
/// refuses, so publication refuses a Trigger every fire of which would.
pub fn validate_command_delivery(kind: TriggerKind, target_existing: bool) -> anyhow::Result<()> {
    anyhow::ensure!(
        kind != TriggerKind::Event,
        "a command Task runs from a schedule or manual fire, not an event Trigger"
    );
    anyhow::ensure!(
        !target_existing,
        "a command Task has no session; remove the Trigger's session_id_template"
    );
    Ok(())
}

#[cfg(test)]
mod conformance {
    use super::{validate_command_delivery, validate_task_delivery_options, TriggerKind};
    use crate::ConcurrencyMode;
    use gents_lean_vocab::families::trigger_delivery_delivery_options as family;

    #[test]
    fn command_delivery_matches_the_lean_owner() {
        gents_lean_vocab::families::triggers_command_run_delivery_allowed::replay(|i| {
            validate_command_delivery(i.kind, i.target_existing).is_ok()
        });
    }

    #[test]
    fn delivery_options_match_the_lean_owner() {
        family::replay::<ConcurrencyMode, TriggerKind>(|i| {
            validate_task_delivery_options(i.kind, i.emit_outcome, i.concurrency, i.target_existing)
                .is_ok()
        });
    }
}
