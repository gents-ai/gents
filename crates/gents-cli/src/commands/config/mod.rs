use anyhow::Result;

use crate::cli::*;

pub(crate) mod apply;
pub(crate) mod backend;
pub(crate) mod behavior;
pub(crate) mod binding;
mod crud;
pub(crate) mod diff;
pub(crate) mod export;
pub(crate) mod import;
pub(crate) mod profile;
pub(crate) mod skill;
pub(crate) mod task_run;
pub(crate) mod tools;
pub(crate) mod validate;
pub(crate) mod workspace_root;

pub(crate) async fn dispatch(command: ConfigCommand) -> Result<()> {
    match command {
        ConfigCommand::Validate(args) => validate::config_validate(args).await,
        ConfigCommand::Diff(args) => diff::config_diff(args).await,
        ConfigCommand::Apply(args) => apply::config_apply(args).await,
        ConfigCommand::Backend { command } => match command {
            BackendCommand::Set(args) => backend::backend_set(args).await,
            BackendCommand::DiscoverModels(args) => backend::backend_discover_models(args).await,
            BackendCommand::List(args) => crud::config_list(crud::BACKEND_SPEC, args).await,
            BackendCommand::Show(args) => crud::config_show(crud::BACKEND_SPEC, args).await,
            BackendCommand::Rm(args) => crud::config_rm(crud::BACKEND_SPEC, args).await,
        },
        ConfigCommand::Behavior { command } => match command {
            BehaviorCommand::Set(args) => behavior::behavior_set(args).await,
            BehaviorCommand::List(args) => crud::config_list(crud::BEHAVIOR_SPEC, args).await,
            BehaviorCommand::Show(args) => crud::config_show(crud::BEHAVIOR_SPEC, args).await,
            BehaviorCommand::Rm(args) => crud::config_rm(crud::BEHAVIOR_SPEC, args).await,
        },
        ConfigCommand::Tools { command } => match command {
            ToolSelectionCommand::Set(args) => tools::tool_selection_set(args).await,
            ToolSelectionCommand::List(args) => {
                crud::config_list(crud::TOOL_SELECTION_SPEC, args).await
            }
            ToolSelectionCommand::Show(args) => {
                crud::config_show(crud::TOOL_SELECTION_SPEC, args).await
            }
            ToolSelectionCommand::Rm(args) => {
                crud::config_rm(crud::TOOL_SELECTION_SPEC, args).await
            }
            ToolSelectionCommand::SubagentTargetEntry(args) => {
                tools::subagent_target_entry_command(args)
            }
        },
        ConfigCommand::Profile { command } => match command {
            InferenceProfileCommand::Set(args) => profile::inference_profile_set(args).await,
            InferenceProfileCommand::List(args) => {
                crud::config_list(crud::PROFILE_SPEC, args).await
            }
            InferenceProfileCommand::Show(args) => {
                crud::config_show(crud::PROFILE_SPEC, args).await
            }
            InferenceProfileCommand::Rm(args) => crud::config_rm(crud::PROFILE_SPEC, args).await,
        },
        ConfigCommand::Task { command } => match command {
            TaskCommand::List(args) => crate::commands::task::task_list(args).await,
            TaskCommand::Show(args) => crate::commands::task::task_show(args).await,
            TaskCommand::Run(args) => task_run::config_task_run(args).await,
        },
        ConfigCommand::Trigger { command } => match command {
            ConfigTriggerCommand::List(args) => crud::config_list(crud::TRIGGER_SPEC, args).await,
            ConfigTriggerCommand::Show(args) => crud::config_show(crud::TRIGGER_SPEC, args).await,
        },
        ConfigCommand::Schedule { command } => match command {
            ConfigScheduleCommand::List(args) => crud::config_list(crud::SCHEDULE_SPEC, args).await,
            ConfigScheduleCommand::Show(args) => crud::config_show(crud::SCHEDULE_SPEC, args).await,
        },
        ConfigCommand::Mcp { command } => match command {
            ConfigMcpCommand::List(args) => crud::config_list(crud::MCP_SPEC, args).await,
            ConfigMcpCommand::Show(args) => crud::config_show(crud::MCP_SPEC, args).await,
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
        ConfigCommand::WorkspaceRoot { command } => match command {
            WorkspaceRootCommand::Set(args) => workspace_root::workspace_root_set(args).await,
            WorkspaceRootCommand::List(args) => {
                crud::config_list(crud::WORKSPACE_ROOT_SPEC, args).await
            }
            WorkspaceRootCommand::Show(args) => {
                crud::config_show(crud::WORKSPACE_ROOT_SPEC, args).await
            }
            WorkspaceRootCommand::Rm(args) => workspace_root::workspace_root_rm(args).await,
        },
        ConfigCommand::Export(args) => export::config_export(args).await,
        ConfigCommand::Import(args) => import::config_import(args).await,
    }
}
