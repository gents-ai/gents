#[path = "lean_vocab_test/support.rs"]
mod support;

pub(crate) use support::*;

#[cfg(test)]
#[path = "lean_vocab_test/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "lean_vocab_test/request_execution_lease_policy.rs"]
mod request_execution_lease_policy;

#[cfg(test)]
#[path = "lean_vocab_test/task_hooks_policy.rs"]
mod task_hooks_policy;
