use gents_protocol::operations::agent::NextCall;

/// A refused operation, shown with the command that corrects it. Its causes
/// stay those of the refusal.
#[derive(Debug)]
pub(crate) struct Refused<E> {
    error: E,
    next: String,
}

impl<E> Refused<E> {
    pub(crate) fn new(error: E, next: NextCall) -> Self {
        Self {
            error,
            next: command(next),
        }
    }
}

/// The CLI spelling of the call an operation names.
fn command(next: NextCall) -> String {
    match next {
        NextCall::VerbHelp {
            resource: "workspace-root",
            verb,
        } => {
            let verb = match verb {
                "publish" => "set",
                "remove" => "rm",
                verb => verb,
            };
            format!("gents config workspace-root {verb} --help")
        }
        NextCall::VerbHelp { resource, verb } => format!("gents config {resource} {verb} --help"),
        NextCall::ResourceHelp { resource } => format!("gents config {resource} --help"),
        NextCall::Inspect { resource: "node" } => "gents status".to_owned(),
        NextCall::Inspect { resource: "p2p" } => "gents runtime p2p status".to_owned(),
        NextCall::Inspect { resource } => format!("gents config {resource} show <ID>"),
        NextCall::List {
            resource: "p2p-template",
        } => "gents runtime p2p templates list".to_owned(),
        NextCall::List { resource } => format!("gents config {resource} list"),
    }
}

impl<E> std::fmt::Display for Refused<E>
where
    E: std::error::Error,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}\nNext: {}", self.error, self.next)
    }
}

impl<E> std::error::Error for Refused<E>
where
    E: std::error::Error,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.error.source()
    }
}
