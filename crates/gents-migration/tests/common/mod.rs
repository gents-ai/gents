//! Shared node bootstrap for the gents-migration integration tests.

use std::sync::Arc;

use defra_node::EmbeddedNode;

/// Boot an isolated node on a fresh temp store.
///
/// The tempdir is deliberately leaked instead of returned as a guard:
/// DefraDB can leave untracked background sweep tasks alive after
/// `shutdown()` (defradb.rs #1309), and deleting the store directory under
/// them is a flake source. Test processes are short-lived, so the leak is
/// bounded by the run.
pub async fn fresh_node() -> Arc<EmbeddedNode> {
    let dir = tempfile::tempdir().expect("tempdir");
    let node = EmbeddedNode::builder()
        .data_path(dir.path())
        .build()
        .await
        .expect("build node");
    std::mem::forget(dir);
    Arc::new(node)
}
