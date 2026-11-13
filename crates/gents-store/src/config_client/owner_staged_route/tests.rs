//! Lean `Identity.OwnerStaged.overHttp` and `applyOverHttp` over real signed
//! HTTP: a node serves the owner-staged route on its loopback endpoint, as the
//! serving runtime and offline commands do, and decides each modeled
//! requester's command exactly as that requester's embedded transaction does.

use std::sync::{Arc, Mutex};

use gents_protocol::owner_staged::{
    OwnerStagedChange, OwnerStagedCommand, OWNER_STAGED_COMMAND_VERSION, OWNER_STAGED_PATH,
};
use reqwest::StatusCode;
use serde_json::Value;

use crate::config_client::{
    ConfigAccess, ConfigApplyTxn, ConfigClientError, ConfigRelationshipGrant, GraphqlEndpoint,
    OwnerStagedRoute, PairingObservation, PairingReadReason,
};
use crate::identity::NodeIdentity;

/// A runtime node whose Node row is owned by `user` and administered by `admin`,
/// another Node administered by `other_admin`, and three pairing
/// observations: runtime-owned, foreign-owned and missing. Every requester is
/// a managed Agent so its embedded request can be signed too.
struct Fixture {
    node: Arc<defra_node::Node>,
    runtime: Arc<dyn NodeIdentity>,
    user: Arc<dyn NodeIdentity>,
    admin: Arc<dyn NodeIdentity>,
    outsider: Arc<dyn NodeIdentity>,
    other_admin: Arc<dyn NodeIdentity>,
    target: identity::Did,
    route: String,
    foreign: String,
    missing: String,
    graphql: String,
    keys: tempfile::TempDir,
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
        author = crate::graphql::escape_graphql_string(author),
    )
}

impl Fixture {
    async fn start() -> Self {
        let keys = tempfile::tempdir().unwrap();
        let runtime: Arc<dyn NodeIdentity> = Arc::new(
            crate::identity::KeyIdentity::load_or_create(keys.path().join("runtime.key"), None)
                .unwrap(),
        );
        let runtime_did = did(runtime.did());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let graphql = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
        let route = OwnerStagedRoute::new();
        let node = Arc::new(
            defra_node::Node::builder()
                .enable_signing()
                .with_node_identity_did(runtime.did())
                .with_node_acp_owner(runtime.did().to_owned())
                .with_http(
                    defra_node::HttpConfig::with_listener(listener)
                        .unwrap()
                        .with_extra_routes(route.router()),
                )
                .build()
                .await
                .unwrap(),
        );
        route.serve(&node, runtime.clone());
        crate::schema::ensure_runtime_schemas(&node).await.unwrap();
        let managed = || -> Arc<dyn NodeIdentity> {
            let binding = crate::identity::provision_agent_identity(runtime.as_ref()).unwrap();
            crate::identity::resolve_agent_identity(runtime.as_ref(), &binding).unwrap()
        };
        let (user, admin, outsider, other_admin, target) =
            (managed(), managed(), managed(), managed(), managed());
        let stranger: Arc<dyn NodeIdentity> = Arc::new(
            crate::identity::KeyIdentity::load_or_create(keys.path().join("stranger.key"), None)
                .unwrap(),
        );
        let runtime_access = ConfigAccess::local_as(node.clone(), runtime_did.clone());
        for actor in [&user, &admin, &outsider, &other_admin, &target, &stranger] {
            crate::identity::GrantSet::OperatorUser
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
                        crate::graphql::escape_graphql_string(node_did)
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
            node,
            runtime,
            user,
            admin,
            outsider,
            other_admin,
            target: did(target.did()),
            route,
            foreign,
            missing,
            graphql,
            keys,
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
        let response = ConfigAccess::local_as(self.node.clone(), self.target.clone())
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

    /// The requester's own embedded configuration transaction.
    async fn embedded(
        &self,
        key: &dyn NodeIdentity,
        doc: &str,
        grant: bool,
    ) -> Result<(), ConfigClientError> {
        ConfigAccess::local_managed_agent(self.node.clone(), did(key.did()), key)?
            .request_owner_staged(&self.change(doc), grant)
            .await
    }

    async fn embedded_outcome(&self, key: &dyn NodeIdentity, doc: &str) -> (&'static str, bool) {
        let result = self.embedded(key, doc, true).await;
        let outcome = error_outcome(&result, doc);
        let staged = self.readable().await == vec![doc.to_owned()];
        if staged {
            self.embedded(key, doc, false).await.unwrap();
        }
        (outcome, staged)
    }

    async fn direct(&self, key: &dyn NodeIdentity, change: &OwnerStagedChange) -> bool {
        let client = self.client(key);
        let direct = client
            .transact_write("test.owner_staged_route.direct", |txn| {
                Box::pin(native_change(txn, change, false))
            })
            .await
            .is_ok();
        if direct {
            client
                .transact_write("test.owner_staged_route.direct_undo", |txn| {
                    Box::pin(native_change(txn, change, true))
                })
                .await
                .unwrap();
        }
        direct
    }

    async fn reads(&self, subject: &identity::Did, collection: &str, doc: &str) -> bool {
        let response = ConfigAccess::local_as(self.node.clone(), subject.clone())
            .execute(&format!("{{ {collection} {{ _docID }} }}"))
            .await
            .unwrap();
        gents_protocol::graphql::graphql_rows_from_response(&response, collection)
            .iter()
            .any(|row| row["_docID"].as_str() == Some(doc))
    }
}

async fn native_change(
    txn: &ConfigApplyTxn<'_>,
    change: &OwnerStagedChange,
    delete: bool,
) -> Result<(), ConfigClientError> {
    let actor = |collection: &'static str, doc: &str, relation: &'static str, target: &str| {
        let target = did(target);
        let doc = doc.to_owned();
        async move {
            if delete {
                txn.delete_actor_relationship(collection, &doc, relation, &target)
                    .await
            } else {
                txn.add_actor_relationship(collection, &doc, relation, &target)
                    .await
            }
        }
    };
    match change {
        OwnerStagedChange::EventCursorWriter {
            cursor_doc_id,
            agent_did,
        } => actor("EventSourceCursor", cursor_doc_id, "writer", agent_did).await,
        OwnerStagedChange::PlacementReader {
            placement_doc_id,
            reader_did,
        } => {
            actor(
                "RepositoryPlacement",
                placement_doc_id,
                "reader",
                reader_did,
            )
            .await
        }
        OwnerStagedChange::PairingReader {
            observation,
            doc_id,
            reason,
            reader_did,
        } => {
            actor(
                observation.collection(),
                doc_id,
                super::super::pairing_reader::reader_relation(*reason),
                reader_did,
            )
            .await
        }
        OwnerStagedChange::WorkspaceSelection {
            agent_doc_id,
            callback_doc_id,
        } => {
            let target = ("configuration", callback_doc_id.as_str(), "reader");
            if delete {
                txn.delete_entity_set_relationship(
                    "Agent",
                    agent_doc_id,
                    "workspace_selected_by",
                    target.0,
                    target.1,
                    target.2,
                )
                .await
            } else {
                txn.add_entity_set_relationship(
                    "Agent",
                    agent_doc_id,
                    "workspace_selected_by",
                    target.0,
                    target.1,
                    target.2,
                )
                .await
            }
        }
    }
}

fn fixture_callback_agents(
    _node_did: &str,
    callback: &Value,
) -> anyhow::Result<std::collections::BTreeSet<String>> {
    let agents = &callback["handler"]["workspace_agents"];
    Ok(["writer_agent_did", "integrator_agent_did"]
        .into_iter()
        .filter_map(|field| agents[field].as_str())
        .chain(
            agents["readers"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|reader| reader["agent_did"].as_str()),
        )
        .map(str::to_owned)
        .collect())
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

/// Lean `Conformance.OwnerStaged.pairing`, with `outcome` decided in the
/// requester's embedded transaction and `http` answered by a signed command to
/// the served route, and `Conformance.OwnerStaged.routeCommands`.
#[tokio::test]
async fn owner_staged_route_replays_generated_cases_over_signed_http() {
    let f = Fixture::start().await;
    let mut denied_bodies: Vec<(String, StatusCode, Value)> = Vec::new();
    for case in gents_lean_vocab::families::owner_staged_pairing::cases() {
        let input = &case.input;
        let key = f.requester(&input.requester).clone();
        let doc = f.document(&input.document).to_owned();
        assert!(f.readable().await.is_empty(), "{input:?}");
        let direct = f.direct(key.as_ref(), &f.change(&doc)).await;
        let (outcome, staged) = f.embedded_outcome(key.as_ref(), &doc).await;
        let (status, body) = f
            .post(
                &f.command(key.as_ref(), &doc, true).await,
                Some(key.as_ref()),
            )
            .await;
        let http = route_outcome(status, &body);
        let readable = f.readable().await == vec![doc.clone()];
        assert_eq!(readable, http == "staged", "{input:?}: {body}");
        if http == "denied" {
            denied_bodies.push((input.requester.clone(), status, body));
        }
        if readable {
            let (status, body) = f
                .post(
                    &f.command(key.as_ref(), &doc, false).await,
                    Some(key.as_ref()),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{input:?}: {body}");
            assert!(f.readable().await.is_empty(), "{input:?}");
        }
        case.check(
            gents_lean_vocab::families::owner_staged_pairing::PairingOutput {
                direct,
                staged,
                outcome: outcome.to_owned(),
                http: http.to_owned(),
            },
        );
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
    for case in gents_lean_vocab::families::owner_staged_route_commands::cases() {
        let input = &case.input;
        let mut command = f.command(user.as_ref(), &f.route, true).await;
        if !input.authority_matches {
            command.audience = "attacker.example".into();
        }
        if !input.signature_valid {
            command.sig[0] ^= 1;
        } else {
            command.sig = user.sign(&command.signing_payload()).await.unwrap();
        }
        let bearer = match input.caller.as_str() {
            "signer" => Some(user.as_ref()),
            "other" => Some(f.admin.as_ref()),
            "anonymous" => None,
            other => panic!("unknown modeled caller {other}"),
        };
        let (status, body) = f.post(&command, bearer).await;
        let decision = route_outcome(status, &body);
        let staged = f.readable().await == vec![f.route.clone()];
        assert_eq!(staged, decision == "staged", "{input:?}: {body}");
        case.check(
            gents_lean_vocab::families::owner_staged_route_commands::CommandOutput {
                decision: decision.to_owned(),
            },
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
    // A route transaction never runs as the runtime itself.
    assert!(matches!(
        ConfigAccess::local_as(f.node.clone(), did(f.runtime.did()))
            .request_verified_client_owner_staged(&f.change(&f.route), true)
            .await,
        Err(ConfigClientError::TransactionActorMismatch)
    ));
    f.node.shutdown().await.unwrap();
}

/// Lean `Conformance.OwnerStaged.httpApply`: in a client's HTTP configuration
/// transaction a refused revocation aborts the apply before its documents
/// commit, and a refused grant is the apply's `CommittedRelationshipsIncomplete`
/// error after they commit, with that grant pending.
#[tokio::test]
async fn http_apply_orders_revocations_before_and_grants_after_commit() {
    let f = Fixture::start().await;
    for case in gents_lean_vocab::families::owner_staged_http_apply::cases() {
        let input = &case.input;
        let key = f.requester(&input.requester).clone();
        let direction = input.direction.as_str();
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
                    // This callback states the replay contract: it re-reads
                    // its inputs and every pass signs afresh.
                    txn.authorize_planned_replay();
                    let pass = passes.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    txn.execute_client_signed_create(
                        "mutation($input: NodeMutationInputArg!) { create_Node(input: $input) { _docID } }",
                        &serde_json::json!({"input": {"node_did": format!("{marker}:{pass}")}}),
                        gents_protocol::schemas::NODE,
                    )
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
                assert_eq!(creator, key.did(), "{input:?}");
                assert_eq!(
                    pending,
                    &vec![ConfigRelationshipGrant::OwnerStaged {
                        change: f.change(&f.route)
                    }],
                    "{input:?}"
                );
                assert!(
                    matches!(
                        source.as_ref(),
                        ConfigClientError::OwnerStagedRelationshipDenied { .. }
                    ),
                    "{input:?}: {source:?}"
                );
                "committed_incomplete"
            }
            Err(error) => panic!("{input:?}: unexpected apply error {error:#}"),
        };
        // PLAN commits nothing; WRITE runs only after its revocations were
        // sent, so a refused revocation aborts before any write attempt. Each
        // pass writes its own marker row: only the committing pass's row
        // exists, and a discarded PLAN's row is absent.
        let passes = passes.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            passes,
            if grant || result == "aborted" { 1 } else { 2 },
            "{input:?}"
        );
        let mut present = Vec::new();
        for pass in 1..=passes {
            let rows = client
                .execute(&format!(
                    r#"{{ Node(filter: {{node_did: {{_eq: "{marker}:{pass}"}}}}) {{_docID}} }}"#
                ))
                .await
                .unwrap();
            if !gents_protocol::graphql::graphql_rows_from_response(&rows, "Node").is_empty() {
                present.push(pass);
            }
        }
        assert!(
            present.is_empty() || present == vec![passes],
            "{input:?}: {present:?}"
        );
        let readable = f.readable().await == vec![f.route.clone()];
        case.check(
            gents_lean_vocab::families::owner_staged_http_apply::ApplyOutput {
                result: result.to_owned(),
                committed: !present.is_empty(),
                readable,
            },
        );
        if readable {
            f.client(f.user.as_ref())
                .request_owner_staged(&f.change(&f.route), false)
                .await
                .unwrap();
        }
    }
    f.node.shutdown().await.unwrap();
}

async fn runtime_node_doc(f: &Fixture) -> String {
    let runtime = ConfigAccess::local_as(f.node.clone(), did(f.runtime.did()));
    let response = runtime
        .execute(&format!(
            r#"{{ Node(filter: {{node_did: {{_eq: "{}"}}}}) {{_docID}} }}"#,
            f.runtime.did()
        ))
        .await
        .unwrap();
    gents_protocol::graphql::graphql_rows_from_response(&response, "Node")[0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Create one document in `access`'s transaction and link it to the runtime's
/// Node, as its owner does.
async fn create_linked(
    access: &ConfigAccess,
    collection: &str,
    input: Value,
    node_doc: &str,
) -> String {
    access
        .transact_write("test.owner_staged_route.create", |txn| {
            let input = &input;
            Box::pin(async move {
                let created = txn
                    .execute_with_variables(
                        &format!(
                            "mutation($input: {collection}MutationInputArg!) {{ create_{collection}(input: $input) {{ _docID }} }}"
                        ),
                        &serde_json::json!({ "input": input }),
                    )
                    .await?;
                let doc = gents_protocol::graphql::extract_mutation_doc_id(&created, collection)?;
                txn.add_resource_relationship(collection, &doc, "parent", "nodes", node_doc)
                    .await?;
                Ok::<_, anyhow::Error>(doc)
            })
        })
        .await
        .unwrap()
}

/// A requester's grant over signed HTTP, or in its verified embedded
/// transaction with or without the runtime's derivation proof.
async fn staged_grant(
    f: &Fixture,
    key: &dyn NodeIdentity,
    change: &OwnerStagedChange,
    transport: &str,
) -> &'static str {
    let result = match transport {
        "http" => f.client(key).request_owner_staged(change, true).await,
        "embedded" | "no_proof" => {
            let access = ConfigAccess::local_as(f.node.clone(), did(key.did()));
            let access = if transport == "embedded" {
                access.with_runtime_agent_proof(crate::identity::RuntimeAgentProof::new(
                    f.runtime.clone(),
                ))
            } else {
                access
            };
            access
                .request_verified_client_owner_staged(change, true)
                .await
        }
        other => panic!("unknown transport {other}"),
    };
    match result {
        Ok(()) => "staged",
        Err(ConfigClientError::OwnerStagedRelationshipDenied { .. }) => "denied",
        Err(ConfigClientError::OwnerStagedRelationshipUnmanaged { .. }) => "unmanaged",
        Err(ConfigClientError::OwnerStagedTargetRefused { .. }) => "out_of_kind",
        Err(error) => panic!("unexpected owner-staged error {error:#}"),
    }
}

async fn genuine_agent(f: &Fixture, runtime_access: &ConfigAccess) -> Arc<dyn NodeIdentity> {
    let runtime = f.runtime.clone();
    let binding = crate::identity::provision_agent_identity(runtime.as_ref()).unwrap();
    agent_row(
        runtime_access,
        runtime.did(),
        &binding.agent_did,
        &binding.creation_nonce,
    )
    .await
    .unwrap();
    let agent = crate::identity::resolve_agent_identity(runtime.as_ref(), &binding).unwrap();
    crate::identity::GrantSet::OperatorUser
        .grant(runtime_access, &did(agent.did()))
        .await
        .unwrap();
    agent
}

async fn agent_row(
    runtime_access: &ConfigAccess,
    node_did: &str,
    agent_did: &str,
    nonce: &str,
) -> Result<String, ConfigClientError> {
    let created = runtime_access
        .write(
            "test.owner_staged_route.agent_row",
            &format!(
                r#"mutation {{ create_Agent(input: {{node_did: "{node_did}", agent_did: "{agent_did}", creation_nonce: "{nonce}", inference_profile_id: "profile", enabled: true}}) {{_docID}} }}"#
            ),
        )
        .await?;
    Ok(gents_protocol::graphql::extract_mutation_doc_id(&created, "Agent").unwrap())
}

/// Lean `Conformance.OwnerStaged.agentRows`
/// (`Identity.OwnerStaged.owner_rows_follow_the_derivation_proof`), replayed
/// on real Agent rows and placements over signed HTTP and embedded: the
/// runtime never stages as a DID a forged row names, as a foreign DID under a
/// genuine nonce, or as any Agent without its derivation proof; a genuine
/// Agent's placement is staged. Local writes keep `agent_did` unique, so a
/// duplicated genuine row arises only from a replicated merge: here its second
/// local write is refused.
#[tokio::test]
async fn forged_agent_row_never_makes_the_runtime_stage_as_its_did() {
    let f = Fixture::start().await;
    let runtime = f.runtime.clone();
    let runtime_access = ConfigAccess::local_as(f.node.clone(), did(runtime.did()));
    let node_doc = runtime_node_doc(&f).await;
    let key = |name: &str| -> Arc<dyn NodeIdentity> {
        Arc::new(
            crate::identity::KeyIdentity::load_or_create(f.keys.path().join(name), None).unwrap(),
        )
    };
    let (victim, foreign) = (key("victim.key"), key("foreign.key"));
    for actor in [&victim, &foreign] {
        crate::identity::GrantSet::OperatorUser
            .grant(&runtime_access, &did(actor.did()))
            .await
            .unwrap();
    }
    let agent = genuine_agent(&f, &runtime_access).await;
    let genuine_nonce = {
        let response = runtime_access
            .execute(&format!(
                r#"{{ Agent(filter: {{agent_did: {{_eq: "{}"}}}}) {{creation_nonce}} }}"#,
                agent.did()
            ))
            .await
            .unwrap();
        gents_protocol::graphql::graphql_rows_from_response(&response, "Agent")[0]["creation_nonce"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    agent_row(
        &runtime_access,
        runtime.did(),
        victim.did(),
        &"f".repeat(64),
    )
    .await
    .unwrap();
    agent_row(
        &runtime_access,
        runtime.did(),
        foreign.did(),
        &genuine_nonce,
    )
    .await
    .unwrap();
    assert!(
        agent_row(&runtime_access, runtime.did(), agent.did(), &genuine_nonce)
            .await
            .is_err(),
        "a second local row for a genuine Agent must be refused by its unique index"
    );
    for case in gents_lean_vocab::families::owner_staged_agent_rows::cases() {
        let input = &case.input;
        let owner = match input.path.as_str() {
            "forged_agent_row" => &victim,
            "genuine_nonce_foreign_did" => &foreign,
            "no_proof" | "derived_agent_row" => &agent,
            other => panic!("unknown modeled path {other}"),
        };
        let requester = match input.requester.as_str() {
            "admin" => f.admin.as_ref(),
            other => panic!("unknown modeled requester {other}"),
        };
        let transports: &[&str] = if input.path == "no_proof" {
            &["no_proof"]
        } else {
            &["http", "embedded"]
        };
        for transport in transports {
            let placement = create_linked(
                &f.client(owner.as_ref()),
                "RepositoryPlacement",
                serde_json::json!({
                    "repository_id": format!("{}-{transport}", input.path),
                    "node_did": runtime.did(),
                    "host_path": "/tmp/repository",
                    "enabled": true,
                    "readers": [{"agent_did": f.target.as_str()}],
                }),
                &node_doc,
            )
            .await;
            let change = OwnerStagedChange::PlacementReader {
                placement_doc_id: placement.clone(),
                reader_did: f.target.to_string(),
            };
            let direct = f.direct(requester, &change).await;
            let outcome = staged_grant(&f, requester, &change, transport).await;
            let staged = f.reads(&f.target, "RepositoryPlacement", &placement).await;
            case.check(
                gents_lean_vocab::families::owner_staged_agent_rows::RelationOutput {
                    direct,
                    staged,
                    outcome: outcome.to_owned(),
                },
            );
        }
    }
    f.node.shutdown().await.unwrap();
}

/// Lean `Conformance.OwnerStaged.targets`
/// (`Identity.OwnerStaged.out_of_kind_targets_are_refused`), replayed on real
/// runtime-owned documents over signed HTTP and in the requester's embedded
/// transaction: a cursor writer must be an Agent the runtime controls, a
/// placement reader must be declared by the placement, a workspace selection
/// must name a Callback bound to the Agent (its configuration selects the
/// Agent), and a writer of the cursor (document `update` without Node
/// authority) is denied.
#[tokio::test]
async fn owner_staged_targets_follow_their_operation_kind() {
    let f = Fixture::start().await;
    crate::node_extension::attach_all(
        &f.node,
        &crate::node_extension::Extensions::of(Arc::new(
            crate::config_client::CallbackAgentSelection(fixture_callback_agents),
        )),
    )
    .unwrap();
    let runtime = f.runtime.clone();
    let runtime_did = did(runtime.did());
    let runtime_access = ConfigAccess::local_as(f.node.clone(), runtime_did.clone());
    let node_doc = runtime_node_doc(&f).await;
    // The cursor's current writer and the Agent it is rebound to.
    let writer = genuine_agent(&f, &runtime_access).await;
    let sibling = genuine_agent(&f, &runtime_access).await;
    let sibling_doc = {
        let response = runtime_access
            .execute(&format!(
                r#"{{ Agent(filter: {{agent_did: {{_eq: "{}"}}}}) {{_docID}} }}"#,
                sibling.did()
            ))
            .await
            .unwrap();
        gents_protocol::graphql::graphql_rows_from_response(&response, "Agent")[0]["_docID"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    runtime_access
        .transact_write("test.owner_staged_route.agent_parent", |txn| {
            let (sibling_doc, node_doc) = (&sibling_doc, &node_doc);
            Box::pin(async move {
                txn.add_resource_relationship("Agent", sibling_doc, "parent", "nodes", node_doc)
                    .await
            })
        })
        .await
        .unwrap();
    let mut documents = Vec::new();
    for index in 0..2 {
        let cursor = create_linked(
            &runtime_access,
            "EventSourceCursor",
            serde_json::json!({
                "cursor_key": format!("cursor-{index}"),
                "owner_did": runtime.did(),
                "consumer": {"agent_did": writer.did()},
                "source_collection": "Node",
            }),
            &node_doc,
        )
        .await;
        runtime_access
            .transact_write("test.owner_staged_route.cursor_writer", |txn| {
                let (cursor, writer) = (&cursor, did(writer.did()));
                Box::pin(async move {
                    txn.add_actor_relationship("EventSourceCursor", cursor, "writer", &writer)
                        .await
                })
            })
            .await
            .unwrap();
        documents.push(cursor);
    }
    let placement = create_linked(
        &runtime_access,
        "RepositoryPlacement",
        serde_json::json!({
            "repository_id": "declared",
            "node_did": runtime.did(),
            "host_path": "/tmp/declared",
            "enabled": true,
            "readers": [{"agent_did": f.target.as_str()}],
        }),
        &node_doc,
    )
    .await;
    // A Callback selects the workspace Agents its handler names
    // (`workspace_selection::selected_agents`): `bound` names the sibling,
    // `unbound` names another Agent.
    let mut callbacks = Vec::new();
    for (callback, selected) in [("bound", sibling.did()), ("unbound", writer.did())] {
        callbacks.push(
            create_linked(
                &runtime_access,
                "Callback",
                serde_json::json!({
                    "callback_id": callback, "node_did": runtime.did(), "enabled": true,
                    "handler": {"kind": "built_in", "emitter": "create_workspace", "workspace_agents": {
                        "writer_agent_did": selected, "integrator_agent_did": selected}},
                    "capabilities": ["create_workspace", "observe_dirty_base"],
                }),
                &node_doc,
            )
            .await,
        );
    }
    for callback in &callbacks {
        crate::test_support::relationships::actor_in_txn(
            &runtime_access,
            "Callback",
            callback,
            "reader",
            &f.target,
            false,
        )
        .await
        .unwrap();
    }
    let outsider = did(f.outsider.did());
    let sibling_did = did(sibling.did());
    for (transport, cursor) in ["http", "embedded"].into_iter().zip(&documents) {
        for case in gents_lean_vocab::families::owner_staged_targets::cases() {
            let input = &case.input;
            let (change, subject, collection, doc) = match input.path.as_str() {
                "event_cursor_rebind" => (
                    OwnerStagedChange::EventCursorWriter {
                        cursor_doc_id: cursor.clone(),
                        agent_did: sibling.did().to_owned(),
                    },
                    &sibling_did,
                    "EventSourceCursor",
                    cursor,
                ),
                "event_cursor_outsider_writer" => (
                    OwnerStagedChange::EventCursorWriter {
                        cursor_doc_id: cursor.clone(),
                        agent_did: f.outsider.did().to_owned(),
                    },
                    &outsider,
                    "EventSourceCursor",
                    cursor,
                ),
                "declared_placement_readers" => (
                    OwnerStagedChange::PlacementReader {
                        placement_doc_id: placement.clone(),
                        reader_did: f.target.to_string(),
                    },
                    &f.target,
                    "RepositoryPlacement",
                    &placement,
                ),
                "placement_undeclared_reader" => (
                    OwnerStagedChange::PlacementReader {
                        placement_doc_id: placement.clone(),
                        reader_did: f.outsider.did().to_owned(),
                    },
                    &outsider,
                    "RepositoryPlacement",
                    &placement,
                ),
                "workspace_selection" => (
                    OwnerStagedChange::WorkspaceSelection {
                        agent_doc_id: sibling_doc.clone(),
                        callback_doc_id: callbacks[0].clone(),
                    },
                    &f.target,
                    "Agent",
                    &sibling_doc,
                ),
                "workspace_unbound_callback" => (
                    OwnerStagedChange::WorkspaceSelection {
                        agent_doc_id: sibling_doc.clone(),
                        callback_doc_id: callbacks[1].clone(),
                    },
                    &f.target,
                    "Agent",
                    &sibling_doc,
                ),
                other => panic!("unknown modeled path {other}"),
            };
            let requester: &dyn NodeIdentity = match input.requester.as_str() {
                "user" => f.user.as_ref(),
                "outsider" => f.outsider.as_ref(),
                "agent" => writer.as_ref(),
                other => panic!("unknown modeled requester {other}"),
            };
            assert!(
                !f.reads(subject, collection, doc).await,
                "{input:?} {transport}"
            );
            let direct = f.direct(requester, &change).await;
            let outcome = staged_grant(&f, requester, &change, transport).await;
            let staged = f.reads(subject, collection, doc).await;
            case.check(
                gents_lean_vocab::families::owner_staged_targets::RelationOutput {
                    direct,
                    staged,
                    outcome: outcome.to_owned(),
                },
            );
            if outcome == "staged" {
                let revoked = if transport == "http" {
                    f.client(requester)
                        .request_owner_staged(&change, false)
                        .await
                } else {
                    ConfigAccess::local_as(f.node.clone(), did(requester.did()))
                        .with_runtime_agent_proof(crate::identity::RuntimeAgentProof::new(
                            runtime.clone(),
                        ))
                        .request_verified_client_owner_staged(&change, false)
                        .await
                };
                revoked.unwrap();
            }
        }
    }
    f.node.shutdown().await.unwrap();
}

/// `Identity.OwnerStaged.applyOverHttp` with `planDiscarded = false`: when the
/// PLAN transaction cannot be confirmed discarded, nothing is revoked and the
/// apply is not run again.
#[tokio::test]
async fn unconfirmed_plan_discard_revokes_nothing() {
    let f = Fixture::start().await;
    f.client(f.user.as_ref())
        .request_owner_staged(&f.change(&f.route), true)
        .await
        .unwrap();
    assert_eq!(f.readable().await, vec![f.route.clone()]);
    let passes = std::sync::atomic::AtomicUsize::new(0);
    let client = f.client(f.admin.as_ref());
    let reached = Arc::new(tokio::sync::Notify::new());
    let cleanup = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let applied = crate::config_client::ConfigApplyTxn::with_rollback_failure_retry_boundary(
        reached,
        cleanup,
        client.transact_write("test.owner_staged_route.unconfirmed", |txn| {
            let (passes, change) = (&passes, f.change(&f.route));
            Box::pin(async move {
                txn.authorize_planned_replay();
                passes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                txn.request_owner_staged(&change, false).await
            })
        }),
    )
    .await;
    assert!(
        matches!(applied, Err(ConfigClientError::OwnerStagedPlanUnconfirmed)),
        "{applied:?}"
    );
    assert_eq!(passes.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(f.readable().await, vec![f.route.clone()]);
    f.node.shutdown().await.unwrap();
}

/// A PLAN pass that dispatched a client-signed graph reruns only with its
/// owner's replay authorization (`client_signed_write_may_retry`); without it
/// nothing is revoked.
#[tokio::test]
async fn unauthorized_signed_plan_is_not_rerun() {
    let f = Fixture::start().await;
    f.client(f.user.as_ref())
        .request_owner_staged(&f.change(&f.route), true)
        .await
        .unwrap();
    let marker = format!("did:key:z{}", uuid::Uuid::new_v4().simple());
    let passes = std::sync::atomic::AtomicUsize::new(0);
    let client = f.client(f.admin.as_ref());
    let applied = client
        .transact_write("test.owner_staged_route.unauthorized_replay", |txn| {
            let (passes, marker, change) = (&passes, &marker, f.change(&f.route));
            Box::pin(async move {
                passes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                txn.execute_client_signed_create(
                    "mutation($input: NodeMutationInputArg!) { create_Node(input: $input) { _docID } }",
                    &serde_json::json!({"input": {"node_did": marker}}),
                    gents_protocol::schemas::NODE,
                )
                .await?;
                txn.request_owner_staged(&change, false).await
            })
        })
        .await;
    assert!(
        matches!(
            applied,
            Err(ConfigClientError::OwnerStagedPlanReplayUnauthorized)
        ),
        "{applied:?}"
    );
    assert_eq!(passes.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(f.readable().await, vec![f.route.clone()]);
    f.node.shutdown().await.unwrap();
}

/// The route decides the signer's authority before it consumes the command's
/// nonce: an unauthorized but validly signed command is denied with the fixed
/// denial and makes the runtime write no nonce, while an authorized command's
/// nonce is still consumed once.
#[tokio::test]
async fn unauthorized_signed_command_writes_no_nonce() {
    let f = Fixture::start().await;
    let runtime = ConfigAccess::local_as(f.node.clone(), did(f.runtime.did()));
    let nonces = || async {
        let response = runtime
            .execute("{ EnrollmentOperatorNonce { _docID } }")
            .await
            .unwrap();
        gents_protocol::graphql::graphql_rows_from_response(&response, "EnrollmentOperatorNonce")
            .len()
    };
    let before = nonces().await;
    for doc in [&f.route, &f.missing, &f.foreign] {
        let command = f.command(f.outsider.as_ref(), doc, true).await;
        let (status, body) = f.post(&command, Some(f.outsider.as_ref())).await;
        assert_eq!(route_outcome(status, &body), "denied", "{body}");
    }
    assert_eq!(nonces().await, before);
    let command = f.command(f.admin.as_ref(), &f.route, true).await;
    let (status, body) = f.post(&command, Some(f.admin.as_ref())).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(nonces().await, before + 1);
    let (status, body) = f.post(&command, Some(f.admin.as_ref())).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "replayed nonce: {body}");
    f.node.shutdown().await.unwrap();
}
