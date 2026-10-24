use std::path::PathBuf;

use gents::plugin::allowed::{self, PluginAllowedError};
use serde_json::json;

use crate::cli::args::PluginDirsCommand;
use crate::request_helpers::RequestHelpersError;

#[derive(Debug, thiserror::Error)]
pub(crate) enum PluginDirsError {
    #[error(transparent)]
    Allowed(#[from] PluginAllowedError),
    #[error(transparent)]
    Print(#[from] RequestHelpersError),
    #[error("{} is not in the allowed folders", path.display())]
    NotAllowed { path: PathBuf },
}

pub(super) fn dispatch(command: PluginDirsCommand) -> Result<(), PluginDirsError> {
    match command {
        PluginDirsCommand::List(args) => {
            let home = crate::home_state::resolve_home_dir(args.home.as_deref());
            Ok(crate::print_json(
                &json!({ "dirs": allowed::list(&home)? }),
            )?)
        }
        PluginDirsCommand::Add(args) => {
            let home = crate::home_state::resolve_home_dir(args.home.as_deref());
            Ok(crate::print_json(
                &json!({ "added": allowed::add(&home, &args.path, args.access)? }),
            )?)
        }
        PluginDirsCommand::Remove(args) => {
            let home = crate::home_state::resolve_home_dir(args.home.as_deref());
            if !allowed::remove(&home, &args.path)? {
                return Err(PluginDirsError::NotAllowed { path: args.path });
            }
            Ok(crate::print_json(&json!({ "removed": args.path }))?)
        }
    }
}
