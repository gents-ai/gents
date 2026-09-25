mod support;

#[path = "../src/lean_vocab_test/support.rs"]
mod lean_vocab_test;

#[path = "e2e_subagent/r4_subagent_completion.rs"]
mod r4_subagent_completion;
#[path = "e2e_subagent/r4_subagent_tools.rs"]
mod r4_subagent_tools;
#[path = "e2e_subagent/r6_background_recovery.rs"]
mod r6_background_recovery;
#[path = "e2e_subagent/r6_background_tools.rs"]
mod r6_background_tools;
#[path = "e2e_subagent/same_behavior_foreground_capacity.rs"]
mod same_behavior_foreground_capacity;
#[path = "e2e_subagent/subagent_convergence.rs"]
mod subagent_convergence;
#[path = "e2e_subagent/subagent_enablement_e2e.rs"]
mod subagent_enablement_e2e;
