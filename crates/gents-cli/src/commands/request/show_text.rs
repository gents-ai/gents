use crate::request_helpers::CliOutputObservation;

use super::show_views::RequestShowSnapshot;

pub(super) fn render_request_show_text(
    snapshot: &RequestShowSnapshot,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    let mut lines = Vec::new();
    let request = &snapshot.request;
    lines.push(format!("Request {}", request.request_id));
    lines.push(format!("state: {}", request.lifecycle_state));
    lines.push(format!("node_did: {}", request.node_did));
    lines.push(format!("agent_id: {}", request.agent_id));
    lines.push(format!("session_id: {}", request.session_id));
    push_option_line(&mut lines, "backend_id", request.backend_id.as_deref());
    push_option_line(
        &mut lines,
        "execution_origin",
        request.execution_origin.as_deref(),
    );
    push_option_line(&mut lines, "created_at", request.created_at.as_deref());
    push_option_line(&mut lines, "claimed_at", request.claimed_at.as_deref());
    push_option_line(&mut lines, "deadline", request.deadline.as_deref());
    push_option_line(&mut lines, "valid_until", request.valid_until.as_deref());
    push_option_line(
        &mut lines,
        "failure_reason",
        request.failure_reason.as_deref(),
    );
    if let Some(blocked) = &snapshot.blocked {
        use gents::goal::blocked_turn::BlockedReason;
        let mut line = format!(
            "blocked: {}",
            match blocked.reason {
                BlockedReason::UsageLimit => "usage limit",
                BlockedReason::AccountDisabled => "account disabled",
                BlockedReason::AccountRemoved => "account removed",
                BlockedReason::AccountSignedOut => "account signed out",
            }
        );
        if let Some(account) = &blocked.account {
            line.push_str(&format!(" on account {:?}", account.label));
        }
        if blocked.reason == BlockedReason::UsageLimit {
            line.push_str(&match blocked.resets_at {
                Some(at) => {
                    let secs = (at - now).num_seconds();
                    let at = at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                    if secs > 0 {
                        format!(
                            ", resets at {at} (in {})",
                            super::super::accounts::short_duration(secs)
                        )
                    } else {
                        format!(", reset at {at} (passed)")
                    }
                }
                None => ", reset not reported".to_owned(),
            });
        }
        lines.push(line);
        if let (Some(profile), Some(switch)) = (&blocked.profile, &blocked.switch_command) {
            let users = blocked.agents_on_profile.len();
            let noun = if users == 1 { "agent" } else { "agents" };
            lines.push(format!(
                "  profile {profile} (used by {users} {noun}); switch: {switch}"
            ));
        }
    }
    push_option_line(
        &mut lines,
        "terminal_cause",
        request.terminal_cause.as_deref(),
    );

    lines.push(String::new());
    lines.push("Canonical output:".to_string());
    push_request_output_text(&mut lines, &snapshot.output);

    lines.push(String::new());
    lines.push("Transition history:".to_string());
    if request.transition_history.is_empty() {
        lines.push("  none observed".to_string());
    } else {
        for transition in &request.transition_history {
            let at = transition.at.as_deref().unwrap_or("unknown");
            let state_change = transition
                .from
                .as_deref()
                .map(|from| format!("{from} -> {}", transition.to))
                .unwrap_or_else(|| transition.to.clone());
            let inferred = if transition.inferred { " inferred" } else { "" };
            let note = transition
                .note
                .as_deref()
                .map(|note| format!("; {note}"))
                .unwrap_or_default();
            lines.push(format!(
                "  - {}: {} at {} (source={}{}{})",
                transition.action, state_change, at, transition.source, inferred, note
            ));
        }
    }

    if let Some(cancel) = &snapshot.cancel_cause {
        lines.push(String::new());
        lines.push("CancelCause:".to_string());
        lines.push(format!("  cause: {}", cancel.cause));
        lines.push(format!(
            "  interrupt_requested_at: {}",
            cancel
                .interrupt_requested_at
                .as_deref()
                .unwrap_or("unknown")
        ));
    }

    lines.push(String::new());
    lines.push("Tool calls:".to_string());
    if snapshot.tool_calls.is_empty() {
        lines.push("  none".to_string());
    } else {
        for tool in &snapshot.tool_calls {
            lines.push(format!("  - {} ({})", tool.tool_call_id, tool.tool_name));
            lines.push(format!(
                "    state: {} (status: {})",
                tool.state, tool.status
            ));
            lines.push(format!("    await_mode: {}", tool.await_mode));
            lines.push(format!("    cancel_cause: {}", tool.cancel_cause));
            lines.push(format!(
                "    started_at: {}",
                tool.started_at.as_deref().unwrap_or("unknown")
            ));
            lines.push(format!(
                "    completed_at: {}",
                tool.completed_at.as_deref().unwrap_or("unknown")
            ));
            lines.push(format!(
                "    active_tool_call: {}",
                yes_no(tool.active_tool_call)
            ));
            lines.push(format!(
                "    active_native_executors: {}",
                tool.active_native_executor_count
            ));
        }
    }

    lines.push(String::new());
    lines.push("Backgrounded tools:".to_string());
    if snapshot.backgrounded_tools.is_empty() {
        lines.push("  none".to_string());
    } else {
        for tool in &snapshot.backgrounded_tools {
            lines.push(format!(
                "  - {} ({}) state={} await_mode={} active_tool_call={} active_native_executors={}",
                tool.tool_call_id,
                tool.tool_name,
                tool.state,
                tool.await_mode,
                yes_no(tool.active_tool_call),
                tool.active_native_executor_count
            ));
        }
    }

    if snapshot.native_executors_available && !snapshot.native_executors.is_empty() {
        lines.push(String::new());
        lines.push("Native executors:".to_string());
        for executor in &snapshot.native_executors {
            lines.push(format!(
                "  - pid={} tool_name={} started_at={} kill_signaled_at={} reaped_at={} exit_code={}",
                display_i64(executor.pid),
                executor.tool_name.as_deref().unwrap_or("unknown"),
                executor.started_at.as_deref().unwrap_or("unknown"),
                executor.kill_signaled_at.as_deref().unwrap_or("unknown"),
                executor.reaped_at.as_deref().unwrap_or("unknown"),
                display_i64(executor.exit_code)
            ));
        }
    }

    lines.push(String::new());
    lines.push("Child requests:".to_string());
    if snapshot.child_requests.is_empty() {
        lines.push("  none".to_string());
    } else {
        for child in &snapshot.child_requests {
            lines.push(format!(
                "  - {} state={} agent_id={} node_did={} session_id={}",
                child.request_id, child.state, child.agent_id, child.node_did, child.session_id
            ));
            push_option_line(
                &mut lines,
                "    caused_by_parent_tool_call_id",
                child.caused_by_parent_tool_call_id.as_deref(),
            );
        }
    }

    lines.push(String::new());
    lines.join("\n")
}

pub(super) fn push_request_output_text(lines: &mut Vec<String>, output: &CliOutputObservation) {
    let (state, presentation) = match output {
        CliOutputObservation::Absent => ("absent", None),
        CliOutputObservation::Live { presentation } => ("live", Some(presentation)),
        CliOutputObservation::Settling { presentation } => ("settling", Some(presentation)),
        CliOutputObservation::Loading => ("loading", None),
        CliOutputObservation::Denied => ("denied", None),
        CliOutputObservation::Conflicted => ("conflicted", None),
        CliOutputObservation::Invalid => ("invalid", None),
        CliOutputObservation::Retracted => ("retracted", None),
        CliOutputObservation::RetainedPartial => ("retained_partial_diagnostic", None),
        CliOutputObservation::Published { presentation, .. } => {
            ("published_nonterminal", Some(presentation))
        }
        CliOutputObservation::TerminalNoMessage => ("terminal_no_message", None),
        CliOutputObservation::TerminalMessage { presentation, .. } => {
            ("terminal_message", Some(presentation))
        }
    };
    lines.push(format!("  state: {state}"));
    let Some(presentation) = presentation else {
        return;
    };
    if !presentation.body_markdown.is_empty() {
        lines.push("  body:".to_string());
        lines.extend(
            presentation
                .body_markdown
                .lines()
                .map(|line| format!("    {line}")),
        );
    }
    if let Some(reasoning) = presentation
        .reasoning_markdown
        .as_deref()
        .filter(|reasoning| !reasoning.is_empty())
    {
        lines.push("  reasoning:".to_string());
        lines.extend(reasoning.lines().map(|line| format!("    {line}")));
    }
}

fn push_option_line(lines: &mut Vec<String>, label: &str, value: Option<&str>) {
    if let Some(value) = value {
        lines.push(format!("{label}: {value}"));
    }
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

fn display_i64(value: Option<i64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}
