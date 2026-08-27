//! Runtime-surface CLI suites (server-backed): chat, requests, status,
//! sessions, subagents, goals, holds, init, reconciliation.

mod support;

#[path = "suites/cli_chat.rs"]
mod cli_chat;
#[path = "suites/cli_diagnose.rs"]
mod cli_diagnose;
#[path = "suites/cli_goal.rs"]
mod cli_goal;
#[path = "suites/cli_init.rs"]
mod cli_init;
#[path = "suites/cli_mailbox.rs"]
mod cli_mailbox;
#[path = "suites/cli_reconciliation.rs"]
mod cli_reconciliation;
#[path = "suites/cli_request.rs"]
mod cli_request;
#[path = "suites/cli_response.rs"]
mod cli_response;
#[path = "suites/cli_session.rs"]
mod cli_session;
#[path = "suites/cli_status.rs"]
mod cli_status;
#[path = "suites/cli_subagent.rs"]
mod cli_subagent;
#[path = "suites/cli_subagent_cancel.rs"]
mod cli_subagent_cancel;
#[path = "suites/cli_tools_holds.rs"]
mod cli_tools_holds;
