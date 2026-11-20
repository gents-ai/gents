use super::*;

#[test]
fn successful_streaks_follow_the_executable_lean_owner() {
    for case in gents_lean_vocab::families::repeated_tool_success_contracts_streak::cases() {
        let mut guard = RepeatedToolProgress::new(DEFAULT_REPEATED_TOOL_FAILURE_LIMIT);
        let mut counts = Vec::new();
        for (index, event) in case.input.events.iter().enumerate() {
            if guard.success_stop_reason().is_some() {
                break;
            }
            let outcome = match &event.result {
                Some(result) => ToolOutcome::Completed(format!(
                    "gents_exec: {{\"ok\":true,\"duration_ms\":{},\"hint\":\"hint {}\"}}\nstdout:\nresult {}\nstderr:\n(empty)",
                    result.volatile, result.volatile, result.stable,
                )),
                None => match index % 4 {
                    0 => ToolOutcome::Cancelled,
                    1 => ToolOutcome::TimedOut { deadline_at: None },
                    2 => ToolOutcome::tool_returned_error("ordinary failure"),
                    _ => ToolOutcome::Failed {
                        class: FailureClass::PolicyDenied, denial: None, text: "denied".into(),
                    },
                },
            };
            guard.record_dispatched(
                &format!("tool-{}", event.tool),
                &event.args.to_string(),
                &outcome,
                event.command_envelope,
            );
            counts.push(
                guard
                    .streak
                    .as_ref()
                    .filter(|s| s.kind == StreakKind::Success)
                    .map_or(0, |s| u64::from(s.count)),
            );
        }
        case.check(
            gents_lean_vocab::families::repeated_tool_success_contracts_streak::Output {
                counts,
                stopped: guard.success_stop_reason().is_some(),
            },
        );
    }
}
