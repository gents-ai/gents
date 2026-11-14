use anyhow::{Context, Result};
use gents_inference::views::backends::{BackendList, ObservationList};
use gents_inference::views::profiles::ProfileList;
use gents_inference::views::settings::{ExecutionList, RetryPolicyList, SamplingList};
use gents_store::identity::NodeIdentity;

#[tokio::test]
async fn inference_view_exposure_follows_reader_acp_on_served_node() -> Result<()> {
    let cases = gents_lean_vocab::families::inference_views_exposure::cases();
    anyhow::ensure!(!cases.is_empty(), "Lean emitted no inference view cases");
    let (db, _owner, access) = gents_inference::test_support::signed_test_node_without_agent(
        "inference-view-exposure",
        gents_store::node_extension::Extensions::default(),
    )
    .await;
    let node_did = db.node_identity.did();
    let reader = tokio::task::spawn_blocking(|| {
        gents_store::test_support::test_identity("inference-view-reader")
    })
    .await?;
    let reader_did = identity::Did::new(reader.did())?;
    for permission in [
        acp::nac::NodePermission::DocumentRead,
        acp::nac::NodePermission::CollectionGet,
    ] {
        access
            .grant_node_permission(&reader_did, permission)
            .await?;
    }
    let backend = serde_json::from_value(serde_json::json!({
        "node_did": node_did, "backend_id": "backend", "name": "Backend",
        "provider_kind": "OpenAiCompatible", "endpoint": "http://127.0.0.1:1/v1",
        "auth": {"kind": "unauthenticated"}
    }))?;
    gents_documents::inference_backend::write_inference_backend_document(
        &access,
        &backend,
        &gents_documents::test_support::AdmitAllProfiles,
    )
    .await?;
    let profile = serde_json::from_value(serde_json::json!({
        "node_did": node_did, "profile_id": "profile", "backend_id": "backend",
        "model_name": "model"
    }))?;
    gents_documents::write_inference_profile_document(
        &access,
        &profile,
        &gents_documents::test_support::AdmitAllProfiles,
    )
    .await?;
    let plan = gents_documents::DesiredStateApplyPlan::new(
        [
            (
                gents_store::collection::Collection::InferenceSampling,
                serde_json::json!({
                    "node_did": node_did, "sampling_id": "sampling", "temperature": 0.5,
                }),
            ),
            (
                gents_store::collection::Collection::InferenceExecution,
                serde_json::json!({
                    "node_did": node_did, "execution_id": "execution", "max_turns": 2,
                }),
            ),
            (
                gents_store::collection::Collection::InferenceRetryPolicy,
                serde_json::json!({
                    "node_did": node_did, "retry_policy_id": "retry", "backoff_ms": [10, 20],
                }),
            ),
        ]
        .into_iter()
        .map(
            |(collection, value)| gents_documents::DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            },
        )
        .collect(),
    )?;
    access
        .transact_write("test.inference.views.settings", |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_documents::test_support::AdmitAllProfiles,
                )
                .await
            })
        })
        .await?;
    gents_inference::backends::registry::set_backend_probe_status_as(
        &access, node_did, "backend", "healthy",
    )
    .await?;
    let backend_doc = access
        .read_view(&BackendList::new(node_did))
        .await?
        .pop()
        .context("backend fixture missing")?
        .metadata
        .physical_doc_id
        .context("backend fixture has no physical ID")?;
    let profile_doc = access
        .read_view(&ProfileList::new(node_did))
        .await?
        .pop()
        .context("profile fixture missing")?
        .doc_id;
    let observation_doc = access
        .read_view(&ObservationList::new(node_did))
        .await?
        .pop()
        .context("observation fixture missing")?
        .doc_id;
    let sampling_doc = access
        .read_view(&SamplingList::new(node_did))
        .await?
        .pop()
        .context("sampling fixture missing")?
        .doc_id;
    let execution_doc = access
        .read_view(&ExecutionList::new(node_did))
        .await?
        .pop()
        .context("execution fixture missing")?
        .doc_id;
    let retry_doc = access
        .read_view(&RetryPolicyList::new(node_did))
        .await?
        .pop()
        .context("retry fixture missing")?
        .doc_id;
    let reader_access = db.client_access(&reader);
    for case in cases {
        for (collection, doc_id, allowed) in [
            (
                "InferenceBackendMetadata",
                &backend_doc,
                case.input.metadata_readable,
            ),
            (
                "InferenceProfile",
                &profile_doc,
                case.input.metadata_readable,
            ),
            (
                "InferenceSampling",
                &sampling_doc,
                case.input.metadata_readable,
            ),
            (
                "InferenceExecution",
                &execution_doc,
                case.input.metadata_readable,
            ),
            (
                "InferenceRetryPolicy",
                &retry_doc,
                case.input.metadata_readable,
            ),
            (
                "InferenceBackendObservation",
                &observation_doc,
                case.input.observation_readable,
            ),
        ] {
            if allowed {
                access
                    .add_actor_relationship(collection, doc_id, "reader", &reader_did)
                    .await?;
            } else {
                access
                    .delete_actor_relationship(collection, doc_id, "reader", &reader_did)
                    .await?;
            }
        }
        let scope = if case.input.own_node {
            node_did
        } else {
            reader.did()
        };
        let rows = gents_inference::views::backends::read_inventory(
            &reader_access,
            &BackendList::new(scope),
        )
        .await?;
        let profiles = reader_access.read_view(&ProfileList::new(scope)).await?;
        let observation = rows.iter().any(|row| row.observation.is_some());
        for backend in [
            !rows.is_empty(),
            !profiles.is_empty(),
            !reader_access
                .read_view(&SamplingList::new(scope))
                .await?
                .is_empty(),
            !reader_access
                .read_view(&ExecutionList::new(scope))
                .await?
                .is_empty(),
            !reader_access
                .read_view(&RetryPolicyList::new(scope))
                .await?
                .is_empty(),
        ] {
            case.check(
                gents_lean_vocab::families::inference_views_exposure::Exposure {
                    backend,
                    observation,
                },
            );
        }
    }
    db.node.shutdown().await?;
    Ok(())
}
