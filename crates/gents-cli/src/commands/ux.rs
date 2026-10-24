//! `gents ux`: the UX plugins the desktop loads from a home, seen from the
//! terminal. `list` and `module` are the same two reads the desktop bridge
//! makes (`gents_server::packs::{ux_list, ux_module}`), so an author can see
//! exactly what the webview will receive; `lint` runs the static lint on
//! one file before it is ever in a pack.

use std::path::PathBuf;

use anyhow::Result;

use crate::cli::args::{UxCommand, UxLintArgs, UxListArgs, UxModuleArgs};

#[derive(Debug, thiserror::Error)]
pub(crate) enum UxLintError {
    #[error("reading {}", .path.display())]
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    RequestHelpers(#[from] crate::request_helpers::RequestHelpersError),
    #[error("{} is refused by the static lint:\n{}", .path.display(), .findings)]
    Refused { path: PathBuf, findings: String },
}

pub(crate) async fn dispatch(command: UxCommand) -> Result<()> {
    match command {
        UxCommand::List(args) => list(args),
        UxCommand::Module(args) => module(args).await,
        UxCommand::Lint(args) => Ok(lint(args)?),
    }
}

fn list(args: UxListArgs) -> Result<()> {
    let home = crate::home_state::resolve_home_dir(args.home.as_deref());
    let plugins = crate::packs::ux_list(&home)?;
    Ok(crate::print_json(
        &serde_json::json!({ "home": home, "plugins": plugins }),
    )?)
}

async fn module(args: UxModuleArgs) -> Result<()> {
    let home = crate::home_state::resolve_home_dir(args.home.as_deref());
    let module = crate::packs::ux_module(home, args.id).await?;
    Ok(crate::print_json(&serde_json::to_value(module)?)?)
}

fn lint(args: UxLintArgs) -> Result<(), UxLintError> {
    let source = std::fs::read_to_string(&args.file).map_err(|source| UxLintError::ReadFile {
        path: args.file.clone(),
        source,
    })?;
    let findings = gents::ux_plugin::lint::lint(&source);
    crate::print_json(&serde_json::json!({
        "file": args.file,
        "admitted": findings.is_empty(),
        "findings": findings,
    }))?;
    if !findings.is_empty() {
        return Err(UxLintError::Refused {
            path: args.file,
            findings: gents::ux_plugin::lint::describe(&findings),
        });
    }
    Ok(())
}
