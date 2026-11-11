pub(crate) use gents::test_support::root::signed_test_node_from_db;
pub(crate) use gents_documents::test_support::bootstrap_signed_test_node;
pub(crate) use gents_store::test_support::{node_builder, runtime_handle};
#[expect(
    dead_code,
    reason = "core document tests use additional members of the shared fixture"
)]
#[path = "../../gents/src/eval/test_support.rs"]
mod documents;
pub(crate) use documents::{completion, draft, identity, origin, HeldEvalFixture};
