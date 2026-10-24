use anyhow::Result;
use gents::plugin::allowed;
use serde_json::json;

use crate::cli::args::PluginDirsCommand;

pub(super) fn dispatch(command: PluginDirsCommand) -> Result<()> {
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
            anyhow::ensure!(
                allowed::remove(&home, &args.path)?,
                "{} is not in the allowed folders",
                args.path.display()
            );
            Ok(crate::print_json(&json!({ "removed": args.path }))?)
        }
    }
}
