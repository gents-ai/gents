//! Lean `Identity.OwnerStaged.pairingOverHttp` over real signed HTTP: the
//! runtime's pairing-reader route decides each modeled requester and
//! observation exactly as that requester's embedded transaction does.

use std::sync::{Arc, Mutex};

use axum::{routing::post, Router};
use gents::store::config_client::{
    ConfigAccess, ConfigClientError, GraphqlEndpoint, PairingObservation, PairingReadReason,
};
use gents::NodeIdentity;
use gents_protocol::pairing_reader::{
    PairingReaderCommand, PAIRING_READER_COMMAND_VERSION, PAIRING_READER_PATH,
};
use reqwest::StatusCode;
use serde_json::Value;

use super::state;

#[derive(serde::Deserialize)]
struct Generated {
    pairing: Vec<Value>,
    commands: Vec<Value>,
}

fn generated() -> Generated {
    let stdout = gents_lean_contract::run_prepared_generator(
        &gents_lean_contract::proofs_dir().unwrap(),
        "Proofs.Conformance.PairingReaderRoute:olean",
        "Proofs/Conformance/PairingReaderRoute.lean",
    )
    .unwrap();
    serde_json::from_str(stdout.trim()).unwrap()
}

/// A runtime home whose Node is owned by `user` and administered by `admin`,
/// another Node administered by `other_admin`, and three pairing
/// observations: runtime-owned, foreign-owned and missing. Every requester is
/// a managed Agent so its embedded request can be signed too.
struct Fixture {
    home: gents::eval::runner::embedded::EmbeddedHome,
    user: Arc<dyn NodeIdentity>,
    admin: Arc<dyn NodeIdentity>,
    outsider: Arc<dyn NodeIdentity>,
    other_admin: Arc<dyn NodeIdentity>,
    target: identity::Did,
    route: String,
    foreign: String,
    missing: String,
    address: std::net::SocketAddr,
    _server: tokio_util::task::AbortOnDropHandle<()>,
    _keys: tempfile::TempDir,
}

fn did(value: &str) -> identity::Did {
    identity::Did::new(value).unwrap()
}

fn client(graphql: &str, key: &dyn NodeIdentity) -> ConfigAccess {
    ConfigAccess::Graphql(
        GraphqlEndpoint::as_principal(graphql, key.did())
            .with_client_signing_capability(key.client_signing_capability().unwrap())
            .unwrap(),
    )
}

fn pairing(author: &str, peer: &str) -> String {
    format!(
        r#"mutation {{ create_PeerPairingDesired(input: {{node_did: "{author}", actor_did: "{author}",
        peer_id: "{peer}", source: "enrollment", collections: ["AgentSessionInputEdit"],
        replicator_addresses: ["{peer}-address"], parent_linked: false}}) {{_docID}} }}"#,
        author = gents::store::graphql::escape_graphql_string(author),
    )
}

impl Fixture {
    async fn start() -> Self {
        let home = gents::eval::runner::embedded::EmbeddedHome::create_temp("pairing-reader-route")
            .await
            .unwrap();
        let keys = tempfile::tempdir().unwrap();
        let runtime = home.identity.clone();
        let runtime_did = did(runtime.did());
        let node = home.node.clone();
        let graphql = home.graphql_endpoint().unwrap().to_owned();
        let managed = || -> Arc<dyn NodeIdentity> {
            let binding =
                gents::store::identity::provision_agent_identity(runtime.as_ref()).unwrap();
            gents::store::identity::resolve_agent_identity(runtime.as_ref(), &binding).unwrap()
        };
        let (user, admin, outsider, other_admin, target) =
            (managed(), managed(), managed(), managed(), managed());
        let stranger: Arc<dyn NodeIdentity> = Arc::new(
            gents::KeyIdentity::load_or_create(keys.path().join("stranger.key"), None).unwrap(),
        );
        let runtime_access = ConfigAccess::local_as(node.clone(), runtime_did.clone());
        for actor in [&user, &admin, &outsider, &other_admin, &target, &stranger] {
            gents::store::identity::GrantSet::OperatorUser
                .grant(&runtime_access, &did(actor.did()))
                .await
                .unwrap();
        }
        let user_access = client(&graphql, user.as_ref());
        for (node_did, administrator) in [(runtime.did(), &admin), (stranger.did(), &other_admin)] {
            let created = user_access
                .write(
                    "test.pairing_route.node",
                    &format!(
                        r#"mutation {{ create_Node(input: {{node_did: "{}"}}) {{_docID}} }}"#,
                        gents::store::graphql::escape_graphql_string(node_did)
                    ),
                )
                .await
                .unwrap();
            let created =
                gents_protocol::graphql::extract_mutation_doc_id(&created, "Node").unwrap();
            user_access
                .add_actor_relationship(
                    "Node",
                    &created,
                    "administrator",
                    &did(administrator.did()),
                )
                .await
                .unwrap();
            if node_did == runtime.did() {
                user_access
                    .add_actor_relationship("Node", &created, "runtime_reader", &runtime_did)
                    .await
                    .unwrap();
            }
        }
        let route = runtime_access
            .write("test.pairing_route.route", &pairing(runtime.did(), "route"))
            .await
            .unwrap();
        let route =
            gents_protocol::graphql::extract_mutation_doc_id(&route, "PeerPairingDesired").unwrap();
        // The runtime may read the foreign observation, so only its owner
        // check refuses it.
        let stranger_access = client(&graphql, stranger.as_ref());
        let foreign = stranger_access
            .write(
                "test.pairing_route.foreign",
                &pairing(stranger.did(), "foreign"),
            )
            .await
            .unwrap();
        let foreign =
            gents_protocol::graphql::extract_mutation_doc_id(&foreign, "PeerPairingDesired")
                .unwrap();
        stranger_access
            .add_actor_relationship("PeerPairingDesired", &foreign, "route_reader", &runtime_did)
            .await
            .unwrap();
        let missing = Mutex::new(None);
        let discarded = runtime_access
            .transact_write("test.pairing_route.missing", |txn| {
                let missing = &missing;
                let runtime_did = &runtime_did;
                Box::pin(async move {
                    let created = txn
                        .execute(&pairing(runtime_did.as_str(), "missing"))
                        .await?;
                    *missing.lock().unwrap() =
                        Some(gents_protocol::graphql::extract_mutation_doc_id(
                            &created,
                            "PeerPairingDesired",
                        )?);
                    Err::<(), anyhow::Error>(anyhow::anyhow!("discard the missing observation"))
                })
            })
            .await;
        assert!(discarded.is_err());
        let missing = missing
            .into_inner()
            .unwrap()
            .expect("missing observation ID");

        let mut state = state();
        state.node_did = runtime.did().to_owned();
        *state.enrollment_decisions.write().await = Some(
            crate::http::enrollment::EnrollmentDecisionService::new(runtime.clone(), node.clone()),
        );
        let app = Router::new()
            .route(
                PAIRING_READER_PATH,
                post(crate::http::pairing_reader::pairing_reader_handler),
            )
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }));
        Self {
            home,
            user,
            admin,
            outsider,
            other_admin,
            target: did(target.did()),
            route,
            foreign,
            missing,
            address,
            _server: server,
            _keys: keys,
        }
    }

    fn requester(&self, name: &str) -> &Arc<dyn NodeIdentity> {
        match name {
            "user" => &self.user,
            "admin" => &self.admin,
            "outsider" => &self.outsider,
            "other-admin" => &self.other_admin,
            other => panic!("unknown modeled requester {other}"),
        }
    }

    fn document(&self, name: &str) -> &str {
        match name {
            "route" => &self.route,
            "foreign" => &self.foreign,
            "missing" => &self.missing,
            other => panic!("unknown modeled observation {other}"),
        }
    }

    fn endpoint(&self) -> String {
        format!("http://{}/api/v0/graphql", self.address)
    }

    /// The runtime's route as the CLI reaches it, authenticated as `key`.
    fn route_endpoint(&self, key: &dyn NodeIdentity) -> GraphqlEndpoint {
        GraphqlEndpoint::as_principal(self.endpoint(), key.did())
            .with_client_signing_capability(key.client_signing_capability().unwrap())
            .unwrap()
    }

    async fn readable(&self) -> Vec<String> {
        let response = ConfigAccess::local_as(self.home.node.clone(), self.target.clone())
            .execute("{ PeerPairingDesired { _docID } }")
            .await
            .unwrap();
        gents_protocol::graphql::graphql_rows_from_response(&response, "PeerPairingDesired")
            .into_iter()
            .filter_map(|row| row["_docID"].as_str().map(str::to_owned))
            .collect()
    }

    async fn command(
        &self,
        signer: &dyn NodeIdentity,
        doc: &str,
        grant: bool,
    ) -> PairingReaderCommand {
        let mut command = PairingReaderCommand {
            version: PAIRING_READER_COMMAND_VERSION,
            node_did: self.home.did().to_owned(),
            observation: PairingObservation::Desired,
            doc_id: doc.to_owned(),
            reason: PairingReadReason::RouteAdmission,
            reader_did: self.target.as_str().to_owned(),
            grant,
            signer_did: signer.did().to_owned(),
            issued_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            nonce: uuid::Uuid::new_v4().simple().to_string(),
            sig: Vec::new(),
        };
        command.sig = signer.sign(&command.signing_payload()).await.unwrap();
        command
    }

    async fn post(
        &self,
        command: &PairingReaderCommand,
        bearer: Option<&dyn NodeIdentity>,
    ) -> (StatusCode, Value) {
        let request = reqwest::Client::new()
            .post(format!("http://{}{PAIRING_READER_PATH}", self.address))
            .json(command);
        let request = match bearer {
            Some(key) => self.route_endpoint(key).authorize(request).unwrap(),
            None => request,
        };
        let response = request.send().await.unwrap();
        assert_eq!(response.headers()["cache-control"], "no-store");
        let status = response.status();
        (status, response.json().await.unwrap())
    }

    /// The CLI's own client for the route.
    async fn over_http(
        &self,
        key: &dyn NodeIdentity,
        doc: &str,
        grant: bool,
    ) -> anyhow::Result<()> {
        crate::commands::p2p::pairings::request_pairing_reader(
            key,
            &self.route_endpoint(key),
            self.home.did(),
            PairingObservation::Desired,
            doc,
            PairingReadReason::RouteAdmission,
            &self.target,
            grant,
        )
        .await
    }

    /// The requester's own embedded configuration transaction.
    async fn embedded(&self, key: &dyn NodeIdentity, doc: &str, grant: bool) -> anyhow::Result<()> {
        ConfigAccess::local_managed_agent(self.home.node.clone(), did(key.did()), key)?
            .set_pairing_reader(
                PairingObservation::Desired,
                doc,
                PairingReadReason::RouteAdmission,
                &self.target,
                grant,
            )
            .await?;
        Ok(())
    }
}

fn route_outcome(status: StatusCode, body: &Value) -> &'static str {
    match (status, body["code"].as_str()) {
        (StatusCode::OK, _) => "staged",
        (StatusCode::FORBIDDEN, Some("denied")) => "denied",
        (StatusCode::CONFLICT, Some("unmanaged")) => "unmanaged",
        (StatusCode::FORBIDDEN, Some(_)) => "forbidden",
        _ => panic!("unexpected route response {status}: {body}"),
    }
}

fn error_outcome(result: &anyhow::Result<()>, collection: &str, doc: &str) -> &'static str {
    let Err(error) = result else {
        return "staged";
    };
    match error.downcast_ref::<ConfigClientError>() {
        Some(ConfigClientError::OwnerStagedRelationshipDenied {
            collection: refused,
            doc_id,
        }) if refused == collection && doc_id == doc => "denied",
        Some(ConfigClientError::OwnerStagedRelationshipUnmanaged {
            collection: refused,
            doc_id,
        }) if refused == collection && doc_id == doc => "unmanaged",
        _ => panic!("unexpected pairing reader error: {error:#}"),
    }
}

#[tokio::test]
async fn pairing_reader_route_replays_generated_cases_over_signed_http() {
    let generated = generated();
    assert_eq!(generated.pairing.len(), 12);
    let f = Fixture::start().await;
    let mut denied_bodies: Vec<(String, StatusCode, Value)> = Vec::new();
    for case in &generated.pairing {
        let key = f.requester(case["requester"].as_str().unwrap()).clone();
        let doc = f.document(case["document"].as_str().unwrap()).to_owned();
        let staged = case["staged"].as_bool().unwrap();
        assert!(f.readable().await.is_empty(), "{case}");
        let (status, body) = f
            .post(
                &f.command(key.as_ref(), &doc, true).await,
                Some(key.as_ref()),
            )
            .await;
        assert_eq!(route_outcome(status, &body), case["http"], "{case}: {body}");
        assert_eq!(f.readable().await == vec![doc.clone()], staged, "{case}");
        if case["http"] == "denied" {
            denied_bodies.push((case["requester"].as_str().unwrap().to_owned(), status, body));
        }
        if staged {
            let (status, body) = f
                .post(
                    &f.command(key.as_ref(), &doc, false).await,
                    Some(key.as_ref()),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{case}: {body}");
            assert!(f.readable().await.is_empty(), "{case}");
        }
    }
    // No existence oracle: a denied requester gets the identical response for
    // the runtime-owned, missing and foreign observations.
    for requester in ["outsider", "other-admin"] {
        let responses: Vec<_> = denied_bodies
            .iter()
            .filter(|(name, ..)| name == requester)
            .map(|(_, status, body)| (*status, body.clone()))
            .collect();
        assert_eq!(responses.len(), 3, "{requester}");
        assert!(
            responses.windows(2).all(|pair| pair[0] == pair[1]),
            "{responses:?}"
        );
    }

    // `decideCommand`: the bearer must be the signer and the signature valid.
    let user = f.user.clone();
    for case in &generated.commands {
        if !case["authorityMatches"].as_bool().unwrap() {
            // Host authority is the full router's middleware; the route is in
            // `foreign_host_is_refused_on_public_private_and_independently_authenticated_routes`.
            assert_eq!(case["decision"], "misdirected", "{case}");
            continue;
        }
        let mut command = f.command(user.as_ref(), &f.route, true).await;
        if !case["signatureValid"].as_bool().unwrap() {
            command.sig[0] ^= 1;
        }
        let bearer = match case["caller"].as_str().unwrap() {
            "signer" => Some(user.as_ref()),
            "other" => Some(f.admin.as_ref()),
            "anonymous" => None,
            other => panic!("unknown modeled caller {other}"),
        };
        let (status, body) = f.post(&command, bearer).await;
        assert_eq!(
            route_outcome(status, &body),
            case["decision"],
            "{case}: {body}"
        );
        let staged = case["decision"] == "staged";
        assert_eq!(
            f.readable().await == vec![f.route.clone()],
            staged,
            "{case}"
        );
        if staged {
            let (status, _) = f
                .post(
                    &f.command(user.as_ref(), &f.route, false).await,
                    Some(user.as_ref()),
                )
                .await;
            assert_eq!(status, StatusCode::OK);
        }
    }

    // A command changed after signing, or replayed, is refused before it is
    // interpreted.
    let mut tampered = f.command(user.as_ref(), &f.route, true).await;
    tampered.reader_did = f.admin.did().to_owned();
    let (status, body) = f.post(&tampered, Some(user.as_ref())).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("invalidSignature"))
    );
    let command = f.command(user.as_ref(), &f.route, true).await;
    let (status, _) = f.post(&command, Some(user.as_ref())).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = f.post(&command, Some(user.as_ref())).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("invalidSignature"))
    );
    let (status, _) = f
        .post(
            &f.command(user.as_ref(), &f.route, false).await,
            Some(user.as_ref()),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let mut foreign_node = f.command(user.as_ref(), &f.route, true).await;
    foreign_node.node_did = f.other_admin.did().to_owned();
    foreign_node.sig = user.sign(&foreign_node.signing_payload()).await.unwrap();
    let (status, body) = f.post(&foreign_node, Some(user.as_ref())).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("differentNode"))
    );
    assert!(f.readable().await.is_empty());
    f.home.node.shutdown().await.unwrap();
}

/// `pairing_outcome_is_transport_independent`: for every modeled requester
/// and observation, the CLI's request over the runtime route and the
/// requester's embedded transaction return the same error variant and leave
/// the same reader.
#[tokio::test]
async fn pairing_reader_outcomes_agree_across_embedded_and_http() {
    let generated = generated();
    let f = Fixture::start().await;
    let collection = PairingObservation::Desired.collection();
    for case in &generated.pairing {
        let key = f.requester(case["requester"].as_str().unwrap()).clone();
        let doc = f.document(case["document"].as_str().unwrap()).to_owned();
        let mut observed = Vec::new();
        for transport in ["embedded", "http"] {
            assert!(f.readable().await.is_empty(), "{case}");
            let result = match transport {
                "embedded" => f.embedded(key.as_ref(), &doc, true).await,
                _ => f.over_http(key.as_ref(), &doc, true).await,
            };
            let outcome = error_outcome(&result, collection, &doc);
            let readable = f.readable().await == vec![doc.clone()];
            if outcome == "staged" {
                match transport {
                    "embedded" => f.embedded(key.as_ref(), &doc, false).await.unwrap(),
                    _ => f.over_http(key.as_ref(), &doc, false).await.unwrap(),
                }
            }
            observed.push((outcome, readable));
        }
        assert_eq!(observed[0], observed[1], "{case}");
        assert_eq!(observed[0].0, case["outcome"], "{case}");
        assert_eq!(observed[0].1, case["staged"].as_bool().unwrap(), "{case}");
    }
    f.home.node.shutdown().await.unwrap();
}
