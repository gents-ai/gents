use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use anyhow::{Context, Result};
use gents_store::client_journal::records::publication;
use gents_store::config_client::{
    ConfigAccess, ConfigClientError, ConfigRelationshipGrant, GraphqlEndpoint,
};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The served runtime, with the User's signed endpoint behind a proxy that
/// records (and can fail) document relationship calls.
struct Fixture {
    runtime: crate::client_test_support::ServedRuntimeHome,
    agent: gents_protocol::agent_identity::AgentIdentityBinding,
    access: ConfigAccess,
    parent_doc: String,
    fault: Arc<AtomicBool>,
    host_hold: Arc<Mutex<Option<HostHold>>>,
    relationships: Arc<Mutex<Vec<(String, String)>>>,
    proxy: tokio::task::JoinHandle<()>,
}

/// The next `host` relationship call never answers the client, as if the
/// client stopped while it was in flight. `applied` forwards it to the runtime
/// first; `reached` fires once the proxy holds it.
struct HostHold {
    applied: bool,
    reached: tokio::sync::oneshot::Sender<String>,
}

impl Fixture {
    async fn new() -> Result<Self> {
        let runtime = crate::client_test_support::ServedRuntimeHome::start().await;
        let node_did = runtime.node_did().to_owned();
        runtime
            .apply_as_user(
                "test.signing.seed",
                vec![
                    (
                        gents_store::collection::Collection::InferenceBackend,
                        json!({"node_did":node_did,"backend_id":"backend","name":"Backend",
                            "provider_kind":"OpenAiCompatible","endpoint":"http://localhost:8000/v1",
                            "auth":{"kind":"unauthenticated"}}),
                    ),
                    (
                        gents_store::collection::Collection::InferenceProfile,
                        json!({"node_did":node_did,"profile_id":"profile","backend_id":"backend","model_name":"model"}),
                    ),
                ],
            )
            .await;
        let nodes = runtime
            .user_access()
            .execute(&format!(
                r#"{{ Node(filter: {{node_did: {{_eq: "{}"}}}}) {{ _docID }} }}"#,
                gents_store::graphql::escape_graphql_string(&node_did)
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
        let host_hold = Arc::new(Mutex::new(None));
        let relationships = Arc::new(Mutex::new(Vec::new()));
        let (url, proxy) = proxy(
            upstream,
            fault.clone(),
            host_hold.clone(),
            relationships.clone(),
        )
        .await?;
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
            host_hold,
            relationships,
            proxy,
        })
    }

    /// The desktop client's own store, whose local signer is the User, holding
    /// its private publication journal.
    async fn client_journal(&self) -> Result<Arc<defra_node::Node>> {
        let node = Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(self.runtime.user().did())
                .build()
                .await?,
        );
        gents_store::schema::ensure_runtime_schemas(&node).await?;
        Ok(node)
    }

    fn publication(
        &self,
        journal: &Arc<defra_node::Node>,
    ) -> Arc<dyn gents_store::config_client::AgentPublicationObserver> {
        publication::Observer::new(
            journal.clone(),
            self.runtime.user().did().to_owned(),
            vec![(
                self.agent.agent_did.clone(),
                self.runtime.node_did().to_owned(),
            )],
        )
    }

    /// A fresh Agent selecting the context its creation requires.
    async fn publish(&self, journal: &Arc<defra_node::Node>) -> Result<()> {
        self.runtime
            .apply_as_user(
                "test.signing.context",
                vec![(
                    gents_store::collection::Collection::AgentContext,
                    json!({"node_did": self.runtime.node_did(), "context_id": "journaled",
                        "system_prompt": "Journaled agent"}),
                )],
            )
            .await;
        super::upsert_agent_with_publication_on(
            &self.access,
            &self.document(Some("journaled"))?,
            None,
            self.publication(journal),
        )
        .await
    }

    async fn recovered(&self, journal: &Arc<defra_node::Node>) -> Result<()> {
        publication::ensure_recovered(
            journal,
            &self.access,
            self.runtime.user().did(),
            self.runtime.node_did(),
            &self.agent.agent_did,
        )
        .await
    }

    fn document(&self, context: Option<&str>) -> Result<gents_documents::Agent> {
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
    host_hold: Arc<Mutex<Option<HostHold>>>,
    relationships: Arc<Mutex<Vec<(String, String)>>>,
) -> Result<(String, tokio::task::JoinHandle<()>)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let task = tokio::spawn(async move {
        while let Ok((mut client, _)) = listener.accept().await {
            let fault = fault.clone();
            let host_hold = host_hold.clone();
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
                        let hold = if relation == "host" { host_hold.lock().unwrap().take() } else { None };
                        if let Some(hold) = hold {
                            let mut answer = Vec::new();
                            if hold.applied {
                                let mut server = tokio::net::TcpStream::connect(upstream).await?;
                                let filtered = headers.lines().filter(|line| !line.to_ascii_lowercase().starts_with("connection:"))
                                    .filter(|line| !line.is_empty()).collect::<Vec<_>>().join("\r\n");
                                server.write_all(format!("{filtered}\r\nConnection: close\r\n\r\n").as_bytes()).await?;
                                server.write_all(&request[header_end..]).await?;
                                server.read_to_end(&mut answer).await?;
                            }
                            let _ = hold.reached.send(String::from_utf8_lossy(&answer).into_owned());
                            let mut bytes = [0u8; 4096];
                            while client.read(&mut bytes).await? > 0 {}
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
    // Agent's identity (`parent`, `executor`, `host`) was issued.
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
    }, ConfigRelationshipGrant::Actor {
        collection: host_collection,
        doc_id: host_doc,
        relation: host_relation,
        target: host_target,
    }] = pending.as_slice()
    else {
        anyhow::bail!("expected exactly the unfinished executor and host grants: {pending:?}");
    };
    assert_eq!(
        (collection.as_str(), relation.as_str(), target.as_str()),
        ("Agent", "executor", fixture.agent.agent_did.as_str())
    );
    assert_eq!(
        (
            host_collection.as_str(),
            host_doc,
            host_relation.as_str(),
            host_target.as_str()
        ),
        ("Agent", doc_id, "host", fixture.runtime.node_did())
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
            ("POST".into(), "executor".into()),
            ("POST".into(), "host".into())
        ]
    );
    fixture.shutdown().await
}

/// Saves a new Agent with the desktop client's publication journal while the
/// `host` grant is held, then drops the save as a client stopping there would.
/// Returns the runtime's answer to the held grant, empty when it never got it.
async fn save_stopped_at_host(
    fixture: &Fixture,
    journal: &Arc<defra_node::Node>,
    applied: bool,
) -> Result<String> {
    let (reached, held) = tokio::sync::oneshot::channel();
    *fixture.host_hold.lock().unwrap() = Some(HostHold { applied, reached });
    let save = fixture.publish(journal);
    tokio::select! {
        result = save => anyhow::bail!("the save finished with its host grant held: {result:?}"),
        answer = held => Ok(answer?),
    }
}

async fn host_grant_unfinished(applied: bool) -> Result<()> {
    let fixture = Fixture::new().await?;
    let journal = fixture.client_journal().await?;
    let answer = save_stopped_at_host(&fixture, &journal, applied).await?;
    assert_eq!(answer.starts_with("HTTP/1.1 200"), applied, "{answer}");
    let rows = fixture.access.execute("{ Agent { _docID } }").await?;
    assert_eq!(rows["data"]["Agent"].as_array().map(Vec::len), Some(1));
    let error = fixture
        .recovered(&journal)
        .await
        .expect_err("a committed Agent without a confirmed host grant is not published");
    let message = error.to_string();
    assert!(
        message.contains("executor: Succeeded, host: Pending"),
        "{message}"
    );
    assert!(fixture
        .relationships
        .lock()
        .unwrap()
        .contains(&("POST".to_owned(), "host".to_owned())));
    journal.shutdown().await?;
    fixture.shutdown().await
}

#[tokio::test]
async fn client_stopped_before_the_host_grant_reaches_the_runtime_keeps_it_pending() -> Result<()> {
    host_grant_unfinished(false).await
}

#[tokio::test]
async fn client_stopped_after_the_host_grant_before_its_answer_keeps_it_pending() -> Result<()> {
    host_grant_unfinished(true).await
}

#[tokio::test]
async fn journaled_publication_completes_only_with_the_host_grant() -> Result<()> {
    let fixture = Fixture::new().await?;
    let journal = fixture.client_journal().await?;
    fixture.publish(&journal).await?;
    fixture.recovered(&journal).await?;
    let relationships = fixture.relationships.lock().unwrap().clone();
    assert_eq!(
        relationships
            .iter()
            .filter(|(method, relation)| method == "POST"
                && ["parent", "executor", "host"].contains(&relation.as_str()))
            .map(|(_, relation)| relation.as_str())
            .collect::<Vec<_>>(),
        ["parent", "executor", "host"]
    );
    journal.shutdown().await?;
    fixture.shutdown().await
}
