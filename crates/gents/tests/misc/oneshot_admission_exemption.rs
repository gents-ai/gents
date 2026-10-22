use std::sync::Arc;

use anyhow::Result;
use gents::defra_node::EmbeddedNode;
use gents::{ensure_runtime_schemas, run_openai_oneshot};

use crate::support::fixtures::{bind_agent_backend, test_agent};
use crate::support::mock_endpoint::MockModelEndpoint;

#[tokio::test]
async fn oneshot_completes_without_backend_admission_reconciliation() -> Result<()> {
    let mut behavior = test_agent(
        "oneshot-admission-exemption",
        "backend-oneshot-exempt",
        None,
    );
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(behavior.node_did())
            .build()
            .await?,
    );
    ensure_runtime_schemas(node.as_ref()).await?;

    let mock_endpoint = MockModelEndpoint::start("default")?;
    behavior.backend_endpoint = mock_endpoint.endpoint().to_string();
    bind_agent_backend(
        &node,
        behavior.node_did(),
        &behavior.agent_id,
        "backend-oneshot-exempt",
        mock_endpoint.endpoint(),
        "default",
    )
    .await;

    let result = run_openai_oneshot(node, &behavior, "ping").await?;

    assert!(
        !result.response_text.trim().is_empty(),
        "one-shot must complete without any AdmissionRegistry ever being reconciled for this backend"
    );
    Ok(())
}
