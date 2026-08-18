//! Desired-state/config CLI suites (server-backed).

mod support;

#[path = "suites/cli_config_apply_e2e.rs"]
mod cli_config_apply_e2e;
#[path = "suites/cli_config_apply_graphql.rs"]
mod cli_config_apply_graphql;
#[path = "suites/cli_config_apply_local.rs"]
mod cli_config_apply_local;
#[path = "suites/cli_config_apply_running.rs"]
mod cli_config_apply_running;
#[path = "suites/cli_config_apply_transactional_rollback.rs"]
mod cli_config_apply_transactional_rollback;
#[path = "suites/cli_config_backend.rs"]
mod cli_config_backend;
#[path = "suites/cli_config_behavior_persona.rs"]
mod cli_config_behavior_persona;
#[path = "suites/cli_config_crud.rs"]
mod cli_config_crud;
#[path = "suites/cli_config_read.rs"]
mod cli_config_read;
#[path = "suites/cli_config_task_run.rs"]
mod cli_config_task_run;
#[path = "suites/cli_config_tools.rs"]
mod cli_config_tools;
#[path = "suites/cli_config_workspace_root.rs"]
mod cli_config_workspace_root;
