pub mod goal;
pub mod lifecycle_hooks;
pub mod operations;
#[cfg(any(test, feature = "test-fixtures"))]
pub mod test_support;
pub mod triggers;
pub mod views;

#[cfg(test)]
mod documents_references_tests;

#[cfg(test)]
mod goal_budget_limited_tests;

#[cfg(test)]
mod interrupt_scope_tests;

#[cfg(test)]
mod session_claim_tests;

#[cfg(test)]
mod session_client_tests;

#[cfg(test)]
mod streaming_auxiliary_tests;
