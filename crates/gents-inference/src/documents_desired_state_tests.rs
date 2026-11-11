use std::sync::Arc;

use anyhow::Result;
use serde_json::{json, Value};

use gents_documents::test_support::desired_state_fixtures::{
    config, document, backend, register_config_schemas, runtime_access, apply,
};
use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
use gents_store::collection::Collection;
use gents_store::config_client::ConfigAccess;

#[tokio::test]
async fn preview_rejects_a_context_window_above_the_advertised_maximum() -> Result<()> {
    let node = Arc::new(gents_store::test_support::node_builder().build().await?);
    register_config_schemas(&node).await?;
    let access = runtime_access(&node)?;
    let owner = "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7";
    let profile = |window: Value| {
        config(
            Collection::InferenceProfile,
            json!({"node_did":owner,"profile_id":"profile","backend_id":"backend","model_name":"model","context_window":window}),
        )
    };
    apply(
        &access,
        vec![document(backend(owner, "backend")), profile(Value::Null)],
    )
    .await?;
    let typed: gents_documents::InferenceBackend = serde_json::from_value(backend(owner, "backend"))?;
    crate::backends::registry::record_model_catalog(
        &node,
        &typed,
        serde_json::from_value(json!({
            "node_did": null,
            "observed_at": "2026-09-25T00:00:00Z",
            "models": [{"model_name":"model","context_window":272000,"max_context_window":872000}],
        }))?,
    )
    .await?;

    let rejected = preview_window(&access, &profile, 900_000)
        .await
        .expect_err("above the advertised maximum");
    assert!(
        format!("{rejected:#}").contains("exceeds model model advertised maximum 872000"),
        "{rejected:#}"
    );
    preview_window(&access, &profile, 500_000).await?;
    preview_window(&access, &profile, 872_000).await?;
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
                    &gents_documents::test_support::AdmitAllProfiles,
                )
                .await
            })
        })
        .await
}

