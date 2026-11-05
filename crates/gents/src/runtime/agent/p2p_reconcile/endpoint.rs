use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::Node as EmbeddedNode;
use gents_protocol::network_token::EndpointRecord;
use tokio_util::sync::CancellationToken;

use crate::store::graphql::escape_graphql_string;
use crate::store::identity::NodeIdentity;

#[derive(Debug, thiserror::Error)]
pub enum EndpointError {
    #[error("invalid advertised GraphQL URL: {0}")]
    InvalidUrl(String),
    #[error("runtime {0} has no signed GraphQL endpoint; configure --advertise-graphql-url on that runtime")]
    Unavailable(String),
    #[error("runtime {0} has an ambiguous or invalid signed GraphQL endpoint")]
    InvalidBinding(String),
    #[error(transparent)]
    Access(#[from] crate::store::config_client::ConfigClientError),
    #[error(transparent)]
    Identity(#[from] crate::store::identity::IdentityError),
}

/// Routability is an operator assertion, not inferred from Iroh or a bind address.
/// Explicit private LAN addresses are supported; unspecified and loopback hosts
/// cannot identify another runtime. DNS reachability remains a transport concern.
/// Runtime control routes share the prefix before `/api/v0/graphql`; native
/// database aliases alone do not define those application routes.
pub fn validate_advertised_graphql_url(value: &str) -> std::result::Result<(), EndpointError> {
    let invalid = || EndpointError::InvalidUrl(value.to_owned());
    let url = reqwest::Url::parse(value).map_err(|_| invalid())?;
    let host = url.host_str().ok_or_else(invalid)?;
    let ip = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<std::net::IpAddr>()
        .ok();
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.path().ends_with("/api/v0/graphql")
        || host.eq_ignore_ascii_case("localhost")
        || host.to_ascii_lowercase().ends_with(".localhost")
        || ip.is_some_and(|ip| ip.is_loopback() || ip.is_unspecified() || ip.is_multicast())
    {
        return Err(invalid());
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct EndpointRow {
    did: String,
    node_id: String,
    address: Option<String>,
    graphql_url: Option<String>,
    updated_at: Option<String>,
    binding_sig: Option<String>,
}

/// Discovery is untrusted until its complete endpoint binding verifies under
/// the target node DID. HTTP requests retain the calling agent's signing DID;
/// possession of the node's advertisement does not confer database permission.
pub async fn resolve_graphql_endpoint(
    access: &crate::store::config_client::ConfigAccess,
    target_node_did: &str,
    caller: &dyn NodeIdentity,
) -> std::result::Result<crate::store::config_client::GraphqlEndpoint, EndpointError> {
    let escaped = escape_graphql_string(target_node_did);
    let value = access.execute(&format!(
        "query {{ PeerRegistry(filter: {{node_did: {{_eq: \"{escaped}\"}}}}, limit: 2) {{ did: node_did node_id: peer_id address: endpoint_address graphql_url updated_at: endpoint_updated_at binding_sig }} }}"
    )).await?;
    let rows = value
        .get("data")
        .unwrap_or(&value)
        .get("PeerRegistry")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| EndpointError::InvalidBinding(target_node_did.to_owned()))?;
    let [row] = rows.as_slice() else {
        return Err(if rows.is_empty() {
            EndpointError::Unavailable(target_node_did.to_owned())
        } else {
            EndpointError::InvalidBinding(target_node_did.to_owned())
        });
    };
    let row: EndpointRow = serde_json::from_value(row.clone())
        .map_err(|_| EndpointError::InvalidBinding(target_node_did.to_owned()))?;
    let (address, updated_at, binding_sig) = match (row.address, row.updated_at, row.binding_sig) {
        (None, None, None) if row.graphql_url.is_none() => {
            return Err(EndpointError::Unavailable(target_node_did.to_owned()));
        }
        (Some(address), Some(updated_at), Some(binding_sig))
            if !address.is_empty() && !updated_at.is_empty() && !binding_sig.is_empty() =>
        {
            (address, updated_at, binding_sig)
        }
        _ => return Err(EndpointError::InvalidBinding(target_node_did.to_owned())),
    };
    let sig = bs58::decode(binding_sig)
        .into_vec()
        .map_err(|_| EndpointError::InvalidBinding(target_node_did.to_owned()))?;
    let record = EndpointRecord {
        did: row.did,
        node_id: row.node_id,
        address,
        graphql_url: row.graphql_url,
        updated_at,
        sig,
    };
    if record.did != target_node_did
        || !caller
            .verify(target_node_did, &record.signing_payload(), &record.sig)
            .await?
    {
        return Err(EndpointError::InvalidBinding(target_node_did.to_owned()));
    }
    let url = record
        .graphql_url
        .ok_or_else(|| EndpointError::Unavailable(target_node_did.to_owned()))?;
    validate_advertised_graphql_url(&url)?;
    Ok(
        crate::store::config_client::GraphqlEndpoint::as_principal(url, caller.did())
            .with_client_signing_capability(caller.client_signing_capability()?)?,
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EndpointBinding {
    peer_id: String,
    address: String,
    graphql_url: Option<String>,
}

pub async fn run_endpoint_heartbeat(
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn NodeIdentity>,
    graphql_url: Option<String>,
    cancel: CancellationToken,
) -> Result<()> {
    if let Some(url) = &graphql_url {
        validate_advertised_graphql_url(url)?;
    }
    endpoint_heartbeat(node, identity, graphql_url, cancel).await
}

async fn endpoint_heartbeat(
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn NodeIdentity>,
    graphql_url: Option<String>,
    cancel: CancellationToken,
) -> Result<()> {
    let Some(p2p) = crate::store::identity::node_p2p(&node) else {
        tracing::debug!(
            "PeerRegistry endpoint heartbeat idle because embedded node has no P2P transport"
        );
        cancel.cancelled().await;
        return Ok(());
    };

    let mut interval = tokio::time::interval(super::intervals::endpoint_interval());
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut published = None;

    if let Err(error) = tick_endpoint(
        &node,
        &p2p,
        identity.as_ref(),
        graphql_url.as_deref(),
        &mut published,
    )
    .await
    {
        tracing::warn!(
            did = %identity.did(),
            error = %error,
            "PeerRegistry endpoint heartbeat: initial signed endpoint publish failed; will retry"
        );
    }

    interval.tick().await;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = interval.tick() => {
                if let Err(error) = tick_endpoint(
                    &node,
                    &p2p,
                    identity.as_ref(),
                    graphql_url.as_deref(),
                    &mut published,
                ).await {
                    tracing::warn!(
                        did = %identity.did(),
                        error = %error,
                        "PeerRegistry endpoint heartbeat: signed endpoint publish failed; will retry next interval"
                    );
                }
            }
        }
    }
}

async fn tick_endpoint(
    node: &std::sync::Arc<EmbeddedNode>,
    p2p: &Arc<dyn defra_p2p_adapter::P2POperations>,
    identity: &dyn NodeIdentity,
    graphql_url: Option<&str>,
    published: &mut Option<EndpointBinding>,
) -> Result<()> {
    let peer_id = p2p
        .local_peer_id()
        .await
        .map_err(|e| anyhow::anyhow!("local_peer_id: {e}"))?;
    let address = p2p
        .shareable_address()
        .await
        .map_err(|e| anyhow::anyhow!("shareable_address: {e}"))?
        .context("P2P node reported no shareable address for PeerRegistry")?;
    let binding = EndpointBinding {
        peer_id,
        address,
        graphql_url: graphql_url.map(str::to_owned),
    };
    // Readers take freshness from the authorization lease, not from this
    // row's `updated_at`, so an unchanged binding is never rewritten.
    if published.as_ref() == Some(&binding) {
        return Ok(());
    }

    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut record = EndpointRecord {
        did: identity.did().to_string(),
        node_id: binding.peer_id.clone(),
        address: binding.address.clone(),
        graphql_url: binding.graphql_url.clone(),
        updated_at: now,
        sig: Vec::new(),
    };
    record.sig = identity
        .sign(&record.signing_payload())
        .await
        .context("signing PeerRegistry endpoint binding")?;
    let mutation = peer_registry_endpoint_upsert_mutation(&record);
    crate::store::config_client::ConfigAccess::write_local_response(
        node,
        "p2p.upsert_endpoint_heartbeat",
        &mutation,
    )
    .await?;
    *published = Some(binding);
    tracing::debug!(
        did = %record.did,
        node_id = %record.node_id,
        "PeerRegistry endpoint heartbeat: signed endpoint written"
    );
    Ok(())
}

pub fn peer_registry_endpoint_upsert_mutation(record: &EndpointRecord) -> String {
    let did = escape_graphql_string(&record.did);
    let node_id = escape_graphql_string(&record.node_id);
    let address = escape_graphql_string(&record.address);
    let graphql_url = record
        .graphql_url
        .as_deref()
        .map(|url| format!("\"{}\"", escape_graphql_string(url)))
        .unwrap_or_else(|| "null".to_owned());
    let updated_at = escape_graphql_string(&record.updated_at);
    let binding_sig = escape_graphql_string(&bs58::encode(&record.sig).into_string());
    format!(
        r#"mutation {{
            upsert_PeerRegistry(
                filter: {{ peer_id: {{ _eq: "{node_id}" }}, node_did: {{ _eq: "{did}" }} }},
                add: {{
                    node_did: "{did}",
                    peer_id: "{node_id}",
                    addresses: ["{address}"],
                    endpoint_address: "{address}",
                    graphql_url: {graphql_url},
                    endpoint_updated_at: "{updated_at}",
                    binding_sig: "{binding_sig}"
                }},
                update: {{
                    endpoint_address: "{address}",
                    graphql_url: {graphql_url},
                    endpoint_updated_at: "{updated_at}",
                    binding_sig: "{binding_sig}"
                }}
            ) {{ _docID }}
        }}"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertisement_requires_an_explicit_remote_graphql_url() {
        for url in [
            "https://runtime.example/api/v0/graphql",
            "https://runtime.example/runtime/api/v0/graphql",
            "http://192.168.1.2:9191/api/v0/graphql",
        ] {
            validate_advertised_graphql_url(url).unwrap();
        }
        for url in [
            "http://127.0.0.1:9191/api/v0/graphql",
            "http://[::1]/graphql",
            "http://0.0.0.0/graphql",
            "http://localhost/graphql",
            "https://user:secret@runtime.example/graphql",
            "https://runtime.example/graphql?token=secret",
            "https://runtime.example/status",
            "https://runtime.example/graphql",
            "https://runtime.example/api/v1/graphql",
            "https://runtime.example/api/v0/graphql/extra",
        ] {
            assert!(validate_advertised_graphql_url(url).is_err());
        }
    }

    #[tokio::test]
    async fn advertised_graphql_url_is_bound_by_node_signature() {
        let directory = tempfile::tempdir().unwrap();
        let signer = crate::store::identity::KeyIdentity::load_or_create(
            directory.path().join("node.key"),
            None,
        )
        .unwrap();
        let mut record = EndpointRecord {
            did: signer.did().to_owned(),
            node_id: "peer".into(),
            address: "iroh-address".into(),
            graphql_url: Some("https://runtime.example/api/v0/graphql".into()),
            updated_at: "2026-11-04T00:00:00Z".into(),
            sig: Vec::new(),
        };
        record.sig = signer.sign(&record.signing_payload()).await.unwrap();
        assert!(signer
            .verify(signer.did(), &record.signing_payload(), &record.sig)
            .await
            .unwrap());
        record.graphql_url = Some("https://other.example/api/v0/graphql".into());
        assert!(!signer
            .verify(signer.did(), &record.signing_payload(), &record.sig)
            .await
            .unwrap());
    }

    #[test]
    fn endpoint_upsert_escapes_and_has_no_empty_lists() {
        let record = EndpointRecord {
            did: r#"did:key:z"member"#.into(),
            node_id: r#"peer"one"#.into(),
            address: r#"/ip4/127.0.0.1/tcp/1/p2p/peer"one"#.into(),
            graphql_url: Some("https://runtime.example/api/v0/graphql".into()),
            updated_at: "2026-06-16T00:00:00Z".into(),
            sig: vec![1, 2, 3],
        };
        let mutation = peer_registry_endpoint_upsert_mutation(&record);
        assert!(mutation.contains(r#"node_did: "did:key:z\"member""#));
        assert!(mutation.contains(r#"peer_id: { _eq: "peer\"one" }"#));
        assert!(mutation.contains("binding_sig: \"Ldp\""));
        assert!(!mutation.contains("[]"));
    }
    #[tokio::test]
    async fn signed_endpoint_and_registry_heartbeats_share_one_bound_row() {
        use super::super::registry::{
            publish_registry_heartbeat, registry_upsert_mutation, RegistryEntry, UpsertKind,
        };
        use crate::store::config_client::ConfigAccess;
        use crate::store::identity::{KeyIdentity, NodeIdentity};

        for endpoint_first in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let runtime =
                KeyIdentity::load_or_create(directory.path().join("runtime.key"), None).unwrap();
            let caller =
                KeyIdentity::load_or_create(directory.path().join("caller.key"), None).unwrap();
            let other =
                KeyIdentity::load_or_create(directory.path().join("other.key"), None).unwrap();
            let node = Arc::new(
                EmbeddedNode::builder()
                    .with_node_identity_did(runtime.did())
                    .build()
                    .await
                    .unwrap(),
            );
            crate::ensure_runtime_schemas(&node).await.unwrap();
            let access = ConfigAccess::local(node.clone());
            let mut record = EndpointRecord {
                did: runtime.did().to_owned(),
                node_id: "peer-consolidated".into(),
                address: "/ip4/192.0.2.1/tcp/1000/p2p/peer-consolidated".into(),
                graphql_url: Some("https://runtime.example/api/v0/graphql".into()),
                updated_at: "2026-11-04T00:00:00Z".into(),
                sig: Vec::new(),
            };
            record.sig = runtime.sign(&record.signing_payload()).await.unwrap();
            let mut entry = RegistryEntry {
                peer_id: record.node_id.clone(),
                node_did: record.did.clone(),
                addresses: vec![record.address.clone()],
                templates: vec!["machine".into()],
                display_name: Some("home".into()),
                status: "online".into(),
                network_id: "household".into(),
            };
            let endpoint = peer_registry_endpoint_upsert_mutation(&record);
            if endpoint_first {
                access
                    .write("test.registry.endpoint", &endpoint)
                    .await
                    .unwrap();
                publish_registry_heartbeat(&node, &entry, "2026-11-04T00:00:01Z")
                    .await
                    .unwrap();
            } else {
                publish_registry_heartbeat(&node, &entry, "2026-11-04T00:00:01Z")
                    .await
                    .unwrap();
                assert!(matches!(
                    resolve_graphql_endpoint(&access, runtime.did(), &caller).await,
                    Err(EndpointError::Unavailable(_))
                ));
                access
                    .write("test.registry.endpoint", &endpoint)
                    .await
                    .unwrap();
            }
            entry.addresses = vec!["/ip4/192.0.2.2/tcp/1000/p2p/peer-consolidated".into()];
            publish_registry_heartbeat(&node, &entry, "2026-11-04T00:00:02Z")
                .await
                .unwrap();
            let rows = access.execute("{ PeerRegistry { peer_id node_did addresses network_id registered_at endpoint_address graphql_url endpoint_updated_at binding_sig } }").await.unwrap();
            let rows = rows["data"]["PeerRegistry"].as_array().unwrap();
            assert_eq!(rows.len(), 1);
            let row = &rows[0];
            assert_eq!(row["peer_id"], record.node_id);
            assert_eq!(row["node_did"], record.did);
            assert_eq!(row["network_id"], "household");
            assert!(row["registered_at"].as_str().is_some());
            assert_eq!(row["endpoint_address"], record.address);
            assert_eq!(row["endpoint_updated_at"], record.updated_at);
            let resolved = resolve_graphql_endpoint(&access, runtime.did(), &caller)
                .await
                .unwrap();
            assert_eq!(resolved.url(), record.graphql_url.as_deref().unwrap());
            assert!(resolved.authorization().unwrap().is_some());
            let did_only =
                crate::store::identity::RegisteredIdentity::from_registered_did(caller.did(), None)
                    .unwrap();
            assert!(matches!(
                resolve_graphql_endpoint(&access, runtime.did(), &did_only).await,
                Err(EndpointError::Access(
                    crate::store::config_client::ConfigClientError::ClientAgentSignerRequired
                ))
            ));
            let binding = crate::store::identity::provision_agent_identity(&runtime).unwrap();
            let agent = crate::store::identity::resolve_agent_identity(&runtime, &binding).unwrap();
            let resolved_agent = resolve_graphql_endpoint(&access, runtime.did(), agent.as_ref())
                .await
                .unwrap();
            assert_eq!(
                ConfigAccess::Graphql(resolved_agent.clone())
                    .actor_did()
                    .as_deref(),
                Some(agent.did())
            );
            assert!(resolved_agent.authorization().unwrap().is_some());
            assert_eq!(
                ConfigAccess::Graphql(resolved).actor_did().as_deref(),
                Some(caller.did())
            );
            entry.network_id = "operator-selected".into();
            access
                .write(
                    "test.registry.operator",
                    &registry_upsert_mutation(&entry, "2026-11-04T00:00:03Z", UpsertKind::Full),
                )
                .await
                .unwrap();
            entry.network_id = "runtime-default".into();
            publish_registry_heartbeat(&node, &entry, "2026-11-04T00:00:04Z")
                .await
                .unwrap();
            let row = access
                .execute("{ PeerRegistry { network_id graphql_url } }")
                .await
                .unwrap();
            assert_eq!(
                row["data"]["PeerRegistry"][0]["network_id"],
                "operator-selected"
            );
            assert_eq!(
                row["data"]["PeerRegistry"][0]["graphql_url"],
                record.graphql_url.as_deref().unwrap()
            );

            let mut conflicting = record.clone();
            conflicting.did = other.did().into();
            conflicting.sig = other.sign(&conflicting.signing_payload()).await.unwrap();
            assert!(access
                .write(
                    "test.registry.conflicting_endpoint",
                    &peer_registry_endpoint_upsert_mutation(&conflicting)
                )
                .await
                .is_err());
            entry.node_did = other.did().into();
            assert!(access
                .write(
                    "test.registry.conflicting_peer",
                    &registry_upsert_mutation(&entry, "2026-11-04T00:00:03Z", UpsertKind::Full)
                )
                .await
                .is_err());
            assert!(resolve_graphql_endpoint(&access, runtime.did(), &caller)
                .await
                .is_ok());
            node.shutdown().await.expect("node shutdown");
        }
    }
}
