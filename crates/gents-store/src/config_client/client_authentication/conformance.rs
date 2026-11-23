use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::response::IntoResponse as _;
use defra_node::{EmbeddedVeraActor, EmbeddedVeraConfig, EmbeddedVeraDeployment, Node};

use gents_protocol::client_authentication::ClientAuthentication;

use super::runtime_authentication;
use crate::config_client::{ClientSigningCapability, ConfigAccess};

fn signing_key() -> anyhow::Result<Vec<u8>> {
    Ok(identity::RawIdentity::from_private_key(crypto::generate_ed25519()?)?.private_key_bytes())
}

#[tokio::test]
async fn client_held_delegations_match_the_native_verifier() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let home = Arc::new(
        tokio::task::spawn_blocking(|| crate::test_support::test_identity("authentication-home"))
            .await?,
    );
    let key = home
        .client_signing_capability()?
        .config
        .private_key_bytes
        .to_vec();
    let keyring = Arc::new(defra_node::keyring::FileKeyring::open(
        directory.path().join("keys"),
        b"delegation-fixture",
    )?);
    let node = Arc::new(
        Node::builder()
            .with_signing_key(defra_node::SigningKey::Ed25519(key.to_vec()))
            .with_embedded_vera(EmbeddedVeraConfig {
                data_dir: directory.path().join("vera"),
                deployment: EmbeddedVeraDeployment {
                    chain_id: 9403,
                    threshold: 1,
                    operator_keys: vec![
                        "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798".into(),
                    ],
                    created: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
                    policies: Vec::new(),
                },
                listen: "127.0.0.1:0".parse()?,
                start_deadline: Duration::from_secs(60),
                stop_deadline: Duration::from_secs(30),
                actor: EmbeddedVeraActor::Signer(Arc::new(
                    vera_delegation::Ed25519Signer::from_slice(&key)?,
                )),
                rpc_listen: None,
                worker_dir: directory.path().join("worker"),
                keyring,
                tuning: vera::AcpTuning::default(),
            })
            .build()
            .await?,
    );
    let authentication = runtime_authentication(&ConfigAccess::local(node.clone())).await?;
    let ClientAuthentication::Vera {
        worker, deployment, ..
    } = authentication
    else {
        anyhow::bail!("the Vera runtime must advertise its delegation audience");
    };
    let node_did = node
        .node_identity_did()
        .expect("runtime identity")
        .to_owned();
    assert_eq!(node_did, crate::identity::NodeIdentity::did(home.as_ref()));
    let metadata = Arc::new(tokio::sync::RwLock::new(serde_json::json!({
        "node_did": node_did,
        "client_authentication": {"kind": "vera", "worker": worker, "deployment": deployment}
    })));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/api/v0/graphql", listener.local_addr()?);
    let router = axum::Router::new().route(
        "/status",
        axum::routing::get({
            let metadata = metadata.clone();
            move |query: axum::extract::Query<rapidhash::RapidHashMap<String, String>>| {
                let (metadata, home) = (metadata.clone(), home.clone());
                async move {
                    let mut body = metadata.read().await.clone();
                    if let Some(nonce) = query
                        .get(gents_protocol::home_attestation::HOME_NONCE_QUERY)
                        .and_then(|nonce| {
                            gents_protocol::home_attestation::HomeNonce::parse(nonce).ok()
                        })
                    {
                        crate::identity::home_status::attest(
                            Some(home.as_ref()),
                            &nonce,
                            &mut body,
                        )
                        .await;
                    }
                    if let Some(location) = body
                        .get("redirect")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                    {
                        return (
                            axum::http::StatusCode::FOUND,
                            [(axum::http::header::LOCATION, location)],
                            axum::Json(body),
                        )
                            .into_response();
                    }
                    axum::Json(body).into_response()
                }
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, router).await });
    let client = ClientSigningCapability::from_ed25519_key(&signing_key()?)?;
    let other = ClientSigningCapability::from_ed25519_key(&signing_key()?)?;
    assert!(defra_core::signing::get_identity(client.did()).is_none());
    assert!(defra_core::signing::get_identity(other.did()).is_none());
    let mut tokens = Vec::new();
    for case in gents_lean_vocab::families::client_delegation_authentication::cases() {
        let input = &case.input;
        let capability = if input.issuer_matches {
            &client
        } else {
            &other
        };
        let audience = acp::DelegationAudience {
            genesis_id: None,
            worker: if input.worker_matches {
                worker.clone()
            } else {
                other.did().to_owned()
            },
            deployment: if input.deployment_matches {
                deployment
            } else {
                deployment + 1
            },
        };
        let mut token = if input.fresh {
            metadata.write().await["client_authentication"] = serde_json::json!({
                "kind": "vera", "worker": audience.worker, "deployment": audience.deployment
            });
            let endpoints =
                crate::config_client::runtime_endpoints::RuntimeEndpoints::new(capability.clone());
            let endpoint = endpoints.endpoint(&node_did, &url)?;
            endpoint.refresh_runtime_authentication().await?;
            endpoint
                .runtime_authorization()
                .await?
                .expect("the held client signs its own delegation")
                .strip_prefix("Bearer ")
                .expect("bearer authorization")
                .to_owned()
        } else {
            let signer =
                vera_delegation::Ed25519Signer::from_slice(&capability.config.private_key_bytes)?;
            vera_delegation::caller_delegation(&signer, &audience, Duration::from_secs(1)).await?
        };
        if !input.verified {
            let signature = token.rfind('.').expect("JWT signature") + 1;
            let replacement = if token.as_bytes()[signature] == b'A' {
                "B"
            } else {
                "A"
            };
            token.replace_range(signature..signature + 1, replacement);
        }
        tokens.push((case, token));
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    for (case, token) in tokens {
        let observed = node
            .document_acp()
            .caller_of_delegation(&token)
            .expect("the native Vera provider recognizes its delegation format")
            .is_ok_and(|issuer| issuer.did.as_str() == client.did());
        case.check(observed);
    }
    assert!(defra_core::signing::get_identity(client.did()).is_none());
    assert!(defra_core::signing::get_identity(other.did()).is_none());
    metadata.write().await["node_did"] = serde_json::json!(other.did());
    let endpoints = crate::config_client::runtime_endpoints::RuntimeEndpoints::new(client.clone());
    let endpoint = endpoints.endpoint(&node_did, &url)?;
    assert!(matches!(
        endpoint.runtime_authorization().await,
        Err(crate::config_client::ConfigClientError::RuntimeAuthenticationIdentity)
    ));
    metadata.write().await["node_did"] = serde_json::json!(node_did);
    metadata.write().await["redirect"] = serde_json::json!("http://127.0.0.1:9/not-selected");
    assert!(
        matches!(endpoint.runtime_authorization().await, Err(crate::config_client::ConfigClientError::RuntimeAuthenticationStatus(status)) if status == reqwest::StatusCode::FOUND)
    );
    let local = defra_node::Node::builder().build().await?;
    for case in gents_lean_vocab::families::client_delegation_client_graph::cases() {
        let input = &case.input;
        let actor = if input.actor_matches {
            client.did()
        } else {
            other.did()
        }
        .parse()?;
        let credential = input
            .presented
            .then(|| {
                super::ClientAuthorization::from_authenticated_bearer(
                    client.did().parse().expect("client DID"),
                    "Bearer presented",
                )
            })
            .transpose()?;
        case.check(super::client_graph_ready(
            if input.remote { &node } else { &local },
            credential.as_ref(),
            &actor,
        ));
    }
    local.shutdown().await?;
    server.abort();
    node.shutdown().await?;
    Ok(())
}

#[test]
fn request_credentials_never_inherit_another_callers_token() -> anyhow::Result<()> {
    use super::ClientAuthorization;

    let client = ClientSigningCapability::from_ed25519_key(&signing_key()?)?;
    let other = ClientSigningCapability::from_ed25519_key(&signing_key()?)?;
    for case in gents_lean_vocab::families::identity_client_delegation_credential_for_actor::cases()
    {
        for ambient in [false, true] {
            let input = &case.input;
            let actor = if input.actor_matches {
                client.did()
            } else {
                other.did()
            };
            if ambient {
                defra_core::signing::set_request_bearer_token(actor, "ambient");
            }
            let authorization = input
                .presented
                .then(|| {
                    ClientAuthorization::from_authenticated_bearer(
                        client.did().parse().expect("client DID"),
                        "Bearer request",
                    )
                })
                .transpose()?;
            let context =
                ClientAuthorization::request_context(authorization.as_ref(), Some(actor.parse()?));
            let observed = context.bearer_token_for(actor);
            defra_core::signing::clear_request_bearer_token(actor);
            case.check(observed.as_deref() == Some("request"));
            assert!(observed.as_deref() != Some("ambient"));
        }
    }
    Ok(())
}
