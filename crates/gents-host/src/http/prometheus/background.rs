use std::collections::BTreeMap;

use super::exposition::{push_metric_prelude, push_metric_sample};

pub(super) fn render_background_completion_metrics(
    lines: &mut Vec<String>,
    node_did: &str,
    diagnostics: Option<&gents_toolcalls::background::diagnostics::BackgroundCompletionDiagnostics>,
) {
    let labels = [("node_did", node_did.to_string())];
    push_metric_prelude(
        lines,
        "gents_background_completion_diagnostics_available",
        "1 when durable background-completion delivery diagnostics were available to this scrape.",
    );
    push_metric_sample(
        lines,
        "gents_background_completion_diagnostics_available",
        &labels,
        i64::from(diagnostics.is_some()),
    );
    let Some(diagnostics) = diagnostics else {
        return;
    };
    for (name, help, value) in [
        (
            "gents_background_completion_pending_notifications",
            "Durable background-completion notifications not yet acknowledged by a successful continuation snapshot.",
            diagnostics.pending_notifications as i64,
        ),
        (
            "gents_background_completion_acknowledged_notifications",
            "Durable background-completion notifications acknowledged by a successful continuation snapshot.",
            diagnostics.acknowledged_notifications as i64,
        ),
        (
            "gents_background_completion_stranded_notifications",
            "Unacknowledged background-completion notifications with no retryable continuation remaining.",
            diagnostics.stranded_notifications as i64,
        ),
        (
            "gents_background_completion_oldest_pending_age_seconds",
            "Age in seconds of the oldest unacknowledged background-completion notification epoch.",
            diagnostics.oldest_pending_age_seconds.unwrap_or(0),
        ),
        (
            "gents_background_completion_scan_truncated",
            "1 when the bounded durable diagnostics scan reached its wake or notification row limit.",
            i64::from(diagnostics.scan_truncated),
        ),
    ] {
        push_metric_prelude(lines, name, help);
        push_metric_sample(lines, name, &labels, value);
    }
    push_metric_prelude(
        lines,
        "gents_background_completion_epochs",
        "Background-completion continuation epochs grouped by durable delivery state.",
    );
    let mut epochs_by_state = BTreeMap::<&str, i64>::new();
    for epoch in &diagnostics.epochs {
        *epochs_by_state.entry(epoch.state.as_str()).or_default() += 1;
    }
    for (state, count) in epochs_by_state {
        push_metric_sample(
            lines,
            "gents_background_completion_epochs",
            &[
                ("node_did", node_did.to_string()),
                ("state", state.to_string()),
            ],
            count,
        );
    }
}
