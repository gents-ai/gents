//! `gents plugin dirs`: the operator's allowed folders for plugin paths that
//! an agent or a graph names (see `gents::plugin::allowed`).

use anyhow::Result;
use gents::plugin::allowed;
use serde_json::json;

use crate::cli::args::PluginDirsCommand;

pub(super) fn dispatch(command: PluginDirsCommand) -> Result<()> {
    match command {
        PluginDirsCommand::List(args) => {
            let home = crate::home_state::resolve_home_dir(args.home.as_deref());
            crate::print_json(&json!({ "dirs": allowed::list(&home)? }))
        }
        PluginDirsCommand::Add(args) => {
            let home = crate::home_state::resolve_home_dir(args.home.as_deref());
            crate::print_json(&json!({ "added": allowed::add(&home, &args.path, args.access)? }))
        }
        PluginDirsCommand::Remove(args) => {
            let home = crate::home_state::resolve_home_dir(args.home.as_deref());
            anyhow::ensure!(
                allowed::remove(&home, &args.path)?,
                "{} is not in the allowed folders",
                args.path.display()
            );
            crate::print_json(&json!({ "removed": args.path }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::{PluginDirsAddArgs, PluginDirsListArgs, PluginDirsRemoveArgs};

    #[test]
    fn add_then_remove_changes_the_list() {
        let home = tempfile::tempdir().unwrap();
        let folder = tempfile::tempdir().unwrap();
        dispatch(PluginDirsCommand::Add(PluginDirsAddArgs {
            path: folder.path().to_owned(),
            access: gents::pack::BindAccess::ReadWrite,
            home: Some(home.path().to_owned()),
        }))
        .unwrap();
        let listed = allowed::list(home.path()).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].access, gents::pack::BindAccess::ReadWrite);
        dispatch(PluginDirsCommand::List(PluginDirsListArgs {
            home: Some(home.path().to_owned()),
        }))
        .unwrap();
        dispatch(PluginDirsCommand::Remove(PluginDirsRemoveArgs {
            path: folder.path().to_owned(),
            home: Some(home.path().to_owned()),
        }))
        .unwrap();
        assert!(allowed::list(home.path()).unwrap().is_empty());
        assert!(dispatch(PluginDirsCommand::Remove(PluginDirsRemoveArgs {
            path: folder.path().to_owned(),
            home: Some(home.path().to_owned()),
        }))
        .is_err());
    }
}
