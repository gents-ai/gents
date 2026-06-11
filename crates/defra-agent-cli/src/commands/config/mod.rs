use anyhow::Result;

use crate::cli::*;

pub(crate) mod apply;
pub(crate) mod backend;
pub(crate) mod behavior;
pub(crate) mod binding;
pub(crate) mod diff;
pub(crate) mod export;
pub(crate) mod import;
pub(crate) mod profile;
pub(crate) mod skill;
pub(crate) mod task_run;
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
        ConfigCommand::Task { command } => match command {
            ConfigTaskCommand::List(args) => crate::commands::task::task_list(args).await,
            ConfigTaskCommand::Show(args) => crate::commands::task::task_show(args).await,
            ConfigTaskCommand::Run(args) => task_run::config_task_run(args).await,
        },
        ConfigCommand::Skill { command } => match command {
            SkillCommand::Add(args) => skill::skill_add(args).await,
            SkillCommand::Import(args) => skill::skill_import(args).await,
            SkillCommand::Export(args) => skill::skill_export(args).await,
            SkillCommand::List(args) => skill::skill_list(args).await,
            SkillCommand::Show(args) => skill::skill_show(args).await,
            SkillCommand::Rm(args) => skill::skill_rm(args).await,
            SkillCommand::Enable(args) => skill::skill_set_enabled(args, true).await,
            SkillCommand::Disable(args) => skill::skill_set_enabled(args, false).await,
        },
        ConfigCommand::Export(args) => export::config_export(args).await,
        ConfigCommand::Import(args) => import::config_import(args).await,
    }
}
