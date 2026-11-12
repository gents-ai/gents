use gents_protocol::operations::NextCall;

/// The CLI command line for the call an operation names. `target` is the
/// document the refused call addressed, when the caller knows it; it fills
/// the argument a preview or inspection takes.
pub(crate) fn command(next: NextCall, target: Option<&str>) -> String {
    match next {
        NextCall::VerbHelp { resource, verb } => {
            format!("{} {} --help", prefix(resource), spelling(resource, verb))
        }
        NextCall::ResourceHelp { resource } => format!("{} --help", prefix(resource)),
        NextCall::Inspect { resource } => inspect(resource, target),
        NextCall::List { resource } => list(resource),
        NextCall::Preview { resource, verb } => match preview_flag(resource, verb) {
            Some(flag) => {
                let target = target.unwrap_or("<ID>");
                format!(
                    "{} {} {target} {flag}",
                    prefix(resource),
                    spelling(resource, verb)
                )
            }
            None => command(NextCall::VerbHelp { resource, verb }, target),
        },
        NextCall::Verb {
            resource: "backend",
            verb: "accounts",
        } => "gents accounts list".to_owned(),
        NextCall::Verb { resource, verb } => {
            format!("{} {}", prefix(resource), spelling(resource, verb))
        }
    }
}

fn prefix(resource: &str) -> String {
    match resource {
        "request" => "gents utils debug request".to_owned(),
        "session" => "gents sessions".to_owned(),
        "mailbox" => "gents automation mailbox".to_owned(),
        "mcp-service" => "gents config mcp".to_owned(),
        resource => format!("gents config {resource}"),
    }
}

fn spelling<'a>(resource: &str, verb: &'a str) -> &'a str {
    match (resource, verb) {
        ("skill", "update") => "add",
        ("mcp-service", "register") => "service register",
        ("tools" | "agent" | "backend" | "profile", "update") => "set",
        ("backend" | "profile", "create") => "set",
        ("workspace-root", "publish") => "set",
        ("workspace-root", "remove") => "rm",
        ("request" | "session" | "mailbox", verb) => verb,
        (_, "delete") => "rm",
        (_, verb) => verb,
    }
}

fn preview_flag(resource: &str, verb: &str) -> Option<&'static str> {
    match (resource, verb) {
        ("pack", "install" | "wire") => Some("--preview"),
        ("skill", "import") => Some("--dry-run"),
        _ => None,
    }
}

fn inspect(resource: &str, target: Option<&str>) -> String {
    let placeholder = match resource {
        "node" => return "gents status".to_owned(),
        "p2p" => return "gents runtime p2p status".to_owned(),
        "pack" => return "gents config pack --help".to_owned(),
        "skill" => {
            return format!(
                "gents config skill show --graphql <GRAPHQL_URL> --skill-id {}",
                target.unwrap_or("<SKILL_ID>")
            )
        }
        "request" => "<REQUEST_ID>",
        "session" => "<SESSION_ID>",
        "agent" => "<AGENT_DID>",
        "mailbox" => "<DOC_ID>",
        _ => "<ID>",
    };
    format!("{} show {}", prefix(resource), target.unwrap_or(placeholder))
}

fn list(resource: &str) -> String {
    match resource {
        "request" | "session" => "gents sessions list".to_owned(),
        "skill" => {
            "gents config skill list --graphql <GRAPHQL_URL> --node-did <NODE_DID>".to_owned()
        }
        "p2p-template" => "gents runtime p2p templates list".to_owned(),
        resource => format!("{} list", prefix(resource)),
    }
}
