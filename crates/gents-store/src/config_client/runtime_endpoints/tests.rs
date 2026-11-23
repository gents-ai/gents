use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::http::{HeaderMap, StatusCode};
use base64::Engine as _;
use gents_protocol::delegation_audience::{DelegationAudience, ReportedDelegationAudience};
use gents_protocol::home_attestation::{HomeNonce, HOME_NONCE_QUERY};

use super::RuntimeEndpoints;
use crate::config_client::ConfigClientError;
use crate::identity::{KeyIdentity, NodeIdentity};

/// Who answers the status read at the saved home's address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Answering {
    /// The home, signing for each read.
    Home,
    /// Anything naming the home's node as a home on the node's own store,
    /// unsigned.
    Unsigned,
    /// The home, signing for each read a status whose client authentication
    /// and audience name different deployments.
    Contradictory,
    /// Anything naming the home's node, signed by another key.
    OtherKey,
    /// The home's own status, signed for an earlier read.
    Replayed,
}

struct Address {
    graphql: String,
    status_reads: Arc<AtomicUsize>,
    delegations: Arc<AtomicUsize>,
    requests: Arc<AtomicUsize>,
}

fn presents_delegation(headers: &HeaderMap) -> Option<u64> {
    let bearer = headers.get("authorization")?.to_str().ok()?;
    let claims = bearer.strip_prefix("Bearer ")?.split('.').nth(1)?;
    let claims: serde_json::Value = serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(claims)
            .ok()?,
    )
    .ok()?;
    claims["aud"].as_str()?.strip_prefix("vera:")?.parse().ok()
}

/// The saved home's address, answered as `answering` says, naming the
/// audience of a Vera node at deployment 2 and taking only a delegation for
/// it.
async fn address(answering: Answering, home: Arc<KeyIdentity>, other: Arc<KeyIdentity>) -> Address {
    let audience = DelegationAudience {
        worker: "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK".into(),
        deployment: 2,
        genesis_id: None,
    };
    let (status_reads, delegations, requests) = (
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    );
    let (reads, presented, sent_requests) =
        (status_reads.clone(), delegations.clone(), requests.clone());
    let router = axum::Router::new()
        .route(
            "/status",
            axum::routing::get(
                move |query: axum::extract::Query<rapidhash::RapidHashMap<String, String>>| {
                    let (home, other, audience, reads) =
                        (home.clone(), other.clone(), audience.clone(), reads.clone());
                    async move {
                        reads.fetch_add(1, Ordering::SeqCst);
                        let sent = query
                            .get(HOME_NONCE_QUERY)
                            .and_then(|nonce| HomeNonce::parse(nonce).ok())
                            .expect("a client keeping what the status reports sends a nonce");
                        let reports = ReportedDelegationAudience::Named(audience);
                        let mut status = match answering {
                            Answering::Home => {
                                crate::test_support::signed_home_status(
                                    home.as_ref(),
                                    &reports,
                                    Some(1_000),
                                    &sent,
                                )
                                .await
                            }
                            Answering::Unsigned => {
                                crate::test_support::signed_home_status(
                                    home.as_ref(),
                                    &ReportedDelegationAudience::Absent,
                                    None,
                                    &sent,
                                )
                                .await
                            }
                            Answering::Contradictory => contradictory(home.as_ref(), &sent).await,
                            Answering::OtherKey => {
                                let mut status = crate::test_support::signed_home_status(
                                    other.as_ref(),
                                    &reports,
                                    Some(1_000),
                                    &sent,
                                )
                                .await;
                                status["node_did"] = home.did().into();
                                status
                            }
                            Answering::Replayed => {
                                crate::test_support::signed_home_status(
                                    home.as_ref(),
                                    &reports,
                                    Some(1_000),
                                    &HomeNonce::from_bytes([9; 32]),
                                )
                                .await
                            }
                        };
                        if answering == Answering::Unsigned {
                            status.as_object_mut().unwrap().remove(
                                gents_protocol::home_attestation::STATUS_HOME_ATTESTATION_FIELD,
                            );
                        }
                        axum::Json(status)
                    }
                },
            ),
        )
        .fallback(move |headers: HeaderMap| {
            let presented = presented.clone();
            sent_requests.fetch_add(1, Ordering::SeqCst);
            async move {
                match presents_delegation(&headers) {
                    Some(2) => {
                        presented.fetch_add(1, Ordering::SeqCst);
                        (
                            StatusCode::OK,
                            axum::Json(serde_json::json!({"data": {"Node": []}})),
                        )
                    }
                    Some(_) => {
                        presented.fetch_add(1, Ordering::SeqCst);
                        (StatusCode::FORBIDDEN, axum::Json(serde_json::json!({})))
                    }
                    None => (
                        StatusCode::UNAUTHORIZED,
                        axum::Json(serde_json::json!({
                            "code": gents_protocol::delegation_audience::DELEGATION_REQUIRED_CODE,
                        })),
                    ),
                }
            }
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await });
    Address {
        graphql: format!("http://{local}/api/v0/graphql"),
        status_reads,
        delegations,
        requests,
    }
}

async fn contradictory(home: &KeyIdentity, nonce: &HomeNonce) -> serde_json::Value {
    use gents_protocol::home_attestation::{
        AttestedAudience, HomeAttestation, HomeStatement, HOME_ATTESTATION_VERSION,
        STATUS_HOME_ATTESTATION_FIELD,
    };
    let statement = HomeStatement {
        version: HOME_ATTESTATION_VERSION,
        nonce: nonce.as_str().to_owned(),
        node_did: home.did().to_owned(),
        client_authentication: gents_protocol::client_authentication::ClientAuthentication::Vera {
            publisher_node: None,
            worker: "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK".into(),
            deployment: 2,
        },
        delegation_audience: AttestedAudience::Named {
            audience: DelegationAudience {
                worker: "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK".into(),
                deployment: 3,
                genesis_id: None,
            },
        },
        provider_time: Some(1_000),
    };
    let signature = home.sign(&statement.signing_payload()).await.unwrap();
    serde_json::json!({
        "node_did": home.did(),
        STATUS_HOME_ATTESTATION_FIELD: HomeAttestation::signed(statement, &signature),
    })
}

#[tokio::test]
async fn a_status_naming_the_saved_node_with_an_audience_it_did_not_sign_mints_nothing() {
    let (home, other, user) = tokio::task::spawn_blocking(|| {
        (
            Arc::new(crate::test_support::test_identity("saved-home")),
            Arc::new(crate::test_support::test_identity("impostor")),
            crate::test_support::test_identity("saved-user"),
        )
    })
    .await
    .unwrap();
    let capability = user.client_signing_capability().unwrap();
    for answering in [
        Answering::Unsigned,
        Answering::Contradictory,
        Answering::OtherKey,
        Answering::Replayed,
        Answering::Home,
    ] {
        let address = address(answering, home.clone(), other.clone()).await;
        let endpoint = RuntimeEndpoints::new(capability.clone())
            .endpoint(home.did(), &address.graphql)
            .unwrap();
        let answered = crate::config_client::graphql::query_with_options(
            &endpoint,
            "query { Node { _docID } }",
            None,
            gents_protocol::graphql::GraphqlRequestOptions {
                max_attempts: 1,
                ..Default::default()
            },
        )
        .await;
        if answering == Answering::Home {
            assert!(answered.is_ok(), "{answered:?}");
            assert_eq!(address.delegations.load(Ordering::SeqCst), 1);
            assert!(endpoint.mints_delegations());
            continue;
        }
        let refused = answered.unwrap_err();
        assert!(
            matches!(refused, ConfigClientError::HomeStatusUnauthenticated(_)),
            "{answering:?}: {refused:?}"
        );
        assert!(
            refused
                .to_string()
                .contains("the home's status could not be authenticated"),
            "{refused}"
        );
        assert_eq!(
            address.delegations.load(Ordering::SeqCst),
            0,
            "{answering:?}: no delegation is minted for the audience the home did not sign"
        );
        assert!(!endpoint.mints_delegations(), "{answering:?}: no record");
        assert_eq!(endpoint.delegation_audience.get(), None);
        assert_eq!(
            address.requests.load(Ordering::SeqCst),
            0,
            "{answering:?}: no credential is sent to a home whose status is not its own"
        );
        assert_eq!(address.status_reads.load(Ordering::SeqCst), 1);
    }
}
