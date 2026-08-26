use super::*;

pub(super) fn command_denial_from_row(row: &AgentToolCallRow) -> Option<CommandDenialView> {
    let rule_id = normalize_optional(row.denial_reason.as_deref())?;
    let (category, category_label, reason_line) = command_denial_presentation(&rule_id);
    let denied_command = normalize_optional(row.denied_command.as_deref())
        .or_else(|| first_token(row.denied_prefix.as_deref()))
        .or_else(|| first_token(row.denied_argv.as_deref()));

    Some(CommandDenialView {
        category: category.to_string(),
        category_label: category_label.to_string(),
        rule_id,
        reason_line: reason_line.to_string(),
        denied_command,
        denied_argument: normalize_optional(row.denied_argument.as_deref()),
        denied_subcommand: normalize_optional(row.denied_subcommand.as_deref()),
        diagnostic: normalize_optional(row.result.as_deref()).unwrap_or_default(),
    })
}

fn first_token(value: Option<&[String]>) -> Option<String> {
    value
        .and_then(|items| items.first().cloned())
        .and_then(|value| normalize_optional(Some(value.as_str())))
}

fn command_denial_presentation(rule_id: &str) -> (&'static str, &'static str, &'static str) {
    match rule_id {
        "forbiddenPrefix" => (
            "forbidden-prefix",
            "Forbidden prefix",
            "argv begins with a forbidden prefix configured on this behavior.",
        ),
        "allowedPrefixRequired" => (
            "allowed-prefix-required",
            "Allowed prefix required",
            "Policy requires argv to match one of the configured allowed prefixes; this argv matches none.",
        ),
        "disabledNetworkUnenforceable" => (
            "network-denied",
            "Network access denied",
            "Network mode is disabled, but the unrestricted bash tool can't enforce it - failing closed.",
        ),
        "disabledNetworkCommand" => (
            "network-denied",
            "Network access denied",
            "This command is denied because the behavior has network mode disabled.",
        ),
        "workspaceWriteSandboxUnavailable" => (
            "sandbox-violation",
            "Sandbox violation",
            "workspace_write needs an enforced sandbox before the command can run.",
        ),
        "readOnlyCommandNotAllowlisted"
        | "readOnlyArgumentNotAllowed"
        | "readOnlySubcommandRequired"
        | "readOnlySubcommandNotAllowlisted"
        | "readOnlyUrlRequired" => (
            "read-only-guard",
            "Read-only guard",
            "The read-only bash policy blocked this command.",
        ),
        _ => (
            "policy-config",
            "Policy configuration",
            "The command was denied by the configured command execution policy.",
        ),
    }
}
