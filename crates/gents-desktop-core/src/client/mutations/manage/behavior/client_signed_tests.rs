use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use anyhow::{Context, Result};
use gents::store::config_client::{
    ConfigAccess, ConfigClientError, ConfigRelationshipGrant, GraphqlEndpoint,
};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The served runtime, with the User's signed endpoint behind a proxy that
/// records (and can fail) document relationship calls.
struct Fixture {
    runtime: crate::test_fixtures::ServedRuntimeHome,
    agent: gents_protocol::agent_identity::AgentIdentityBinding,
    access: ConfigAccess,
    parent_doc: String,
    fault: Arc<AtomicBool>,
    relationships: Arc<Mutex<Vec<(String, String)>>>,
    proxy: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn new() -> Result<Self> {
        let runtime = crate::test_fixtures::ServedRuntimeHome::start().await;
        let node_did = runtime.node_did().to_owned();
        runtime
            .apply_as_user(
                "test.signing.seed",
                vec![
                    (
                        gents::Collection::InferenceBackend,
                        json!({"node_did":node_did,"backend_id":"backend","name":"Backend",
                            "provider_kind":"OpenAiCompatible","endpoint":"http://localhost:8000/v1",
                            "auth":{"kind":"unauthenticated"}}),
                    ),
                    (
                        gents::Collection::InferenceProfile,
                        json!({"node_did":node_did,"profile_id":"profile","backend_id":"backend","model_name":"model"}),
                    ),
                ],
            )
            .await;
        let nodes = runtime
            .user_access()
            .execute(&format!(
                r#"{{ Node(filter: {{node_did: {{_eq: "{}"}}}}) {{ _docID }} }}"#,
                gents::store::graphql::escape_graphql_string(&node_did)
            ))
            .await?;
        let parent_doc = nodes["data"]["Node"][0]["_docID"]
            .as_str()
            .context("parent document")?
            .to_owned();
        let agent = runtime.provision_agent().await;
        let upstream: std::net::SocketAddr = reqwest::Url::parse(runtime.graphql())?
            .socket_addrs(|| None)?
            .into_iter()
            .next()
            .context("runtime address")?;
        let fault = Arc::new(AtomicBool::new(false));
        let relationships = Arc::new(Mutex::new(Vec::new()));
        let (url, proxy) = proxy(upstream, fault.clone(), relationships.clone()).await?;
        let user = runtime.user();
        let access = ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(url, user.did())
                .with_client_signing_capability(user.client_signing_capability()?)?
                .with_client_signed_agent_creates(),
        );
        Ok(Self {
            runtime,
            agent,
            access,
            parent_doc,
            fault,
            relationships,
            proxy,
        })
    }

    fn document(&self, context: Option<&str>) -> Result<gents::AgentDocument> {
        Ok(serde_json::from_value(json!({
            "node_did": self.runtime.node_did(),
            "agent_did": self.agent.agent_did, "creation_nonce": self.agent.creation_nonce,
            "inference_profile_id":"profile", "context_id":context,
            "display_name":"Desktop agent"
        }))?)
    }

    async fn shutdown(self) -> Result<()> {
        self.proxy.abort();
        self.runtime.shutdown().await;
        Ok(())
    }
}

async fn proxy(
    upstream: std::net::SocketAddr,
    fault: Arc<AtomicBool>,
    relationships: Arc<Mutex<Vec<(String, String)>>>,
) -> Result<(String, tokio::task::JoinHandle<()>)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let task = tokio::spawn(async move {
        while let Ok((mut client, _)) = listener.accept().await {
            let fault = fault.clone();
            let relationships = relationships.clone();
            tokio::spawn(async move {
                let _ = async {
                    let mut request = Vec::new();
                    let header_end = loop {
                        let mut bytes = [0u8; 4096];
                        let count = client.read(&mut bytes).await?;
                        if count == 0 { return Ok::<(), std::io::Error>(()); }
                        request.extend_from_slice(&bytes[..count]);
                        if let Some(index) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                            break index + 4;
                        }
                        if request.len() > 65536 { return Ok(()); }
                    };
                    let headers = String::from_utf8_lossy(&request[..header_end]).into_owned();
                    let upgrade = headers.lines().any(|line| line.to_ascii_lowercase().starts_with("upgrade: websocket"));
                    let content_length = headers.lines().find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().ok()).flatten()
                    }).unwrap_or(0);
                    while request.len() < header_end + content_length {
                        let mut bytes = [0u8; 4096];
                        let count = client.read(&mut bytes).await?;
                        if count == 0 { return Ok(()); }
                        request.extend_from_slice(&bytes[..count]);
                    }
                    if headers.lines().next().is_some_and(|line| line.contains("/acp/document/relationship")) {
                        let body: Value = serde_json::from_slice(&request[header_end..header_end + content_length]).unwrap();
                        let method = headers.split_whitespace().next().unwrap().to_owned();
                        let relation = body["relation"].as_str().unwrap().to_owned();
                        relationships.lock().unwrap().push((method, relation.clone()));
                        if relation == "executor" && fault.swap(false, Ordering::SeqCst) {
                            client.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await?;
                            return Ok(());
                        }
                    }
                    let mut server = tokio::net::TcpStream::connect(upstream).await?;
                    if upgrade {
                        server.write_all(&request).await?;
                    } else {
                        let filtered = headers.lines().filter(|line| !line.to_ascii_lowercase().starts_with("connection:"))
                            .filter(|line| !line.is_empty()).collect::<Vec<_>>().join("\r\n");
                        server.write_all(format!("{filtered}\r\nConnection: close\r\n\r\n").as_bytes()).await?;
                        server.write_all(&request[header_end..]).await?;
                    }
                    tokio::io::copy_bidirectional(&mut client, &mut server).await?;
                    Ok(())
                }.await;
            });
        }
    });
    Ok((format!("http://{address}/api/v0/graphql"), task))
}

#[tokio::test]
async fn desktop_create_rolls_back_signed_agent_after_late_reference_failure() -> Result<()> {
    let fixture = Fixture::new().await?;
    let result =
        super::upsert_agent_on(&fixture.access, &fixture.document(Some("missing-context"))?).await;
    assert!(
        result.is_err(),
        "the missing context rejects the staged create"
    );
    let rows = fixture.access.execute("{ Agent { _docID } }").await?;
    assert!(rows["data"]["Agent"].as_array().unwrap().is_empty());
    // Only the Node manager usersets were staged before the context check,
    // inside the transaction the failure rolled back; no grant naming the
    // Agent's identity (`parent`, `executor`) was issued.
    assert_eq!(
        fixture.relationships.lock().unwrap().clone(),
        vec![
            ("POST".to_owned(), "administrator".to_owned()),
            ("POST".to_owned(), "administrator".to_owned()),
        ]
    );
    fixture.shutdown().await
}

#[tokio::test]
async fn desktop_recovers_only_unfinished_grant_after_parent_is_revoked() -> Result<()> {
    let fixture = Fixture::new().await?;
    fixture.fault.store(true, Ordering::SeqCst);
    let error = super::upsert_agent_on(&fixture.access, &fixture.document(None)?)
        .await
        .expect_err("executor grant failure follows a committed create");
    let (creator, pending) =
        gents_protocol::error_chain::find_map_source(error.as_ref(), |source| {
            match source.downcast_ref::<ConfigClientError>()? {
                ConfigClientError::CommittedRelationshipsIncomplete {
                    creator, pending, ..
                } => Some((creator.clone(), pending.clone())),
                _ => None,
            }
        })
        .context("committed grant suffix")?;
    assert_eq!(creator, fixture.runtime.user().did());
    let [ConfigRelationshipGrant::Actor {
        collection,
        doc_id,
        relation,
        target,
    }] = pending.as_slice()
    else {
        anyhow::bail!("expected exactly the unfinished executor grant: {pending:?}");
    };
    assert_eq!(
        (collection.as_str(), relation.as_str(), target.as_str()),
        ("Agent", "executor", fixture.agent.agent_did.as_str())
    );
    let rows = fixture.access.execute("{ Agent { _docID } }").await?;
    assert_eq!(rows["data"]["Agent"][0]["_docID"], *doc_id);
    fixture
        .access
        .delete_resource_relationship("Agent", doc_id, "parent", "nodes", &fixture.parent_doc)
        .await?;
    fixture
        .access
        .complete_relationship_grants(&creator, &pending)
        .await?;
    let relationships = fixture.relationships.lock().unwrap().clone();
    assert_eq!(
        relationships,
        vec![
            ("POST".into(), "administrator".into()),
            ("POST".into(), "administrator".into()),
            ("POST".into(), "selected_by".into()),
            ("POST".into(), "parent".into()),
            ("POST".into(), "executor".into()),
            ("DELETE".into(), "parent".into()),
            ("POST".into(), "executor".into())
        ]
    );
    fixture.shutdown().await
}
