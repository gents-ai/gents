use anyhow::Result;

use crate::cli::*;

pub(crate) mod apply;
pub(crate) mod backend;
pub(crate) mod behavior;
pub(crate) mod diff;
pub(crate) mod export;
pub(crate) mod import;
pub(crate) mod profile;
pub(crate) mod tools;
pub(crate) mod validate;

pub(crate) async fn dispatch(command: ConfigCommand) -> Result<()> {
    match command {
        ConfigCommand::Validate(args) => validate::config_validate(args).await,
        ConfigCommand::Diff(args) => diff::config_diff(args).await,
        ConfigCommand::Apply(args) => apply::config_apply(args).await,
        ConfigCommand::Backend { command } => match command {
            BackendCommand::Set(args) => backend::backend_set(args).await,
            BackendCommand::DiscoverModels(args) => backend::backend_discover_models(args).await,
        },
        ConfigCommand::Behavior { command } => match command {
            BehaviorCommand::Set(args) => behavior::behavior_set(args).await,
        },
        ConfigCommand::Tools { command } => match command {
            ToolSelectionCommand::Set(args) => tools::tool_selection_set(args).await,
        },
        ConfigCommand::Profile { command } => match command {
            InferenceProfileCommand::Set(args) => profile::inference_profile_set(args).await,
        },
        ConfigCommand::Export(args) => export::config_export(args).await,
        ConfigCommand::Import(args) => import::config_import(args).await,
    }
}
