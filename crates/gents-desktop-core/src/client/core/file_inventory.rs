use anyhow::{ensure, Context, Result};
use gents::file_inventory::{
    admit_demand, begin_client_progress, load_client_progress_for_scope, receive_client_reply,
    resume_client_progress, verified_client_response,
};
use gents::store::config_client::{ConfigAccess, GraphqlEndpoint};
use gents::NodeIdentity;
use gents_protocol::file_inventory::{
    FileInventoryAdmission, FileInventoryObservation, FileInventoryPageDemand,
    FileInventoryReadAction, FileInventoryReply, FileInventoryResponse,
    FILE_INVENTORY_OPERATOR_PATH,
};
use gents_protocol::session_hydration::HydrationRequest;

use super::bootstrap::normalize_required;
use super::writes::ensure_peer_chat_ready_at;
use super::ClientCore;

impl ClientCore {
    /// Reads retain their durable attempt. Only explicit refresh changes it;
    /// passive operator observation never unlocks an identity.
    pub async fn session_file_inventory(
        &self,
        node_did: &str,
        session_id: &str,
        action: FileInventoryReadAction,
    ) -> Result<Option<FileInventoryObservation>> {
        let node_did = normalize_required("node_did", node_did)?;
        let session_id = normalize_required("session_id", session_id)?;
        let records = self.sync_state.records();
        let mut matching = records.iter().filter(|record| record.node_did == node_did);
        let record = matching.next();
        ensure!(
            matching.next().is_none(),
            "multiple runtime routes own this node"
        );
        let local = ConfigAccess::local(self.node_arc());
        let mut session = load_inventory_session(&local, node_did, session_id).await?;
        if session.is_none() || record.is_none() {
            if let Some(retained) = load_client_progress_for_scope(
                &self.node,
                self.local_peer_id(),
                node_did,
                session_id,
                &self.node_identity,
            )
            .await?
            {
                if let Some(session) = &session {
                    ensure!(
                        session.doc_id == retained.admission().session_doc_id,
                        "Retained file changes belong to a different physical session."
                    );
                }
                if let (None, FileInventoryReadAction::Refresh { attempt_id }) = (record, &action) {
                    let id = uuid::Uuid::parse_str(attempt_id)?.to_string();
                    return begin_client_progress(
                        &self.node,
                        retained.admission(),
                        &id,
                        &self.node_identity,
                    )
                    .await?
                    .observation(None, &self.node_identity)
                    .await
                    .map(Some);
                }
                let cursor = match &action {
                    FileInventoryReadAction::Read { cursor } => cursor.as_ref(),
                    _ => None,
                };
                if matches!(action, FileInventoryReadAction::Read { .. })
                    || (matches!(action, FileInventoryReadAction::Advance {})
                        && retained
                            .active()
                            .pending_demand(&self.node_identity)
                            .await?
                            .is_none())
                {
                    return retained
                        .observation(cursor, &self.node_identity)
                        .await
                        .map(Some);
                }
            }
        }
        let record = record
            .context("Runtime authorization is unavailable. Reconnect to load new file changes.")?;
        let operator_record = record.clone();
        let operator = tokio::task::spawn_blocking(move || {
            crate::local_runtime::cached_operator_endpoint(&operator_record)
                .filter(|endpoint| !endpoint.is_anonymous())
        })
        .await
        .context("reading cached runtime authority")?;
        if session.is_none() {
            if let Some(endpoint) = &operator {
                session = load_inventory_session(
                    &ConfigAccess::Graphql(endpoint.clone()),
                    node_did,
                    session_id,
                )
                .await?;
            }
        }
        let Some(session) = session else {
            return Ok(None);
        };
        let canonical_requester = session
            .session
            .requester_did
            .as_deref()
            .filter(|requester| !requester.is_empty())
            .context("File changes require an explicit session requester.")?;
        let requester = if record.is_enrollment() {
            self.node_identity.did()
        } else {
            canonical_requester
        };
        let admission = FileInventoryAdmission {
            request: HydrationRequest {
                request_key: format!("{}:{session_id}", self.local_peer_id()),
                peer_id: self.local_peer_id().into(),
                requester_did: requester.into(),
                node_did: node_did.into(),
                session_id: session_id.into(),
            },
            session_doc_id: session.doc_id,
        };
        let initial = uuid::Uuid::new_v4().to_string();
        let mut progress = match &action {
            FileInventoryReadAction::Refresh { attempt_id } => {
                let id = uuid::Uuid::parse_str(attempt_id)?.to_string();
                begin_client_progress(&self.node, &admission, &id, &self.node_identity).await?
            }
            _ => {
                resume_client_progress(&self.node, &admission, &initial, &self.node_identity)
                    .await?
            }
        };
        if matches!(action, FileInventoryReadAction::Advance {}) {
            if let Some(demand) = progress
                .active()
                .pending_demand(&self.node_identity)
                .await?
            {
                let reply = if record.is_enrollment() {
                    self.remote_inventory_page(&demand).await?
                } else {
                    let endpoint = operator.as_ref().context(
                        "Runtime authorization is unavailable. Reconnect to load file changes.",
                    )?;
                    read_operator_page(endpoint, &demand, &self.node_identity).await?
                };
                progress =
                    receive_client_reply(&self.node, &demand, &reply, &self.node_identity).await?;
            }
        }
        let cursor = match &action {
            FileInventoryReadAction::Read { cursor } => cursor.as_ref(),
            _ => None,
        };
        progress
            .observation(cursor, &self.node_identity)
            .await
            .map(Some)
    }

    /// A peer can remain connected without delivering the requested page;
    /// end the read with a retryable error instead of waiting indefinitely.
    async fn remote_inventory_page(
        &self,
        demand: &FileInventoryPageDemand,
    ) -> Result<FileInventoryReply> {
        let mut changes = gents::subscribe_collections(
            &self.node,
            ["FileInventoryRequest", "FileInventoryPage"],
        )?;
        let mut sync = self.sync_state.subscribe();
        let read = async {
            let records = self.sync_state.records();
            let record = records
                .iter()
                .find(|record| record.node_did == demand.admission.request.node_did);
            ensure_peer_chat_ready_at(
                &demand.admission.request.node_did,
                record,
                chrono::Utc::now(),
            )?;
            admit_demand(&self.node, demand).await?;
            loop {
                if let Some(response) =
                    verified_client_response(&self.node, demand, &self.node_identity).await?
                {
                    match response.response() {
                        FileInventoryResponse::Refused(receipt) => {
                            return Ok(FileInventoryReply::Refused(receipt.clone()))
                        }
                        FileInventoryResponse::Page(_) => {
                            if let Some(page) = response
                                .read_page(self.node_arc(), &self.node_identity)
                                .await?
                            {
                                return Ok(FileInventoryReply::Page(page));
                            }
                        }
                    }
                }
                let records = self.sync_state.records();
                let record = records
                    .iter()
                    .find(|record| record.node_did == demand.admission.request.node_did);
                ensure_peer_chat_ready_at(
                    &demand.admission.request.node_did,
                    record,
                    chrono::Utc::now(),
                )
                .context("Reconnect this runtime to load file changes.")?;
                tokio::select! {
                    event = changes.recv() => { event.context("file inventory observation closed")?; }
                    update = sync.changed() => { update.context("file inventory route observation closed")?; }
                }
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(30), read)
            .await
            .context("File changes are waiting for the runtime. Retry when it is connected.")?
    }
}

async fn load_inventory_session(
    access: &ConfigAccess,
    node: &str,
    session: &str,
) -> Result<Option<gents::session::SessionOwnerRow>> {
    access
        .transact_readonly("file_inventory.client_scope", |txn| {
            Box::pin(gents::session::load_agent_session_owner_in_txn(
                txn, node, session,
            ))
        })
        .await
}

/// An operator endpoint can be stale or unresponsive while a page is requested.
async fn operator_http_client() -> Result<&'static reqwest::Client> {
    static CLIENT: tokio::sync::OnceCell<reqwest::Client> = tokio::sync::OnceCell::const_new();
    CLIENT
        .get_or_try_init(|| async {
            tokio::task::spawn_blocking(|| {
                reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(std::time::Duration::from_secs(30))
                    .build()
            })
            .await
            .context("building file inventory HTTP client")?
            .context("configuring file inventory HTTP client")
        })
        .await
}

async fn read_operator_page(
    endpoint: &GraphqlEndpoint,
    demand: &FileInventoryPageDemand,
    verifier: &dyn NodeIdentity,
) -> Result<FileInventoryReply> {
    /// Bound the operator's response before retaining and decoding the page.
    const MAX_BYTES: usize = 8 * 1024 * 1024;
    ensure!(
        !endpoint.is_anonymous(),
        "runtime operator authorization is unavailable"
    );
    let mut url = reqwest::Url::parse(endpoint.url()).context("runtime endpoint is invalid")?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "runtime endpoint must be an HTTP URL without credentials"
    );
    let base = url
        .path()
        .strip_suffix("/api/v0/graphql")
        .context("runtime endpoint has no GraphQL path")?;
    let path = format!("{base}{FILE_INVENTORY_OPERATOR_PATH}");
    url.set_path(&path);
    let mut response = endpoint
        .authorize(operator_http_client().await?.post(url))?
        .json(demand)
        .send()
        .await
        .map_err(|error| {
            crate::error::BridgeError::runtime_http_error(
                error,
                "reading runtime file changes".to_owned(),
            )
        })?;
    ensure!(
        response.status().is_success(),
        "File changes could not be loaded (HTTP {}). Reconnect the runtime or retry.",
        response.status().as_u16()
    );
    ensure!(
        response
            .content_length()
            .is_none_or(|length| length <= MAX_BYTES as u64),
        "file inventory response exceeds its limit"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        let message = error.to_string();
        crate::error::BridgeError::runtime_http_error(error, message)
    })? {
        ensure!(
            bytes.len().saturating_add(chunk.len()) <= MAX_BYTES,
            "file inventory response exceeds its limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    let reply = serde_json::from_slice(&bytes).context("decoding runtime file changes")?;
    gents::file_inventory::verify_reply(demand, &reply, verifier).await?;
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    #[tokio::test]
    async fn operator_transport_checks_scope_and_never_follows_redirects() -> Result<()> {
        let root = tempfile::tempdir()?;
        let key = root.path().join("source.key");
        let source =
            tokio::task::spawn_blocking(move || gents::KeyIdentity::load_or_create(key, None))
                .await??;
        let node = std::sync::Arc::new(
            gents::defra_node::EmbeddedNode::builder()
                .with_node_identity_did(source.did())
                .with_node_acp_enabled()
                .build()
                .await?,
        );
        gents::ensure_runtime_schemas(&node).await?;
        let session = "operator-transport";
        let owner = source.did();
        let document = ConfigAccess::transact_local_write(&node, None, "test.inventory.transport_scope", |txn| {
            Box::pin(async move {
                let result = txn.execute_with_variables(
                    "mutation($input: AgentSessionMutationInputArg!) { create_AgentSession(input: $input) { _docID } }",
                    &json!({"input": {"node_did": owner, "session_id": session, "requester_did": owner,
                        "agent_id": "fixture", "created_at": "2026-10-24T00:00:00Z"}})).await?;
                gents::store::graphql::created_doc_id(&result, "AgentSession")
            })
        }).await?;
        let demand = FileInventoryPageDemand {
            admission: FileInventoryAdmission {
                request: HydrationRequest {
                    request_key: format!("peer:{session}"),
                    peer_id: "peer".into(),
                    requester_did: owner.into(),
                    node_did: owner.into(),
                    session_id: session.into(),
                },
                session_doc_id: document,
            },
            attempt_id: "attempt-1".into(),
            cursor: None,
        };
        let reply =
            gents::file_inventory::read_operator_inventory(&node, &source, owner, &demand).await?;
        let server = MockServer::start().await;
        let graphql = format!("{}/runtime/api/v0/graphql", server.uri());
        let endpoint = GraphqlEndpoint::as_principal(&graphql, owner);
        let route = format!("/runtime{FILE_INVENTORY_OPERATOR_PATH}");
        Mock::given(method("POST"))
            .and(path(&route))
            .respond_with(ResponseTemplate::new(200).set_body_json(&reply))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            read_operator_page(&endpoint, &demand, &source).await?,
            reply
        );
        let requests = server.received_requests().await.unwrap();
        assert!(requests[0]
            .headers
            .get("authorization")
            .unwrap()
            .to_str()?
            .starts_with("Bearer "));
        assert_eq!(requests[0].body_json::<FileInventoryPageDemand>()?, demand);
        let anonymous = GraphqlEndpoint::anonymous(&graphql);
        assert!(read_operator_page(&anonymous, &demand, &source)
            .await
            .is_err());
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        server.verify().await;
        server.reset().await;
        let mut other = demand.clone();
        other.attempt_id = "other-attempt".into();
        let wrong =
            gents::file_inventory::read_operator_inventory(&node, &source, owner, &other).await?;
        Mock::given(method("POST"))
            .and(path(&route))
            .respond_with(ResponseTemplate::new(200).set_body_json(wrong))
            .expect(1)
            .mount(&server)
            .await;
        assert!(read_operator_page(&endpoint, &demand, &source)
            .await
            .is_err());
        server.verify().await;
        server.reset().await;
        let sink = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&sink)
            .await;
        Mock::given(method("POST"))
            .and(path(&route))
            .respond_with(
                ResponseTemplate::new(307)
                    .insert_header("Location", format!("{}/stolen", sink.uri())),
            )
            .mount(&server)
            .await;
        assert!(read_operator_page(&endpoint, &demand, &source)
            .await
            .is_err());
        sink.verify().await;
        node.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn cold_inventory_read_retains_attempt_without_unlocking_or_submitting() -> Result<()> {
        use crate::client::{ClientCoreOptions, DesktopPaths};
        use identity::Identity as _;
        let root = tempfile::tempdir()?;
        let server = MockServer::start().await;
        let key = root.path().join("runtime.key");
        let file = key.clone();
        let did = tokio::task::spawn_blocking(move || -> Result<String> {
            Ok(gents::store::identity::load_or_create_file_identity(&file)?
                .did()?
                .to_string())
        })
        .await??;
        let endpoint = format!("{}/api/v0/graphql", server.uri());
        tokio::fs::write(
            root.path().join("init.json"),
            json!({
                "node_name":"Inventory runtime", "node_did":did, "key_path":key,
            })
            .to_string(),
        )
        .await?;
        tokio::fs::write(root.path().join("runtime.json"), json!({
            "node_name":"Inventory runtime", "node_did":did, "graphql":endpoint, "p2p_transport":"iroh",
        }).to_string()).await?;
        let original = tokio::fs::read(&key).await?;
        assert!(!gents::store::identity::can_mint_defradb_bearer(&did));
        let mut core = ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(root.path().join("client")),
            ClientCoreOptions {
                operator_identity_interaction:
                    crate::local_runtime::OperatorIdentityInteraction::Prevent,
                ..ClientCoreOptions::local_only()
            },
        )
        .await?;
        core.persist_local_standard_peer(
            "Inventory runtime",
            "endpoint:cold-inventory",
            &did,
            &endpoint,
            root.path().to_str().unwrap(),
        )
        .await?;
        let node = core.node_arc();
        ConfigAccess::transact_local_write(&node, None, "test.inventory.cold_session", |txn| {
            let did = &did;
            Box::pin(async move { txn.execute_with_variables(
                "mutation($input: AgentSessionMutationInputArg!) { create_AgentSession(input: $input) { _docID } }",
                &json!({"input": {"node_did": did, "session_id": "cold-inventory", "requester_did": did,
                    "agent_id":"fixture", "created_at":"2026-10-24T00:00:00Z"}})).await })
        }).await?;
        core.options.operator_identity_interaction =
            crate::local_runtime::OperatorIdentityInteraction::Allow;
        let core = std::sync::Arc::new(core);
        ConfigAccess::transact_local_write(&core.node, None, "test.inventory.received_scope", |txn| {
            let (did, requester) = (&did, core.node_identity.did());
            Box::pin(async move { txn.execute_with_variables(
                "mutation($input: AgentSessionMutationInputArg!) { create_AgentSession(input: $input) { _docID } }",
                &json!({"input": {"node_did": did, "session_id": "cold-received", "requester_did": requester,
                    "agent_id":"fixture", "created_at":"2026-10-24T00:00:00Z"}})).await })
        }).await?;
        let received = core
            .session_file_inventory(&did, "cold-received", FileInventoryReadAction::default())
            .await?
            .unwrap();
        assert_eq!(
            received.admission.request.requester_did,
            core.node_identity.did()
        );
        assert_ne!(received.admission.request.requester_did, did);
        let first = core
            .session_file_inventory(&did, "cold-inventory", FileInventoryReadAction::default())
            .await?
            .unwrap();
        let again = core
            .session_file_inventory(&did, "cold-inventory", FileInventoryReadAction::default())
            .await?
            .unwrap();
        assert_eq!(first, again);
        assert!(first.current.is_none() && first.refusal.is_none());
        assert!(core
            .session_file_inventory(&did, "cold-inventory", FileInventoryReadAction::Advance {})
            .await
            .is_err());
        assert_eq!(
            core.session_file_inventory(&did, "cold-inventory", FileInventoryReadAction::default())
                .await?,
            Some(first)
        );
        let remained_cold = !gents::store::identity::can_mint_defradb_bearer(&did);
        let unchanged_key = tokio::fs::read(&key).await? == original;
        let submitted = ConfigAccess::local(node)
            .execute("{ AgentRequest { _docID } FileInventoryRequest { _docID } }")
            .await?;
        core.shutdown().await?;
        assert!(remained_cold && unchanged_key);
        assert!(submitted["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(submitted["data"]["FileInventoryRequest"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(!server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .any(|request| request.url.path().ends_with(FILE_INVENTORY_OPERATOR_PATH)));
        Ok(())
    }
    #[tokio::test]
    async fn enrolled_received_inventory_keeps_reader_and_canonical_author_separate_after_reload(
    ) -> Result<()> {
        let root = tempfile::tempdir()?;
        let paths = crate::client::DesktopPaths::from_root(root.path().join("client"));
        let core = std::sync::Arc::new(
            ClientCore::start_with_paths_and_options(
                paths.clone(),
                crate::client::ClientCoreOptions::local_only(),
            )
            .await?,
        );
        let source_key = root.path().join("source.key");
        let source = tokio::task::spawn_blocking(move || {
            gents::KeyIdentity::load_or_create(source_key, None)
        })
        .await??;
        core.sync_state
            .upsert_enrollment_peer(
                "source-peer",
                "Shared source",
                "iroh://source-ticket",
                source.did(),
                "network",
                "enrollment",
                "request-digest",
                source.did(),
                1,
                "2099-10-28T00:00:00Z",
            )
            .await?;
        let node = core.node_arc();
        let created = ConfigAccess::transact_local_write(&node, None, "test.inventory.received_author", |txn| {
            let source = &source;
            Box::pin(async move {
                let response = txn.execute_with_variables(
                    "mutation($input: AgentSessionMutationInputArg!) { create_AgentSession(input: $input) { _docID } }",
                    &json!({"input": {"node_did": source.did(), "session_id": "shared-received",
                        "requester_did": source.did(), "agent_id": "fixture", "created_at": "2026-10-28T00:00:00Z"}}),
                ).await?;
                gents::store::graphql::created_doc_id(&response, "AgentSession")
            })
        }).await?;
        let first = core
            .session_file_inventory(
                source.did(),
                "shared-received",
                FileInventoryReadAction::default(),
            )
            .await?
            .unwrap();
        assert_eq!(
            first.admission.request.requester_did,
            core.node_identity.did()
        );
        assert_ne!(first.admission.request.requester_did, source.did());
        assert_eq!(first.admission.session_doc_id, created);
        let session = load_inventory_session(
            &ConfigAccess::local(node.clone()),
            source.did(),
            "shared-received",
        )
        .await?
        .unwrap();
        assert_eq!(session.session.requester_did.as_deref(), Some(source.did()));
        let requests = ConfigAccess::local(node)
            .execute("{FileInventoryRequest {_docID} FileImageHydrationRequest {_docID}}")
            .await?;
        assert_eq!(requests["data"]["FileInventoryRequest"], json!([]));
        assert_eq!(requests["data"]["FileImageHydrationRequest"], json!([]));
        core.shutdown().await?;
        drop(core);
        let core = std::sync::Arc::new(
            ClientCore::start_with_paths_and_options(
                paths,
                crate::client::ClientCoreOptions::local_only(),
            )
            .await?,
        );
        assert_eq!(
            core.session_file_inventory(
                source.did(),
                "shared-received",
                FileInventoryReadAction::default()
            )
            .await?,
            Some(first)
        );
        core.shutdown().await?;
        Ok(())
    }
}
