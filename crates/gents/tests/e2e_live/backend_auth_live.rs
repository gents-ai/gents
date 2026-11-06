use anyhow::{Context, Result};
use gents::documents::BackendAuth;
use gents::eval::runner::embedded::EmbeddedHome;
use gents::BackendProviderKind;

use crate::support::fixtures::{test_agent_with_identity, test_node_for};

#[tokio::test]
#[ignore = "hits the live OpenRouter API and requires OPENROUTER_API_KEY"]
async fn live_openrouter_oneshot_succeeds() -> Result<()> {
    let api_key = std::env::var("OPENROUTER_API_KEY")
        .context("set OPENROUTER_API_KEY to run the live OpenRouter smoke test")?;
    let model_name = std::env::var("GENTS_TEST_OPENROUTER_MODEL")
        .unwrap_or_else(|_| "openai/gpt-4o-mini".to_string());

    let db = crate::support::test_db_from_home(EmbeddedHome::create_temp("openrouter-live").await?);
    let fixture = crate::support::managed_agent_fixture(&db).await;
    let mut agent = test_agent_with_identity(
        test_node_for(db.node_identity.clone(), fixture.binding.agent_did.clone()),
        fixture.agent.clone(),
    );
    agent.backend_id = Some(format!("{}:backend", fixture.binding.agent_did));
    agent.backend_provider_kind = BackendProviderKind::OpenRouter;
    agent.backend_endpoint = "https://openrouter.ai/api/v1".to_string();
    agent.backend_auth = BackendAuth::ApiKey { key: api_key };
    agent.model_name = model_name;

    let result = gents::run_openai_oneshot_admitted(
        db.node.clone(),
        &agent,
        "Reply with exactly the word READY and nothing else.",
        &db.home.backend_admission(),
    )
    .await?;

    assert!(!result.response_text.trim().is_empty());

    Ok(())
}
