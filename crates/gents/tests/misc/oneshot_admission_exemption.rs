//! Pins the `// exemption:` decision in `gents::oneshot`: one-shot completion
//! calls do not route through the daemon's `AdmissionRegistry`.
//!
//! Admission enforces a per-backend concurrency ceiling built from the
//! backend document's `max_concurrent`/`max_queue_depth`/`probe_status`
//! (`BackendAdmissionConfig`) and requires a registry that has been
//! `reconcile()`-d with that config — only the daemon's runtime reconciler
//! ever drives that. If a future change plugged one-shot's model into an
//! `AdmissionRegistry` without also reconciling it, every completion call
//! would fail immediately with "BackendGone: backend admission controller is
//! not active" instead of running — which is exactly what this test would
//! catch: it runs a real one-shot call against a mock backend with nothing in
//! the process ever calling `AdmissionRegistry::reconcile`, and asserts it
//! still succeeds.

use std::sync::Arc;

use anyhow::Result;
use gents::defra_node::EmbeddedNode;
use gents::{ensure_runtime_schemas, run_openai_oneshot};

use crate::support::fixtures::test_behavior;
use crate::support::mock_endpoint::MockModelEndpoint;

#[tokio::test]
async fn oneshot_completes_without_backend_admission_reconciliation() -> Result<()> {
    let node = Arc::new(EmbeddedNode::builder().build().await?);
    ensure_runtime_schemas(node.as_ref()).await?;

    let mock_endpoint = MockModelEndpoint::start("default")?;
    let mut behavior = test_behavior(
        "oneshot-admission-exemption",
        "backend-oneshot-exempt",
        None,
    );
    behavior.backend_endpoint = mock_endpoint.endpoint().to_string();

    let result = run_openai_oneshot(node, &behavior, "ping").await?;

    assert!(
        !result.response_text.trim().is_empty(),
        "one-shot must complete without any AdmissionRegistry ever being reconciled for this backend"
    );
    Ok(())
}
