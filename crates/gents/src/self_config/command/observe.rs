use super::*;

pub(super) fn observed_call(args: &ConfigCommandParams) -> String {
    let call = json!({
        "argv": args.argv,
        "target_id": args.target_id,
        "set": args.set,
        "clear": args.clear,
        "options": args.options,
    });
    truncated(&call.to_string(), 2048)
}

fn truncated(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_owned(),
    }
}

pub(super) fn log_config_call(
    argv: &[String],
    call: &str,
    help: bool,
    previewing: bool,
    result: &Result<String>,
) {
    let outcome = if result.is_ok() { "ok" } else { "error" };
    let error = result
        .as_ref()
        .err()
        .map(|error| truncated(&format!("{error:#}"), 500));
    tracing::info!(
        target: "gents::self_config",
        call = %call,
        outcome,
        error = error.as_deref().unwrap_or(""),
        "tool call"
    );
    let resource = argv.first().map(String::as_str).unwrap_or("");
    let verb = argv.get(1).map(String::as_str).unwrap_or("");
    let reads = [
        "get", "list", "discover", "context", "help", "--help", "-h", "scan", "verify",
    ];
    if help
        || previewing
        || resource == "help"
        || resource == "get"
        || resource == "batch"
        || resource == "validate"
        || reads.contains(&verb)
    {
        return;
    }
    let collection = match (resource, argv.get(2).map(String::as_str)) {
        ("automation", Some(kind)) => kind,
        (resource, _) => resource,
    };
    let verb = match verb {
        "remove" | "delete" => "delete",
        "apply" => "plan",
        "create" | "clone" | "install" | "import" => "create",
        _ => "edit",
    };
    let error_kind = error.as_deref().map(|error| {
        error
            .split([':', ';'])
            .next()
            .unwrap_or("")
            .trim()
            .chars()
            .take(80)
            .collect::<String>()
    });
    tracing::info!(
        target: "gents::self_config",
        collection,
        verb,
        outcome = if result.is_ok() { "committed" } else { "refused" },
        error_kind = error_kind.as_deref().unwrap_or(""),
        "self-config write"
    );
}

pub(super) fn append_receipt(
    text: &str,
    receipt: &super::execution::ConfigExecutionReceipt,
) -> Result<String> {
    let body = text
        .trim_end()
        .strip_suffix('}')
        .filter(|_| text.trim_start().starts_with('{'))
        .context("config output must be an object")?
        .trim_end();
    let separator = if body.ends_with('{') { "" } else { "," };
    Ok(format!(
        "{body}{separator}\n  \"config_execution\": {}\n}}",
        serde_json::to_string(receipt)?
    ))
}
