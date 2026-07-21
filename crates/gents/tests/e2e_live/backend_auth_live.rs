use std::sync::Arc;

use anyhow::{Context, Result};
use gents::defra_node::EmbeddedNode;
use gents::{ensure_runtime_schemas, BackendProviderKind};

use crate::support::fixtures::test_behavior;

#[tokio::test]
#[ignore = "hits the live OpenRouter API and requires OPENROUTER_API_KEY"]
async fn live_openrouter_oneshot_succeeds() -> Result<()> {
    let api_key = std::env::var("OPENROUTER_API_KEY")
        .context("set OPENROUTER_API_KEY to run the live OpenRouter smoke test")?;
    let model_name = std::env::var("GENTS_TEST_OPENROUTER_MODEL")
        .unwrap_or_else(|_| "openai/gpt-4o-mini".to_string());

    let node = Arc::new(EmbeddedNode::builder().build().await?);
    ensure_runtime_schemas(node.as_ref()).await?;
    let mut behavior = test_behavior("openrouter-live", "backend-openrouter-live", None);
    behavior.backend_provider_kind = BackendProviderKind::OpenRouter;
    behavior.backend_endpoint = "https://openrouter.ai/api/v1".to_string();
    behavior.backend_api_key = Some(api_key);
    behavior.model_name = model_name;

    let result = gents::run_openai_oneshot(
        node,
        &behavior,
        "Reply with exactly the word READY and nothing else.",
    )
    .await?;

    assert!(!result.response_text.trim().is_empty());

    Ok(())
}
