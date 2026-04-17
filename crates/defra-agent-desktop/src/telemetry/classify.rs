use tracing::Level;

use super::{DesktopLogCategory, DesktopLogField};

pub fn classify_log_entry(
    level: Level,
    target: &str,
    message: &str,
    fields: &[DesktopLogField],
) -> DesktopLogCategory {
    if is_warning_level(level) {
        return DesktopLogCategory::Warnings;
    }

    let mut haystack = String::with_capacity(target.len() + message.len() + 64);
    haystack.push_str(&target.to_ascii_lowercase());
    haystack.push(' ');
    haystack.push_str(&message.to_ascii_lowercase());
    for field in fields {
        haystack.push(' ');
        haystack.push_str(&field.name.to_ascii_lowercase());
        haystack.push('=');
        haystack.push_str(&field.value.to_ascii_lowercase());
    }

    if contains_any(
        &haystack,
        &[
            "peer",
            "peering",
            "p2p",
            "iroh",
            "relay",
            "dial",
            "replicator",
            "listen_addresses",
            "ticket",
            "node_id",
        ],
    ) {
        DesktopLogCategory::Peering
    } else if contains_any(
        &haystack,
        &[
            "replication",
            "replica",
            "schema",
            "collection",
            "subscribe",
            "subscription",
            "snapshot",
            "gossip",
            "observer",
            "update",
        ],
    ) {
        DesktopLogCategory::Replication
    } else if contains_any(
        &haystack,
        &[
            "request",
            "response",
            "turn",
            "session",
            "conversation",
            "tool_call",
            "tool result",
            "tool_result",
            "scheduler",
            "scheduled task",
        ],
    ) {
        DesktopLogCategory::Turns
    } else if contains_any(
        &haystack,
        &[
            "write", "mutation", "upsert", "save_", "saved", "persist", "apply", "create", "submit",
        ],
    ) {
        DesktopLogCategory::Writes
    } else {
        DesktopLogCategory::Replication
    }
}

fn is_warning_level(level: Level) -> bool {
    matches!(level, Level::WARN | Level::ERROR)
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}
