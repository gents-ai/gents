use std::collections::BTreeSet;
use std::path::Path;

use super::runtime_hop_cap::RuntimeHopCapError;
use super::ReceiptWriteError;
use crate::config_client::access_classification_fixture::{
    classify_erased, classify_owner, error_owner_variants_holding, AccessClassification,
    AccessFailure, Erasure,
};
use crate::config_client::authored_document::AuthoredDocumentCreateError;
use crate::config_client::document_decide::DocumentDecideError;
use crate::config_client::node_deferred_operation::DeferredOperationError;
use crate::config_client::{AgentSelfConfigPermissionError, PackEvidenceError, SchemaInstallError};
use crate::graphql::GraphqlResponseError;
use crate::identity::{
    AgentAdmissionObservationError, AgentProvisionError, EnrollmentApprovalError,
    EnrollmentGrantError, GrantSetError, RemoteAgentProvisionError,
};
use crate::schema::migration::RuntimeMigrationError;
use crate::schema::LegacyBackendPreflightError;
use crate::views::ViewError;

fn wrapped(wrapper: &str, failure: AccessFailure, erasure: Erasure) -> AccessClassification {
    let access = failure.error();
    let boxed = || Box::new(failure.error());
    let response = || GraphqlResponseError::Access(Box::new(failure.error()));
    match wrapper {
        "view_error_execute" => classify_owner(erasure, ViewError::Execute(access)),
        "pack_evidence_error_config" => classify_owner(erasure, PackEvidenceError::Config(boxed())),
        "document_decide_error_authorize" => {
            classify_erased(erasure, DocumentDecideError::Authorize(access))
        }
        "deferred_operation_error_config" => {
            classify_erased(erasure, DeferredOperationError::Config(access))
        }
        "schema_install_error_config_client" => {
            classify_erased(erasure, SchemaInstallError::ConfigClient(access))
        }
        "agent_self_config_permission_error_access" => {
            classify_owner(erasure, AgentSelfConfigPermissionError::Access(access))
        }
        "receipt_write_error_write" => classify_erased(erasure, ReceiptWriteError::Write(access)),
        "runtime_hop_cap_error_access" => {
            classify_erased(erasure, RuntimeHopCapError::Access(access))
        }
        "agent_provision_error_access" => {
            classify_erased(erasure, AgentProvisionError::Access(access))
        }
        "agent_admission_observation_error_access" => {
            classify_erased(erasure, AgentAdmissionObservationError::Access(access))
        }
        "remote_agent_provision_error_authorization" => {
            classify_erased(erasure, RemoteAgentProvisionError::Authorization(access))
        }
        "runtime_migration_error_config" => {
            classify_erased(erasure, RuntimeMigrationError::Config(access))
        }
        "authored_document_create_error_publication" => classify_erased(
            erasure,
            AuthoredDocumentCreateError::Publication(response()),
        ),
        "authored_document_create_error_unconfirmed" => classify_erased(
            erasure,
            AuthoredDocumentCreateError::Unconfirmed(response()),
        ),
        "enrollment_approval_error_node_read" => {
            classify_erased(erasure, EnrollmentApprovalError::NodeRead(access))
        }
        "enrollment_grant_error_read" => {
            classify_owner(erasure, EnrollmentGrantError::Read(access))
        }
        "enrollment_grant_error_observe" => {
            classify_owner(erasure, EnrollmentGrantError::Observe(access))
        }
        "enrollment_grant_error_record" => {
            classify_owner(erasure, EnrollmentGrantError::Record(access))
        }
        "enrollment_grant_error_issue" => classify_owner(
            erasure,
            EnrollmentGrantError::Issue {
                tuple: "node admin",
                source: access,
            },
        ),
        "enrollment_grant_error_revoke" => classify_owner(
            erasure,
            EnrollmentGrantError::Revoke {
                tuple: "node admin",
                source: access,
            },
        ),
        "enrollment_grant_error_retire" => {
            classify_owner(erasure, EnrollmentGrantError::Retire(access))
        }
        "grant_set_error_grant" => classify_erased(
            erasure,
            GrantSetError::Grant {
                set: "operator",
                relation: "admin",
                source: boxed(),
            },
        ),
        "grant_set_error_rollback" => classify_erased(
            erasure,
            GrantSetError::Rollback {
                set: "operator",
                relation: "admin",
                source: boxed(),
                rollback: Box::new(AccessFailure::Other.error()),
            },
        ),
        "grant_set_error_revoke" => classify_erased(
            erasure,
            GrantSetError::Revoke {
                set: "operator",
                relation: "admin",
                source: boxed(),
            },
        ),
        "legacy_backend_preflight_error_query" => {
            classify_erased(erasure, LegacyBackendPreflightError::Query(access))
        }
        other => panic!(
            "store owner variant {other} has no constructor here; add it with its \
             `AccessClassification.StoreWrapper` constructor"
        ),
    }
}

#[test]
fn every_store_owner_variant_preserves_the_access_classification() {
    let cases = gents_lean_vocab::families::access_classification_store_wrapped::cases::<
        AccessClassification,
        Erasure,
        AccessFailure,
        String,
    >();
    let modeled: BTreeSet<String> = cases
        .iter()
        .map(|case| case.input.wrapper.clone())
        .collect();
    let declared = error_owner_variants_holding(
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src")),
        &["ConfigClientError", "GraphqlResponseError"],
    );
    assert_eq!(
        declared, modeled,
        "every store owner variant holding an access error is an \
         `AccessClassification.StoreWrapper`, and no other is"
    );
    for case in cases {
        case.check(wrapped(
            &case.input.wrapper,
            case.input.failure,
            case.input.erasure,
        ));
    }
}

#[derive(Default)]
struct Served {
    begins: std::sync::atomic::AtomicUsize,
    reads: std::sync::atomic::AtomicUsize,
}

async fn begin(
    axum::extract::State(served): axum::extract::State<std::sync::Arc<Served>>,
) -> axum::Json<serde_json::Value> {
    let attempt = served
        .begins
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    axum::Json(serde_json::json!({"id": format!("view-read-{attempt}")}))
}

async fn read(
    axum::extract::State(served): axum::extract::State<std::sync::Arc<Served>>,
) -> axum::Json<serde_json::Value> {
    if served
        .reads
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        == 0
    {
        return axum::Json(serde_json::json!({"errors": [{
            "message": "opaque",
            "extensions": {"code": gents_protocol::database_error::TXN_CONFLICT_CODE}
        }]}));
    }
    axum::Json(serde_json::json!({"data": {"view_0": []}}))
}

async fn finish() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({}))
}

/// `AccessClassification.storeWrapped viewErrorExecute transactionConflict
/// anyhow = conflict`, at the transaction owner: the callback is run again.
#[tokio::test]
async fn a_conflict_on_a_typed_view_read_reruns_an_application_callback() {
    use axum::routing::post;

    let served = std::sync::Arc::new(Served::default());
    let app = axum::Router::new()
        .route("/api/v0/tx", post(begin))
        .route("/api/v0/graphql", post(read))
        .route("/api/v0/tx/{id}", post(finish).delete(finish))
        .with_state(std::sync::Arc::clone(&served));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
    let _server = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    let identity =
        tokio::task::spawn_blocking(|| crate::test_support::test_identity("view-conflict-reader"))
            .await
            .unwrap();
    let access = crate::config_client::ConfigAccess::Graphql(
        crate::config_client::GraphqlEndpoint::with_authenticated_delegation(
            endpoint,
            "Bearer fixture",
            crate::identity::NodeIdentity::did(&identity)
                .parse()
                .unwrap(),
        )
        .unwrap(),
    );
    let rows = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        access.transact_readonly("test.view_read.conflict", |txn| {
            Box::pin(async move {
                let rows = txn
                    .read_view(&crate::views::runtime::NodeRuntimeList::new("node"))
                    .await?;
                anyhow::Ok(rows.len())
            })
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(rows, 0);
    assert_eq!(served.reads.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(served.begins.load(std::sync::atomic::Ordering::SeqCst), 2);
}
