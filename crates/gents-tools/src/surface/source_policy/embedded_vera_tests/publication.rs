use gents_store::local::backend_protection::FreshBackendStore;
use gents_store::local::document_acp::{ensure_node_publication, DocumentAcp, DocumentAcpError};

use super::{home, start};

#[tokio::test]
async fn a_vera_home_refuses_node_run_peer_publication_and_a_local_home_does_not(
) -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    for (name, document_acp) in [("vera", DocumentAcp::Vera), ("local", DocumentAcp::Local)] {
        let home = home(directory.path(), name).await?;
        FreshBackendStore::observe(&home.data, &home.lock)
            .await?
            .initialize(&home.key, &home.node, &home.owner, document_acp)
            .await?;
        let node = start(&home, document_acp).await?;
        let published = ensure_node_publication(&node);
        match document_acp {
            DocumentAcp::Vera => assert!(matches!(
                published,
                Err(DocumentAcpError::TransportGrantsAwaitProvider)
            )),
            DocumentAcp::Local => assert!(published.is_ok()),
        }
        node.shutdown().await?;
    }
    Ok(())
}
