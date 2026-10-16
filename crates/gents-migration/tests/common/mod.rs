use std::sync::Arc;

use defra_node::EmbeddedNode;

/// DefraDB can leave background sweep tasks alive after shutdown, so the temp store must outlive the node.
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
