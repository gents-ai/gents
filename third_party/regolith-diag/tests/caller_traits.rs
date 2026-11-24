//! Every method of every trait a caller implements is called by regolith,
//! with the caller's own data.
//!
//! One module per trait: an implementation that records every call, a small
//! workload that reaches each method, and an assertion per method on what it
//! was handed.
//!
//! The traits whose every call already has such a test are covered where
//! their keys are: `MergeOperator` in `tests/merge_operator_key.rs`,
//! `CompactionFilter` in `tests/compaction_filter_key.rs`, `PrefixExtractor`
//! in `tests/prefix_extractor_key.rs`, and `TransactionHooks` with a
//! transaction's own callbacks in `tests/transaction_callbacks.rs`. Their
//! `name` methods are asked here.

// Native-only. wasm-pack builds every test target for wasm32, and the browser
// suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

/// The `#[path]` attributes are what keep `tests/caller_traits/` from being
/// picked up as test binaries of their own.
#[path = "caller_traits/env.rs"]
mod env;
#[path = "caller_traits/event_listener.rs"]
mod event_listener;
/// An environment that refuses to create tables while told to, for the
/// listener's failed flush.
#[path = "common/faulty_env.rs"]
mod faulty_env;
#[path = "caller_traits/key_classifier.rs"]
mod key_classifier;
#[path = "caller_traits/key_provider.rs"]
mod key_provider;
#[path = "caller_traits/log_layout.rs"]
mod log_layout;
#[path = "caller_traits/names.rs"]
mod names;
#[path = "caller_traits/rate_limiter.rs"]
mod rate_limiter;
#[path = "caller_traits/ttl_filter.rs"]
mod ttl_filter;
#[path = "caller_traits/ttl_merge.rs"]
mod ttl_merge;
