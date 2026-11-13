use std::collections::VecDeque;

use super::*;

#[derive(Clone, Copy)]
enum WritePlan {
    Success,
}

struct ControlledWriter {
    plans: std::sync::Mutex<VecDeque<WritePlan>>,
    attempts: mpsc::UnboundedSender<NodeReadinessProcessState>,
    persisted: tokio::sync::Mutex<Vec<NodeReadinessProcessState>>,
}

#[async_trait::async_trait]
impl NodeReadinessWriter for ControlledWriter {
    async fn upsert(
        &self,
        _node_did: &str,
        snapshot: &NodeReadinessSnapshot,
        _updated_at: &str,
    ) -> Result<()> {
        let plan = self
            .plans
            .lock()
            .expect("writer plans mutex")
            .pop_front()
            .expect("test supplied a write plan");
        self.attempts
            .send(snapshot.process_state)
            .expect("attempt observer remains alive");
        match plan {
            WritePlan::Success => {}
        }
        self.persisted.lock().await.push(snapshot.process_state);
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn generated_readiness_publication_traces_write_only_semantic_changes() {
    for case in gents_lean_vocab::families::contracts_readiness_publication::cases() {
        let (attempts_tx, mut attempts_rx) = mpsc::unbounded_channel();
        let writer = Arc::new(ControlledWriter {
            plans: std::sync::Mutex::new(VecDeque::from(vec![
                WritePlan::Success;
                case.input.states.len()
            ])),
            attempts: attempts_tx,
            persisted: tokio::sync::Mutex::new(Vec::new()),
        });
        let (owner, publisher) = NodeReadinessPublisherHandle::start_with_writer(
            writer,
            "did:test:semantic-readiness-writer",
            Duration::from_millis(1),
        );
        let mut publishes = Vec::with_capacity(case.input.states.len());
        for (index, &state) in case.input.states.iter().enumerate() {
            let (process, written) = match state {
                0 => (
                    ProcessLifecycleState::Recovering,
                    NodeReadinessProcessState::Recovering,
                ),
                1 => (
                    ProcessLifecycleState::Ready,
                    NodeReadinessProcessState::Ready,
                ),
                2 => (
                    ProcessLifecycleState::Shutdown,
                    NodeReadinessProcessState::Shutdown,
                ),
                other => panic!("unmodeled readiness state {other}"),
            };
            if index == 0 {
                assert_eq!(state, 0);
                publisher.initialize("general").await.unwrap();
            } else {
                publisher.set_process_state(process).await.unwrap();
            }
            let attempt = attempts_rx.try_recv().ok();
            if let Some(attempt) = attempt {
                assert_eq!(attempt, written);
            }
            publishes.push(attempt.is_some());
            tokio::time::advance(Duration::from_secs(3600)).await;
            tokio::task::yield_now().await;
            assert!(
                attempts_rx.try_recv().is_err(),
                "idle time must not publish readiness"
            );
        }
        case.check(
            gents_lean_vocab::families::contracts_readiness_publication::ReadinessPublicationOutput {
                publishes,
            },
        );
        owner.close().await.unwrap();
    }
}

async fn test_publisher() -> (
    gents_inference::test_support::TestDb,
    NodeReadinessPublisherOwner,
    NodeReadinessPublisherHandle,
) {
    let db = gents_inference::test_support::test_db(
        "node-readiness-publisher",
        gents_automation::lifecycle_hooks::composition(),
    )
    .await;
    let node = db.node.clone();
    let node_did = db.node_identity.did().to_owned();
    // Readiness is published under the Node document; production creates it
    // through the runtime owner's signed access before the publisher starts.
    gents_documents::test_support::ensure_test_node(&node, &node_did).await;
    let (owner, publisher) = NodeReadinessPublisherHandle::start(node, node_did);
    (db, owner, publisher)
}

#[tokio::test]
async fn replacement_ready_cannot_clear_demotion_until_source_generation_advances() {
    let (_db, owner, publisher) = test_publisher().await;
    publisher.initialize("general").await.unwrap();
    publisher.register_slot("general", 1).await.unwrap();
    publisher
        .send(|ack| Command::PublishSource {
            source: ReadinessSource {
                active_generation: 1,
                default_agent_did: "general".to_string(),
                entries: BTreeMap::from([(
                    "general".to_string(),
                    AgentReadinessSourceEntry {
                        agent_did: "general".to_string(),
                        dispatcher_present: true,
                        unavailable_reason: None,
                        startup_demoted: false,
                        effective_inference: None,
                    },
                )]),
                slot_generations: BTreeMap::new(),
            },
            ack,
        })
        .await
        .unwrap();
    assert!(publisher
        .demote_slot("general", 1, "generation one failed".to_string())
        .await
        .unwrap());

    publisher.register_slot("general", 2).await.unwrap();
    assert!(!publisher.mark_slot_ready("general", 0).await.unwrap());
    assert_eq!(
        publisher.observation().demotion_reason("general"),
        Some("generation one failed")
    );

    assert!(publisher.mark_slot_ready("general", 2).await.unwrap());
    assert_eq!(
        publisher.observation().demotion_reason("general"),
        Some("generation one failed"),
        "new slot readiness must not admit the still-published old source"
    );

    publisher
        .send(|ack| Command::PublishSource {
            source: ReadinessSource {
                active_generation: 2,
                default_agent_did: "general".to_string(),
                entries: BTreeMap::from([(
                    "general".to_string(),
                    AgentReadinessSourceEntry {
                        agent_did: "general".to_string(),
                        dispatcher_present: true,
                        unavailable_reason: None,
                        startup_demoted: false,
                        effective_inference: None,
                    },
                )]),
                slot_generations: BTreeMap::new(),
            },
            ack,
        })
        .await
        .unwrap();
    assert_eq!(publisher.observation().demotion_reason("general"), None);
    assert!(publisher.retire_slot("general", 1).await.unwrap());
    owner.close().await.unwrap();
}

#[tokio::test]
async fn invalid_source_leaves_committed_standing_and_observation_usable() {
    let (_db, owner, publisher) = test_publisher().await;
    publisher.initialize("general").await.unwrap();
    publisher.register_slot("general", 1).await.unwrap();
    publisher
        .send(|ack| Command::PublishSource {
            source: ReadinessSource {
                active_generation: 1,
                default_agent_did: "general".to_string(),
                entries: BTreeMap::from([(
                    "general".to_string(),
                    AgentReadinessSourceEntry {
                        agent_did: "general".to_string(),
                        dispatcher_present: true,
                        unavailable_reason: None,
                        startup_demoted: false,
                        effective_inference: None,
                    },
                )]),
                slot_generations: BTreeMap::new(),
            },
            ack,
        })
        .await
        .unwrap();
    assert!(publisher
        .demote_slot("general", 1, "standing demotion".to_string())
        .await
        .unwrap());
    let committed = publisher.observation();

    let invalid = publisher
        .send(|ack| Command::PublishSource {
            source: ReadinessSource {
                active_generation: 2,
                default_agent_did: "missing".to_string(),
                entries: BTreeMap::from([(
                    "general".to_string(),
                    AgentReadinessSourceEntry {
                        agent_did: "general".to_string(),
                        dispatcher_present: true,
                        unavailable_reason: None,
                        startup_demoted: false,
                        effective_inference: None,
                    },
                )]),
                slot_generations: BTreeMap::new(),
            },
            ack,
        })
        .await;
    assert!(invalid.is_err());
    assert_eq!(publisher.observation(), committed);
    assert!(
        publisher.mark_slot_ready("general", 1).await.unwrap(),
        "projection failure must not discard the committed slot standing"
    );
    assert_eq!(publisher.observation().demotion_reason("general"), None);

    publisher
        .send(|ack| Command::PublishSource {
            source: ReadinessSource {
                active_generation: 2,
                default_agent_did: "general".to_string(),
                entries: BTreeMap::from([(
                    "general".to_string(),
                    AgentReadinessSourceEntry {
                        agent_did: "general".to_string(),
                        dispatcher_present: true,
                        unavailable_reason: None,
                        startup_demoted: false,
                        effective_inference: None,
                    },
                )]),
                slot_generations: BTreeMap::new(),
            },
            ack,
        })
        .await
        .unwrap();
    assert_eq!(publisher.observation().source_generation(), 2);
    owner.close().await.unwrap();
}

#[tokio::test]
async fn unchanged_demoted_slot_stays_demoted_across_unrelated_global_generation() {
    let (_db, owner, publisher) = test_publisher().await;
    publisher.initialize("general").await.unwrap();
    publisher.register_slot("general", 1).await.unwrap();

    for active_generation in [1, 2] {
        publisher
            .send(|ack| Command::PublishSource {
                source: ReadinessSource {
                    active_generation,
                    default_agent_did: "general".to_string(),
                    entries: BTreeMap::from([(
                        "general".to_string(),
                        AgentReadinessSourceEntry {
                            agent_did: "general".to_string(),
                            dispatcher_present: true,
                            unavailable_reason: None,
                            startup_demoted: false,
                            effective_inference: None,
                        },
                    )]),
                    slot_generations: BTreeMap::new(),
                },
                ack,
            })
            .await
            .unwrap();
        if active_generation == 1 {
            assert!(publisher
                .demote_slot("general", 1, "generation one failed".to_string())
                .await
                .unwrap());
        }
    }

    let observation = publisher.observation();
    assert_eq!(observation.source_generation(), 2);
    assert_eq!(
        observation.demotion_reason("general"),
        Some("generation one failed"),
        "an unrelated source generation must not clear a reused slot's demotion"
    );
    owner.close().await.unwrap();
}

#[tokio::test]
async fn semantic_noop_and_stale_commands_do_not_emit_watch_revisions() {
    let (_db, owner, publisher) = test_publisher().await;
    publisher.initialize("general").await.unwrap();
    publisher.register_slot("general", 2).await.unwrap();
    let mut observation = publisher.observation.clone();
    observation.borrow_and_update();

    publisher.register_slot("general", 2).await.unwrap();
    assert!(!publisher.retire_slot("general", 1).await.unwrap());
    assert!(
        tokio::time::timeout(Duration::from_millis(50), observation.changed())
            .await
            .is_err()
    );
    owner.close().await.unwrap();
}

#[test]
fn generated_storage_failure_cases_bind_readiness_classification() {
    gents_lean_vocab::families::storage_failure_contracts_readiness::replay(|i| {
        let error = anyhow::Error::new(gents_protocol::database_error::GraphqlFailure::new(
            "test",
            serde_json::json!([{"message": i.message, "extensions": {"code": i.code}}]),
        ));
        gents_lean_vocab::families::storage_failure_contracts_readiness::ReadinessOutput {
            fatal_readiness: is_fatal_node_readiness_write(&error),
        }
    });
}

async fn verify_native_owner(
    access: &gents_store::config_client::ConfigAccess,
    collection: &'static str,
    id: &str,
    expected: Option<&str>,
) {
    let ids = vec![id.to_owned()];
    let owners = access
        .transact_readonly("test.readiness.owner_evidence", |txn| {
            let ids = &ids;
            Box::pin(async move { txn.document_owners(collection, ids).await })
        })
        .await
        .unwrap();
    assert_eq!(owners.len(), 1);
    assert!(
        owners[0].visible,
        "fixture row must exist and be visible in the transaction"
    );
    assert_eq!(
        owners[0].owner.as_ref().map(identity::Did::as_str),
        expected
    );
}

async fn remove_native_owner(node: &defra_node::Node, resource: &str, id: &str) {
    let acp = node.document_acp();
    let policy = gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID;
    assert!(acp.is_doc_registered(policy, resource, id).await.unwrap());
    acp.unregister_doc_object(policy, resource, id)
        .await
        .unwrap();
    assert!(!acp.is_doc_registered(policy, resource, id).await.unwrap());
}

struct RecoveryFixture {
    _dir: tempfile::TempDir,
    node: Arc<defra_node::Node>,
    runtime: gents_store::identity::KeyIdentity,
    foreign: gents_store::identity::KeyIdentity,
    user_access: gents_store::config_client::ConfigAccess,
    runtime_access: gents_store::config_client::ConfigAccess,
    doc: String,
    published: bool,
    parent_linked: bool,
}

impl RecoveryFixture {
    async fn user_reads_readiness(&self) -> bool {
        let visible = self
            .user_access
            .execute(&format!(
                r#"{{ NodeReadiness(filter: {{_docID: {{_eq: "{}"}}}}) {{_docID}} }}"#,
                escape_graphql_string(&self.doc)
            ))
            .await
            .unwrap();
        !visible["data"]["NodeReadiness"]
            .as_array()
            .unwrap()
            .is_empty()
    }
}

async fn recovery_fixture(name: &str, pending: bool) -> RecoveryFixture {
    use gents_protocol::node_readiness::NODE_READINESS_FORMAT_VERSION;
    use gents_store::config_client::ConfigAccess;
    use gents_store::identity::NodeIdentity;

    let dir = tempfile::tempdir().unwrap();
    let runtime =
        gents_store::identity::KeyIdentity::load_or_create(dir.path().join("runtime.key"), None)
            .unwrap();
    let user =
        gents_store::identity::KeyIdentity::load_or_create(dir.path().join("user.key"), None)
            .unwrap();
    let foreign =
        gents_store::identity::KeyIdentity::load_or_create(dir.path().join("foreign.key"), None)
            .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
    let node = Arc::new(
        defra_node::Node::builder()
            .enable_signing()
            .with_node_identity_did(runtime.did())
            .with_http(defra_node::HttpConfig::with_listener(listener).unwrap())
            .build()
            .await
            .unwrap(),
    );
    node.add_dac_policy_with_counter_as(
        &defra_core::RequestContext::default().with_identity(Some(runtime.did().to_owned())),
        gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
        0,
    )
    .await
    .unwrap();
    node.add_collection(gents_protocol::schemas::NODE)
        .await
        .unwrap();
    node.add_collection(gents_protocol::schemas::NODE_READINESS)
        .await
        .unwrap();
    node.add_collection(gents_protocol::schemas::NODE_RUNTIME)
        .await
        .unwrap();
    node.add_collection(gents_protocol::schemas::AGENT)
        .await
        .unwrap();
    let user_access = ConfigAccess::Graphql(
        gents_store::config_client::GraphqlEndpoint::as_principal(&endpoint, user.did())
            .with_client_signing_capability(user.client_signing_capability().unwrap())
            .unwrap(),
    );
    let runtime_access =
        ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did()).unwrap());
    let parent_write = format!(
        r#"mutation {{ create_Node(input: {{node_did: "{}"}}) {{_docID}} }}"#,
        escape_graphql_string(runtime.did())
    );
    let parent = match name {
        "parent-missing" | "node-unknown" => None,
        "unregistered-parent" => {
            let parent = ConfigAccess::write_local(
                &node,
                "test.readiness.unregistered_parent",
                &parent_write,
            )
            .await
            .unwrap();
            let parent = gents_protocol::graphql::extract_mutation_doc_id(&parent, "Node").unwrap();
            verify_native_owner(&runtime_access, "Node", &parent, Some(runtime.did())).await;
            remove_native_owner(&node, "nodes", &parent).await;
            verify_native_owner(&runtime_access, "Node", &parent, None).await;
            Some(parent)
        }
        _ => {
            let parent = user_access
                .write("test.readiness.parent", &parent_write)
                .await
                .unwrap();
            let parent = gents_protocol::graphql::extract_mutation_doc_id(&parent, "Node").unwrap();
            user_access
                .add_actor_relationship(
                    "Node",
                    &parent,
                    "runtime_reader",
                    &identity::Did::new(runtime.did()).unwrap(),
                )
                .await
                .unwrap();
            verify_native_owner(&runtime_access, "Node", &parent, Some(user.did())).await;
            Some(parent)
        }
    };
    let recorded_parent = match name {
        "parent-mismatch" => Some("another-parent".to_owned()),
        "node-unknown" => Some("unknown-node".to_owned()),
        _ => parent,
    };
    let node_doc_id = recorded_parent
        .map(|parent| format!(r#", node_doc_id: "{}""#, escape_graphql_string(&parent)))
        .unwrap_or_default();
    let linked = !pending;
    let mutation = format!(
        r#"mutation {{ create_NodeReadiness(input: {{node_did: "{}"{node_doc_id}, parent_linked: {linked}, snapshot_json: "{{}}", updated_at: "2026-01-01T00:00:00Z"}}) {{_docID}} }}"#,
        escape_graphql_string(runtime.did()),
    );
    let created = match name {
        "owner-unknown" => {
            ConfigAccess::write_local(&node, "test.readiness.unregistered", &mutation)
                .await
                .unwrap()
        }
        "owner-mismatch" => user_access
            .write("test.readiness.wrong_owner", &mutation)
            .await
            .unwrap(),
        _ => runtime_access
            .write("test.readiness.pending", &mutation)
            .await
            .unwrap(),
    };
    let doc = gents_protocol::graphql::extract_mutation_doc_id(&created, "NodeReadiness").unwrap();
    match name {
        "owner-unknown" => {
            verify_native_owner(&runtime_access, "NodeReadiness", &doc, Some(runtime.did())).await;
            remove_native_owner(&node, "operations", &doc).await;
            verify_native_owner(&runtime_access, "NodeReadiness", &doc, None).await;
        }
        "owner-mismatch" => {
            verify_native_owner(&user_access, "NodeReadiness", &doc, Some(user.did())).await;
        }
        _ => {
            verify_native_owner(&runtime_access, "NodeReadiness", &doc, Some(runtime.did())).await;
        }
    }
    let snapshot = NodeReadinessSnapshot {
        format_version: NODE_READINESS_FORMAT_VERSION,
        process_state: NodeReadinessProcessState::Ready,
        active_generation: 1,
        router_generation: 1,
        default_agent_did: "general".to_owned(),
        agents: vec![gents_protocol::node_readiness::AgentReadinessEntry {
            agent_did: "general".to_owned(),
            state: gents_protocol::node_readiness::AgentReadinessState::Ready,
            reason: None,
            effective_inference: None,
        }],
    };
    let selected = if name == "foreign-runtime" {
        foreign.did()
    } else {
        runtime.did()
    };
    let result = upsert_node_readiness(&node, selected, &snapshot, "2026-01-01T00:00:01Z").await;
    let observation_access = if name == "owner-mismatch" {
        &user_access
    } else {
        &runtime_access
    };
    let observed = observation_access
        .execute(&format!(
            r#"{{ NodeReadiness(filter: {{_docID: {{_eq: "{}"}}}}) {{parent_linked}} }}"#,
            escape_graphql_string(&doc)
        ))
        .await
        .unwrap();
    let parent_linked = observed["data"]["NodeReadiness"][0]["parent_linked"]
        .as_bool()
        .unwrap();
    RecoveryFixture {
        _dir: dir,
        node,
        runtime,
        foreign,
        user_access,
        runtime_access,
        doc,
        published: result.is_ok(),
        parent_linked,
    }
}

async fn assert_success_stays_private(fixture: &RecoveryFixture) {
    use gents_p2p::directory_projection::DirectoryStore;
    use gents_store::identity::NodeIdentity;
    let runtime = &fixture.runtime;
    fixture.runtime_access.write("test.readiness.runtime_status", &format!(
        r#"mutation {{ create_NodeRuntime(input: {{node_did: "{}", reconcile_phase: "idle", last_reconcile_result: "applied"}}) {{_docID}} }}"#,
        escape_graphql_string(runtime.did()),
    )).await.unwrap();
    fixture.user_access.write("test.readiness.private_agent", &format!(
        r#"mutation {{ create_Agent(input: {{node_did: "{}", agent_did: "private-agent"}}) {{_docID}} }}"#,
        escape_graphql_string(runtime.did()),
    )).await.unwrap();
    let goal = gents_automation::goal::observe_goal_agent(
        &fixture.node,
        runtime.did(),
        Some("general"),
        None,
    )
    .await
    .unwrap();
    assert!(matches!(
        goal.observation,
        gents_automation::goal::GoalAgentObservation::Ready { .. }
    ));
    assert!(
        !goal.settled,
        "persisted NodeRuntime must not supply live settled state"
    );
    assert!(gents_automation::goal::observe_goal_agent(
        &fixture.node,
        fixture.foreign.did(),
        Some("general"),
        None
    )
    .await
    .is_err());
    let directory =
        gents_p2p::directory_projection::GraphqlDirectoryStore::new(fixture.node.clone());
    let observed = directory.load_source_snapshot().await.unwrap();
    assert!(
        observed.runtimes.is_empty(),
        "readiness must not enter the shared directory"
    );
    assert!(
        observed.nodes.is_empty(),
        "private Node must not be projected through runtime authority"
    );
    assert!(
        observed.agents.is_empty(),
        "private Agent must not be projected through runtime authority"
    );
}

#[tokio::test]
async fn readiness_parent_recovery_matches_native_owner_and_model() {
    for case in gents_lean_vocab::families::identity_operations_readiness_recovery::cases() {
        let fixture = recovery_fixture(&case.input.name, case.input.pending).await;
        assert_eq!(fixture.published, fixture.parent_linked, "{:?}", case.input);
        if case.input.name == "success" {
            assert_success_stays_private(&fixture).await;
        }
        case.check(
            gents_lean_vocab::families::identity_operations_readiness_recovery::RecoveryOutput {
                parent_granted: fixture.user_reads_readiness().await,
                parent_linked: fixture.parent_linked,
            },
        );
        fixture.node.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn readiness_recovery_refuses_rows_the_runtime_does_not_own() {
    for case in
        gents_lean_vocab::families::identity_operations_readiness_recovery_foreign_owner::cases()
    {
        let fixture = recovery_fixture(&case.input.name, case.input.pending).await;
        assert_eq!(fixture.published, fixture.parent_linked, "{:?}", case.input);
        case.check(gents_lean_vocab::families::identity_operations_readiness_recovery_foreign_owner::ParentLinked {
            parent_linked: fixture.parent_linked,
        });
        fixture.node.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn published_readiness_observation_matches_model() {
    use gents_store::config_client::ConfigAccess;
    use gents_store::identity::NodeIdentity;
    for case in gents_lean_vocab::families::identity_operations_readiness_observation::cases() {
        let (fixture_name, pending, mark) = match case.input.name.as_str() {
            "valid" => ("success", true, false),
            "readiness-unregistered" => ("owner-unknown", true, true),
            "foreign-readiness" => ("owner-mismatch", true, true),
            "replaced-parent" => ("parent-mismatch", true, true),
            "node-unregistered" => ("unregistered-parent", true, true),
            "revoked-after-publication" => ("already-complete", false, false),
            "pending" => ("foreign-runtime", true, false),
            other => panic!("the recovery scenario has no fixture for {other}"),
        };
        let fixture = recovery_fixture(fixture_name, pending).await;
        if mark {
            let mark = format!(
                r#"mutation {{ update_NodeReadiness(docID: "{}", input: {{parent_linked: true}}) {{_docID}} }}"#,
                escape_graphql_string(&fixture.doc)
            );
            match fixture_name {
                "owner-unknown" => {
                    ConfigAccess::write_local(
                        &fixture.node,
                        "test.readiness.mark_unregistered",
                        &mark,
                    )
                    .await
                    .unwrap();
                    verify_native_owner(
                        &fixture.runtime_access,
                        "NodeReadiness",
                        &fixture.doc,
                        None,
                    )
                    .await;
                }
                "owner-mismatch" => {
                    fixture
                        .user_access
                        .write("test.readiness.mark_foreign", &mark)
                        .await
                        .unwrap();
                }
                _ => {
                    fixture
                        .runtime_access
                        .write("test.readiness.mark_mismatch", &mark)
                        .await
                        .unwrap();
                }
            }
        }
        let actor = identity::Did::new(fixture.runtime.did()).unwrap();
        let observed = fixture
            .runtime_access
            .transact_readonly("test.readiness.observe_owner", |txn| {
                let actor = &actor;
                Box::pin(async move {
                    gents_store::config_client::node_readiness::observe_published_readiness(
                        txn, actor,
                    )
                    .await
                })
            })
            .await
            .unwrap();
        case.check(
            gents_lean_vocab::families::identity_operations_readiness_observation::Usable {
                usable: observed.is_some(),
            },
        );
        fixture.node.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn readiness_binding_preflight_matches_model() {
    let policy_binding = format!(
        " @policy(id: \"{}\", resource: \"operations\")",
        gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID
    );
    for case in gents_lean_vocab::families::identity_operations_readiness_binding::cases::<String>()
    {
        let node = Arc::new(
            gents_store::test_support::node_builder()
                .build()
                .await
                .unwrap(),
        );
        match case.input.binding.as_str() {
            "absent" => {}
            "expected" => gents_store::schema::ensure_runtime_schemas(node.as_ref())
                .await
                .unwrap(),
            "unbound" => node
                .add_collection(
                    &gents_protocol::schemas::NODE_READINESS.replace(&policy_binding, ""),
                )
                .await
                .unwrap(),
            "different" => {
                node.add_dac_policy_with_counter_as(
                    &defra_core::RequestContext::default()
                        .with_identity(node.node_identity_did().map(str::to_owned)),
                    gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
                    0,
                )
                .await
                .unwrap();
                node.add_collection(&gents_protocol::schemas::NODE_READINESS.replace(
                    &policy_binding,
                    &policy_binding.replace("operations", "configuration"),
                ))
                .await
                .unwrap();
            }
            other => panic!("no readiness binding fixture for {other}"),
        }
        let result = gents_store::schema::ensure_runtime_schemas(node.as_ref()).await;
        if case.input.binding == "unbound" {
            assert!(node
                .get_collection("NodeReadiness")
                .unwrap()
                .unwrap()
                .policy
                .is_none());
        }
        case.check(result.is_ok());
        node.shutdown().await.unwrap();
    }
}
