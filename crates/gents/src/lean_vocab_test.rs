#[path = "lean_vocab_test/support.rs"]
mod support;

pub(crate) use support::*;

#[cfg(test)]
#[path = "lean_vocab_test/tests.rs"]
mod tests;
