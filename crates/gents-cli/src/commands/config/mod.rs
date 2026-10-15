use anyhow::Result;

use crate::cli::*;

pub(crate) mod apply;
pub(crate) mod backend;
pub(crate) mod behavior;
pub(crate) mod binding;
mod crud;
pub(crate) mod diff;
pub(crate) mod export;
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
        ConfigCommand::Agent { command } => match command {
            AgentCommand::Set(args) => behavior::agent_set(args).await,
            AgentCommand::Create(args) => behavior::agent_create(args).await,
            AgentCommand::Clone(args) => behavior::agent_clone(args).await,
            AgentCommand::Disable(args) => behavior::agent_disable(args).await,
            AgentCommand::List(args) => crud::config_list(crud::AGENT_SPEC, args).await,
            AgentCommand::Show(args) => behavior::agent_show(args).await,
            AgentCommand::Rm(args) => crud::config_rm(crud::AGENT_SPEC, args).await,
        },
        ConfigCommand::Tools { command } => match command {
            ToolsConfigCommand::Set(args) => tools::tools_set(args).await,
            ToolsConfigCommand::List(args) => crud::config_list(crud::TOOLS_SPEC, args).await,
            ToolsConfigCommand::Show(args) => crud::config_show(crud::TOOLS_SPEC, args).await,
            ToolsConfigCommand::Rm(args) => crud::config_rm(crud::TOOLS_SPEC, args).await,
            ToolsConfigCommand::AgentTargetEntry(args) => tools::agent_target_entry_command(args),
        },
        ConfigCommand::Profile { command } => match command {
            InferenceProfileCommand::Set(args) => profile::inference_profile_set(args).await,
            InferenceProfileCommand::SetAccount(args) => profile::profile_set_account(args).await,
            InferenceProfileCommand::List(args) => profile::profile_list(args).await,
            InferenceProfileCommand::Show(args) => profile::profile_show(args).await,
            InferenceProfileCommand::Rm(args) => crud::config_rm(crud::PROFILE_SPEC, args).await,
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
            WorkspaceRootCommand::List(args) => workspace_root::workspace_root_list(args).await,
            WorkspaceRootCommand::Show(args) => workspace_root::workspace_root_show(args).await,
            WorkspaceRootCommand::Rm(args) => workspace_root::workspace_root_rm(args).await,
        },
        ConfigCommand::Export(args) => export::config_export(args).await,
    }
}
