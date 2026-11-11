pub mod goal;
pub mod lifecycle_hooks;
#[cfg(any(test, feature = "test-fixtures"))]
pub mod test_support;
pub mod triggers;

#[cfg(test)]
mod documents_references_tests;
