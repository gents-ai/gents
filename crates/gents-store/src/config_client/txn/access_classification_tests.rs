use super::runtime_hop_cap::RuntimeHopCapError;
use super::ReceiptWriteError;
use crate::config_client::access_classification_fixture::{
    classify_erased, classify_owner, AccessClassification, AccessFailure, Erasure,
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
    gents_lean_vocab::families::access_classification_store_wrapped::replay::<
        AccessClassification,
        Erasure,
        AccessFailure,
        String,
    >(|input| wrapped(&input.wrapper, input.failure, input.erasure));
}

fn database_failure(retryable: bool) -> gents_protocol::database_error::GraphqlFailure {
    let code = if retryable {
        gents_protocol::database_error::TXN_CONFLICT_CODE
    } else {
        "INTERNAL"
    };
    gents_protocol::database_error::GraphqlFailure::new(
        "write",
        serde_json::json!([{"message": "opaque", "extensions": {"code": code}}]),
    )
}

fn retryable_as<E>(erasure: Erasure, error: E) -> bool
where
    E: std::error::Error + Send + Sync + 'static,
{
    match erasure {
        Erasure::Typed => gents_protocol::graphql::graphql_error_is_retryable(&error),
        Erasure::Anyhow => {
            gents_protocol::graphql::graphql_error_is_retryable(anyhow::Error::new(error).as_ref())
        }
        Erasure::Boxed => gents_protocol::graphql::graphql_error_is_retryable(
            (Box::new(error) as Box<dyn std::error::Error + Send + Sync>).as_ref(),
        ),
    }
}

/// `AccessClassification.storeRetryWrapped`: the GraphQL owners' retryability
/// of a database failure held by each store variant that holds one.
#[test]
fn every_store_holder_of_a_graphql_failure_keeps_its_retryability() {
    use gents_protocol::graphql::GraphqlError;
    gents_lean_vocab::families::access_classification_store_retry_wrapped::replay::<Erasure, String>(
        |input| {
            let failure = || database_failure(input.retryable);
            match input.wrapper.as_str() {
                "config_client_error_protocol" => retryable_as(
                    input.erasure,
                    crate::config_client::ConfigClientError::Protocol(GraphqlError::Database(
                        failure(),
                    )),
                ),
                "config_client_error_database" => retryable_as(
                    input.erasure,
                    crate::config_client::ConfigClientError::Database(failure()),
                ),
                "graphql_response_error_database" => {
                    retryable_as(input.erasure, GraphqlResponseError::Database(failure()))
                }
                "deferred_operation_error_graphql" => retryable_as(
                    input.erasure,
                    DeferredOperationError::Graphql(GraphqlError::Database(failure())),
                ),
                "enrollment_grant_error_record_unconfirmed" => retryable_as(
                    input.erasure,
                    EnrollmentGrantError::RecordUnconfirmed(GraphqlError::Database(failure())),
                ),
                other => panic!(
                    "store variant {other} has no constructor here; add it with its \
                 `AccessClassification.StoreRetryWrapper` constructor"
                ),
            }
        },
    );
}

/// `AccessClassification.OwnerProbe`.
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum OwnerProbe {
    InsecureKey,
    LockHeld,
    Incompatible,
}

/// The first store classifier that answers for `error`.
fn store_owner_answer(error: &(dyn std::error::Error + 'static)) -> Option<OwnerProbe> {
    use crate::local::storage_backend::{find_incompatible, find_insecure_key};
    if find_insecure_key(error).is_some() {
        Some(OwnerProbe::InsecureKey)
    } else if crate::local::home::store_lock_held(error).is_some() {
        Some(OwnerProbe::LockHeld)
    } else if find_incompatible(error).is_some() {
        Some(OwnerProbe::Incompatible)
    } else {
        None
    }
}

fn owner_answer_as<E>(erasure: Erasure, error: E) -> Option<OwnerProbe>
where
    E: std::error::Error + Send + Sync + 'static,
{
    match erasure {
        Erasure::Typed => store_owner_answer(&error),
        Erasure::Anyhow => store_owner_answer(anyhow::Error::new(error).as_ref()),
        Erasure::Boxed => store_owner_answer(
            (Box::new(error) as Box<dyn std::error::Error + Send + Sync>).as_ref(),
        ),
    }
}

/// `AccessClassification.storeOwnerFound`: an owner held through a box, or
/// by `StoreCompatibilityError`, is asked by the classifier that answers for
/// it.
#[test]
fn every_boxed_or_compatibility_holder_reaches_its_store_classifier() {
    use crate::identity::{IdentityError, InsecureKeyPermissions};
    use crate::local::home::{HomeError, StoreLockHeld};
    use crate::local::store_compat::StoreCompatibilityError;
    use crate::local::store_key::StoreKeyError;
    use gents_protocol::operations::install_repair::{IncompatibleStore, IncompatibleStoreKind};

    let insecure = || {
        Box::new(IdentityError::InsecureKey(InsecureKeyPermissions {
            path: "/home/keys/identity.key".into(),
            mode: 0o644,
        }))
    };
    let held = || {
        HomeError::LockHeld(StoreLockHeld {
            home: "/home".into(),
            holder_pid: Some(7),
        })
    };
    let label = || "gents.identity".to_string();
    gents_lean_vocab::families::access_classification_store_owner_found::replay::<
        Erasure,
        OwnerProbe,
        String,
    >(|input| {
        let erasure = input.erasure;
        match input.wrapper.as_str() {
            "identity_error_load_signing_identity" => owner_answer_as(
                erasure,
                IdentityError::LoadSigningIdentity {
                    did: "did:key:z6Mk".into(),
                    source: insecure(),
                },
            ),
            "identity_error_store_keychain_identity" => owner_answer_as(
                erasure,
                IdentityError::StoreKeychainIdentity {
                    label: label(),
                    source: insecure(),
                },
            ),
            "identity_error_secure_enclave_public_key" => owner_answer_as(
                erasure,
                IdentityError::SecureEnclavePublicKey {
                    label: label(),
                    source: insecure(),
                },
            ),
            "identity_error_secure_enclave_did" => owner_answer_as(
                erasure,
                IdentityError::SecureEnclaveDid {
                    label: label(),
                    source: insecure(),
                },
            ),
            "store_key_error_keychain_store" => owner_answer_as(
                erasure,
                StoreKeyError::KeychainStore {
                    label: label(),
                    source: insecure(),
                },
            ),
            "store_key_error_keychain_delete" => owner_answer_as(
                erasure,
                StoreKeyError::KeychainDelete {
                    label: label(),
                    source: insecure(),
                },
            ),
            "pack_evidence_error_identity" => {
                owner_answer_as(erasure, PackEvidenceError::Identity(insecure()))
            }
            "home_error_archive_rolled_back" => owner_answer_as(
                erasure,
                HomeError::ArchiveRolledBack {
                    source: Box::new(held()),
                },
            ),
            "home_error_archive_unrestored" => owner_answer_as(
                erasure,
                HomeError::ArchiveUnrestored {
                    entries: "data".into(),
                    source: Box::new(held()),
                },
            ),
            "home_error_delete_stopped" => owner_answer_as(
                erasure,
                HomeError::DeleteStopped {
                    removed: "data".into(),
                    source: Box::new(held()),
                },
            ),
            "store_compatibility_error_home" => {
                owner_answer_as(erasure, StoreCompatibilityError::Home(held()))
            }
            "store_compatibility_error_key" => owner_answer_as(
                erasure,
                StoreCompatibilityError::Key(StoreKeyError::Incompatible(IncompatibleStore {
                    kind: IncompatibleStoreKind::ForeignVersion,
                    data_path: "/home/data".into(),
                })),
            ),
            other => panic!(
                "store variant {other} has no constructor here; add it with its \
                 `AccessClassification.StoreOwnerWrapper` constructor"
            ),
        }
    });
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
