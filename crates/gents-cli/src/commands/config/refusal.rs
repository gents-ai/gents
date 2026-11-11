use gents_protocol::operations::NextCall;

/// A refused operation as a person at a terminal reads it: what happened,
/// then the command that corrects it.
#[derive(Debug)]
pub(crate) struct Refusal<E> {
    error: E,
    next: NextCall,
}

impl<E> Refusal<E> {
    pub(crate) fn new(error: E, next: NextCall) -> Self {
        Self { error, next }
    }
}

fn noun(resource: &str) -> &str {
    match resource {
        "mcp-service" => "mcp",
        other => other,
    }
}

fn verb(resource: &str, verb: &str) -> String {
    match (resource, verb) {
        ("skill", "update") => "add".to_owned(),
        ("mcp-service", "register") => "service register".to_owned(),
        ("tools", "update") => "set".to_owned(),
        (_, "delete") => "rm".to_owned(),
        (_, other) => other.to_owned(),
    }
}

pub(crate) fn command(next: NextCall) -> String {
    match next {
        NextCall::VerbHelp {
            resource,
            verb: name,
        } => {
            format!(
                "gents config {} {} --help",
                noun(resource),
                verb(resource, name)
            )
        }
        NextCall::ResourceHelp { resource } => format!("gents config {} --help", noun(resource)),
        NextCall::Inspect { resource: "skill" } => {
            "gents config skill show --graphql <GRAPHQL_URL> --skill-id <SKILL_ID>".to_owned()
        }
        NextCall::Inspect { resource } => format!("gents config {} show <ID>", noun(resource)),
        NextCall::List { resource: "skill" } => {
            "gents config skill list --graphql <GRAPHQL_URL> --node-did <NODE_DID>".to_owned()
        }
        NextCall::List { resource } => format!("gents config {} list", noun(resource)),
        NextCall::Verb {
            resource,
            verb: name,
        } => format!("gents config {} {}", noun(resource), verb(resource, name)),
    }
}

impl<E> std::fmt::Display for Refusal<E>
where
    E: std::fmt::Display,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}\nNext: {}", self.error, command(self.next))
    }
}

impl<E> std::error::Error for Refusal<E>
where
    E: std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.error.source()
    }
}
