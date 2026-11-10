//! Lean `Identity.OwnerStaged.overHttp` and `applyOverHttp` over real signed
//! HTTP: a trial home serves the owner-staged route on its loopback endpoint,
//! as the serving runtime and offline commands do, and decides each modeled
//! requester's command exactly as that requester's embedded transaction does.

use std::sync::{Arc, Mutex};

use gents_protocol::owner_staged::{
    OwnerStagedChange, OwnerStagedCommand, OWNER_STAGED_COMMAND_VERSION, OWNER_STAGED_PATH,
};
use reqwest::StatusCode;
use serde_json::Value;

use crate::eval::runner::embedded::EmbeddedHome;
use crate::store::config_client::{
    ConfigAccess, ConfigClientError, ConfigRelationshipGrant, GraphqlEndpoint, PairingObservation,
    PairingReadReason,
};
use crate::store::identity::NodeIdentity;

#[derive(serde::Deserialize)]
struct Generated {
    pairing: Vec<Value>,
    commands: Vec<Value>,
    http_apply: Vec<Value>,
}

fn generated() -> Generated {
    let stdout = gents_lean_contract::run_prepared_generator(
        &gents_lean_contract::proofs_dir().unwrap(),
        "Proofs.Conformance.OwnerStagedRoute:olean",
        "Proofs/Conformance/OwnerStagedRoute.lean",
    )
    .unwrap();
    serde_json::from_str(stdout.trim()).unwrap()
}

/// A runtime home whose Node is owned by `user` and administered by `admin`,
/// another Node administered by `other_admin`, and three pairing
/// observations: runtime-owned, foreign-owned and missing. Every requester is
/// a managed Agent so its embedded request can be signed too.
struct Fixture {
    home: EmbeddedHome,
    user: Arc<dyn NodeIdentity>,
    admin: Arc<dyn NodeIdentity>,
    outsider: Arc<dyn NodeIdentity>,
    other_admin: Arc<dyn NodeIdentity>,
    target: identity::Did,
    route: String,
    foreign: String,
    missing: String,
    graphql: String,
    _keys: tempfile::TempDir,
}

fn did(value: &str) -> identity::Did {
    identity::Did::new(value).unwrap()
}

fn endpoint(graphql: &str, key: &dyn NodeIdentity) -> GraphqlEndpoint {
    GraphqlEndpoint::as_principal(graphql, key.did())
        .with_client_signing_capability(key.client_signing_capability().unwrap())
        .unwrap()
}

fn pairing(author: &str, peer: &str) -> String {
    format!(
        r#"mutation {{ create_PeerPairingDesired(input: {{node_did: "{author}", actor_did: "{author}",
        peer_id: "{peer}", source: "enrollment", collections: ["AgentSessionInputEdit"],
        replicator_addresses: ["{peer}-address"], parent_linked: false}}) {{_docID}} }}"#,
        author = crate::store::graphql::escape_graphql_string(author),
    )
}

impl Fixture {
    async fn start() -> Self {
        let home = EmbeddedHome::create_temp("owner-staged-route")
            .await
            .unwrap();
        let keys = tempfile::tempdir().unwrap();
        let runtime = home.identity.clone();
        let runtime_did = did(runtime.did());
        let node = home.node.clone();
        let graphql = home.graphql_endpoint().unwrap().to_owned();
        let managed = || -> Arc<dyn NodeIdentity> {
            let binding =
                crate::store::identity::provision_agent_identity(runtime.as_ref()).unwrap();
            crate::store::identity::resolve_agent_identity(runtime.as_ref(), &binding).unwrap()
        };
        let (user, admin, outsider, other_admin, target) =
            (managed(), managed(), managed(), managed(), managed());
        let stranger: Arc<dyn NodeIdentity> = Arc::new(
            crate::KeyIdentity::load_or_create(keys.path().join("stranger.key"), None).unwrap(),
        );
        let runtime_access = ConfigAccess::local_as(node.clone(), runtime_did.clone());
        for actor in [&user, &admin, &outsider, &other_admin, &target, &stranger] {
            crate::store::identity::GrantSet::OperatorUser
                .grant(&runtime_access, &did(actor.did()))
                .await
                .unwrap();
        }
        let user_access = ConfigAccess::Graphql(endpoint(&graphql, user.as_ref()));
        for (node_did, administrator) in [(runtime.did(), &admin), (stranger.did(), &other_admin)] {
            let created = user_access
                .write(
                    "test.owner_staged_route.node",
                    &format!(
                        r#"mutation {{ create_Node(input: {{node_did: "{}"}}) {{_docID}} }}"#,
                        crate::store::graphql::escape_graphql_string(node_did)
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
            .write(
                "test.owner_staged_route.route",
                &pairing(runtime.did(), "route"),
            )
            .await
            .unwrap();
        let route =
            gents_protocol::graphql::extract_mutation_doc_id(&route, "PeerPairingDesired").unwrap();
        // The runtime may read the foreign observation, so only its owner
        // check refuses it.
        let stranger_access = ConfigAccess::Graphql(endpoint(&graphql, stranger.as_ref()));
        let foreign = stranger_access
            .write(
                "test.owner_staged_route.foreign",
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
            .transact_write("test.owner_staged_route.missing", |txn| {
                let (missing, runtime_did) = (&missing, &runtime_did);
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
            graphql,
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

    fn client(&self, key: &dyn NodeIdentity) -> ConfigAccess {
        ConfigAccess::Graphql(endpoint(&self.graphql, key))
    }

    fn change(&self, doc: &str) -> OwnerStagedChange {
        OwnerStagedChange::PairingReader {
            observation: PairingObservation::Desired,
            doc_id: doc.to_owned(),
            reason: PairingReadReason::RouteAdmission,
            reader_did: self.target.to_string(),
        }
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
    ) -> OwnerStagedCommand {
        let mut command = OwnerStagedCommand {
            version: OWNER_STAGED_COMMAND_VERSION,
            audience: endpoint(&self.graphql, signer).audience().unwrap(),
            change: self.change(doc),
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
        command: &OwnerStagedCommand,
        bearer: Option<&dyn NodeIdentity>,
    ) -> (StatusCode, Value) {
        let mut url = reqwest::Url::parse(&self.graphql).unwrap();
        url.set_path(OWNER_STAGED_PATH);
        let request = reqwest::Client::new().post(url).json(command);
        let request = match bearer {
            Some(key) => endpoint(&self.graphql, key).authorize(request).unwrap(),
            None => request,
        };
        let response = request.send().await.unwrap();
        assert_eq!(response.headers()["cache-control"], "no-store");
        let status = response.status();
        (status, response.json().await.unwrap())
    }

    /// The requester's request over the runtime's route or in its own
    /// embedded configuration transaction.
    async fn request(
        &self,
        over_http: bool,
        key: &dyn NodeIdentity,
        doc: &str,
        grant: bool,
    ) -> Result<(), ConfigClientError> {
        if over_http {
            self.client(key)
                .request_owner_staged(&self.change(doc), grant)
                .await
        } else {
            self.embedded(key, doc, grant).await
        }
    }

    /// The requester's own embedded configuration transaction.
    async fn embedded(
        &self,
        key: &dyn NodeIdentity,
        doc: &str,
        grant: bool,
    ) -> Result<(), ConfigClientError> {
        ConfigAccess::local_managed_agent(self.home.node.clone(), did(key.did()), key)?
            .request_owner_staged(&self.change(doc), grant)
            .await
    }
}

fn route_outcome(status: StatusCode, body: &Value) -> &'static str {
    match (status, body["code"].as_str()) {
        (StatusCode::OK, _) => "staged",
        (StatusCode::FORBIDDEN, Some("denied")) => "denied",
        (StatusCode::CONFLICT, Some("unmanaged")) => "unmanaged",
        (StatusCode::FORBIDDEN, Some(_)) => "forbidden",
        (StatusCode::MISDIRECTED_REQUEST, Some("misdirected")) => "misdirected",
        _ => panic!("unexpected route response {status}: {body}"),
    }
}

fn error_outcome(result: &Result<(), ConfigClientError>, doc: &str) -> &'static str {
    let collection = PairingObservation::Desired.collection();
    match result {
        Ok(()) => "staged",
        Err(ConfigClientError::OwnerStagedRelationshipDenied {
            collection: refused,
            doc_id,
        }) if refused == collection && doc_id == doc => "denied",
        Err(ConfigClientError::OwnerStagedRelationshipUnmanaged {
            collection: refused,
            doc_id,
        }) if refused == collection && doc_id == doc => "unmanaged",
        Err(error) => panic!("unexpected owner-staged error: {error:#}"),
    }
}

#[tokio::test]
async fn owner_staged_route_replays_generated_cases_over_signed_http() {
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

    // `decideCommand`: the command must address this endpoint, the bearer
    // must be the signer and the signature valid.
    let user = f.user.clone();
    for case in &generated.commands {
        let mut command = f.command(user.as_ref(), &f.route, true).await;
        if !case["authorityMatches"].as_bool().unwrap() {
            command.audience = "attacker.example".into();
        }
        if !case["signatureValid"].as_bool().unwrap() {
            command.sig[0] ^= 1;
        } else {
            command.sig = user.sign(&command.signing_payload()).await.unwrap();
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
    tampered.change = OwnerStagedChange::PlacementReader {
        placement_doc_id: f.route.clone(),
        reader_did: f.target.to_string(),
    };
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
    assert!(f.readable().await.is_empty());
    f.home.node.shutdown().await.unwrap();
}

/// `outcome_is_transport_independent`: for every modeled requester and
/// observation, the client's request over the runtime route and the
/// requester's embedded transaction return the same error variant and leave
/// the same reader.
#[tokio::test]
async fn owner_staged_outcomes_agree_across_embedded_and_http() {
    let generated = generated();
    let f = Fixture::start().await;
    for case in &generated.pairing {
        let key = f.requester(case["requester"].as_str().unwrap()).clone();
        let doc = f.document(case["document"].as_str().unwrap()).to_owned();
        let mut observed = Vec::new();
        for over_http in [false, true] {
            assert!(f.readable().await.is_empty(), "{case}");
            let result = f.request(over_http, key.as_ref(), &doc, true).await;
            let outcome = error_outcome(&result, &doc);
            let readable = f.readable().await == vec![doc.clone()];
            if outcome == "staged" {
                f.request(over_http, key.as_ref(), &doc, false)
                    .await
                    .unwrap();
            }
            observed.push((outcome, readable));
        }
        assert_eq!(observed[0], observed[1], "{case}");
        assert_eq!(observed[0].0, case["outcome"], "{case}");
        assert_eq!(observed[0].1, case["staged"].as_bool().unwrap(), "{case}");
    }
    f.home.node.shutdown().await.unwrap();
}

/// `applyOverHttp`: in a client's HTTP configuration transaction a refused
/// revocation aborts the apply before its documents commit, and a refused
/// grant is the apply's `CommittedRelationshipsIncomplete` error after they
/// commit, with that grant pending.
#[tokio::test]
async fn http_apply_orders_revocations_before_and_grants_after_commit() {
    let generated = generated();
    assert_eq!(generated.http_apply.len(), 12);
    let f = Fixture::start().await;
    for case in &generated.http_apply {
        let key = f.requester(case["requester"].as_str().unwrap()).clone();
        let direction = case["direction"].as_str().unwrap();
        let grant = match direction {
            "grant" => true,
            "revoke" | "replan" => false,
            other => panic!("unknown modeled direction {other}"),
        };
        let marker = format!("did:key:z{}", uuid::Uuid::new_v4().simple());
        // `replan`: a concurrent writer commits `changed` between PLAN and
        // WRITE, after which the apply also derives revoking the foreign
        // observation's reader.
        let changed = format!("did:key:z{}", uuid::Uuid::new_v4().simple());
        // The requester's own concurrent writer, so WRITE can read its row.
        let writer = f.client(key.as_ref());
        let passes = std::sync::atomic::AtomicUsize::new(0);
        let client = f.client(key.as_ref());
        let applied = client
            .transact_write("test.owner_staged_route.apply", |txn| {
                let (marker, changed, writer, passes) = (&marker, &changed, &writer, &passes);
                let f = &f;
                Box::pin(async move {
                    passes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    txn.execute(&format!(
                        r#"mutation {{ create_Node(input: {{node_did: "{marker}"}}) {{_docID}} }}"#
                    ))
                    .await?;
                    let seen = txn
                        .execute(&format!(
                            r#"{{ Node(filter: {{node_did: {{_eq: "{changed}"}}}}) {{_docID}} }}"#
                        ))
                        .await?;
                    let seen =
                        !gents_protocol::graphql::graphql_rows_from_response(&seen, "Node")
                            .is_empty();
                    txn.request_owner_staged(&f.change(&f.route), grant).await?;
                    if direction == "replan" {
                        if seen {
                            txn.request_owner_staged(&f.change(&f.foreign), false).await?;
                        } else {
                            writer
                                .write(
                                    "test.owner_staged_route.concurrent",
                                    &format!(
                                        r#"mutation {{ create_Node(input: {{node_did: "{changed}"}}) {{_docID}} }}"#
                                    ),
                                )
                                .await?;
                        }
                    }
                    Ok(())
                })
            })
            .await;
        let result = match &applied {
            Ok(()) => "applied",
            Err(ConfigClientError::OwnerStagedRelationshipDenied { doc_id, .. })
                if doc_id == &f.route =>
            {
                "aborted"
            }
            Err(ConfigClientError::OwnerStagedPlanChanged { revoked: 1 }) => "plan_changed",
            Err(ConfigClientError::CommittedRelationshipsIncomplete {
                creator,
                pending,
                source,
            }) => {
                assert_eq!(creator, key.did(), "{case}");
                assert_eq!(
                    pending,
                    &vec![ConfigRelationshipGrant::OwnerStaged {
                        change: f.change(&f.route)
                    }],
                    "{case}"
                );
                assert!(
                    matches!(
                        source.as_ref(),
                        ConfigClientError::OwnerStagedRelationshipDenied { .. }
                    ),
                    "{case}: {source:?}"
                );
                "committed_incomplete"
            }
            Err(error) => panic!("{case}: unexpected apply error {error:#}"),
        };
        assert_eq!(result, case["result"], "{case}");
        // PLAN runs once; WRITE runs again only after its revocations were
        // sent, so a refused revocation aborts before any write attempt.
        let expected_passes = if grant || result == "aborted" { 1 } else { 2 };
        assert_eq!(
            passes.load(std::sync::atomic::Ordering::SeqCst),
            expected_passes,
            "{case}"
        );
        let committed = client
            .execute(&format!(
                r#"{{ Node(filter: {{node_did: {{_eq: "{marker}"}}}}) {{_docID}} }}"#
            ))
            .await
            .unwrap();
        assert_eq!(
            !gents_protocol::graphql::graphql_rows_from_response(&committed, "Node").is_empty(),
            case["committed"].as_bool().unwrap(),
            "{case}"
        );
        let readable = f.readable().await == vec![f.route.clone()];
        assert_eq!(readable, case["readable"].as_bool().unwrap(), "{case}");
        if readable {
            f.client(f.user.as_ref())
                .request_owner_staged(&f.change(&f.route), false)
                .await
                .unwrap();
        }
    }
    f.home.node.shutdown().await.unwrap();
}
