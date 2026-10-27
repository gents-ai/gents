mod support;

#[path = "../src/lean_vocab_test/support.rs"]
mod lean_vocab_test;

#[path = "e2e_background/plugin_job_admission.rs"]
mod plugin_job_admission;
#[path = "e2e_background/plugin_job_kill.rs"]
mod plugin_job_kill;
#[path = "e2e_background/plugin_job_restart.rs"]
mod plugin_job_restart;
#[path = "e2e_background/plugin_job_support.rs"]
mod plugin_job_support;
#[path = "e2e_background/plugin_job_surface.rs"]
mod plugin_job_surface;
#[path = "e2e_background/r6_background_recovery.rs"]
mod r6_background_recovery;
#[path = "e2e_background/r6_background_tools.rs"]
mod r6_background_tools;
#[path = "e2e_background/session_message_notification_order.rs"]
mod session_message_notification_order;
#[path = "e2e_background/session_message_turn_limit.rs"]
mod session_message_turn_limit;
