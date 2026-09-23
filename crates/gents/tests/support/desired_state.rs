//! Publishing desired-state documents into a test home in one transaction.

use gents::config_client::{
    apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents::{Collection, ConfigAccess};
use serde_json::Value;

/// Applies `documents` as one desired-state plan in one transaction named
/// `label`, adding each document or updating it in place.
pub async fn install_documents(
    access: &ConfigAccess,
    label: &'static str,
    documents: Vec<(Collection, Value)>,
) {
    let plan = DesiredStateApplyPlan::new(
        documents
            .into_iter()
            .map(|(collection, value)| DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            })
            .collect(),
    )
    .unwrap();
    access
        .transact(label, |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await
        .unwrap();
}
