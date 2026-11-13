use std::sync::Arc;

use anyhow::Result;
use serde_json::{json, Value};

use gents_documents::test_support::desired_state_fixtures::{
    apply, backend, config, document, register_config_schemas, runtime_access,
};
use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
use gents_store::collection::Collection;
use gents_store::config_client::ConfigAccess;

#[tokio::test]
async fn desired_state_preview_applies_the_advertised_context_ceiling() -> Result<()> {
    use gents_lean_vocab::families::configuration_scope_context_preview as preview;

    let node = Arc::new(gents_store::test_support::node_builder().build().await?);
    register_config_schemas(&node).await?;
    let access = runtime_access(&node)?;
    let owner = "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7";
    let typed: gents_documents::InferenceBackend =
        serde_json::from_value(backend(owner, "backend"))?;
    for case in gents_lean_vocab::families::configuration_scope_context_preview::cases() {
        let input: &preview::ContextPreviewInput = &case.input;
        let profile = |window: Value| {
            config(
                Collection::InferenceProfile,
                json!({"node_did":owner,"profile_id":input.profile_id,"backend_id":"backend",
                    "model_name":input.model_name,"context_window":window}),
            )
        };
        apply(
            &access,
            vec![document(backend(owner, "backend")), profile(Value::Null)],
        )
        .await?;
        crate::backends::registry::record_model_catalog(
            &node,
            &typed,
            serde_json::from_value(json!({
                "node_did": null,
                "observed_at": "2026-09-25T00:00:00Z",
                "models": [{"model_name":input.model_name,"context_window":input.model_default,
                    "max_context_window":input.maximum}],
            }))?,
        )
        .await?;
        let rejection = preview_window(&access, &profile, input.selected)
            .await
            .err()
            .map(|error| error.root_cause().to_string());
        case.check(rejection);
    }
    Ok(())
}

async fn preview_window(
    access: &ConfigAccess,
    profile: &dyn Fn(Value) -> DesiredStateApplyDocument,
    window: u64,
) -> Result<()> {
    let plan = DesiredStateApplyPlan::new(vec![profile(json!(window))])?;
    access
        .transact_write("test.preview.context_window", |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents_documents::validate_desired_state_plan(
                    txn,
                    plan,
                    &crate::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
}
