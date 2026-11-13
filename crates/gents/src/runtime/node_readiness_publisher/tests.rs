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
    let cases = gents_lean_vocab::lean_readiness_publication_cases();
    assert_eq!(cases.len(), 2);
    for case in cases {
        let (attempts_tx, mut attempts_rx) = mpsc::unbounded_channel();
        let writer = Arc::new(ControlledWriter {
            plans: std::sync::Mutex::new(VecDeque::from(vec![
                WritePlan::Success;
                case.states.len()
            ])),
            attempts: attempts_tx,
            persisted: tokio::sync::Mutex::new(Vec::new()),
        });
        let (owner, publisher) = NodeReadinessPublisherHandle::start_with_writer(
            writer,
            "did:test:semantic-readiness-writer",
            Duration::from_millis(1),
        );
        assert_eq!(case.states.len(), case.publishes.len());
        for (index, (&state, &publishes)) in case.states.iter().zip(&case.publishes).enumerate() {
            let (process, expected) = match state {
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
            assert_eq!(attempts_rx.try_recv().ok(), publishes.then_some(expected));
            tokio::time::advance(Duration::from_secs(3600)).await;
            tokio::task::yield_now().await;
            assert!(
                attempts_rx.try_recv().is_err(),
                "idle time must not publish readiness"
            );
        }
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

#[tokio::test]
async fn readiness_parent_recovery_matches_native_owner_and_model() {
    use gents_protocol::node_readiness::NODE_READINESS_FORMAT_VERSION;
    use gents_store::config_client::ConfigAccess;
    use gents_store::identity::NodeIdentity;

    async fn verify_native_owner(
        access: &ConfigAccess,
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

    let cases = &gents_lean_vocab::lean_contract_snapshot().identity_readiness_recovery_cases;
    let mut checked = 0;
    for case in cases.iter().filter(|case| {
        matches!(
            case["name"].as_str(),
            Some(
                "success"
                    | "owner-unknown"
                    | "owner-mismatch"
                    | "parent-mismatch"
                    | "foreign-runtime"
                    | "unregistered-parent"
                    | "already-complete"
            )
        )
    }) {
        checked += 1;
        let dir = tempfile::tempdir().unwrap();
        let runtime = gents_store::identity::KeyIdentity::load_or_create(
            dir.path().join("runtime.key"),
            None,
        )
        .unwrap();
        let user =
            gents_store::identity::KeyIdentity::load_or_create(dir.path().join("user.key"), None)
                .unwrap();
        let foreign = gents_store::identity::KeyIdentity::load_or_create(
            dir.path().join("foreign.key"),
            None,
        )
        .unwrap();
        // The User configures the Node through client-signed access; a local
        // non-node write without a managed signer is refused by design.
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
        let parent = if case["name"] == "unregistered-parent" {
            ConfigAccess::write_local(&node, "test.readiness.unregistered_parent", &parent_write)
                .await
                .unwrap()
        } else {
            user_access
                .write("test.readiness.parent", &parent_write)
                .await
                .unwrap()
        };
        let parent = gents_protocol::graphql::extract_mutation_doc_id(&parent, "Node").unwrap();
        if case["name"] != "unregistered-parent" {
            user_access
                .add_actor_relationship(
                    "Node",
                    &parent,
                    "runtime_reader",
                    &identity::Did::new(runtime.did()).unwrap(),
                )
                .await
                .unwrap();
        }
        if case["name"] == "unregistered-parent" {
            verify_native_owner(&runtime_access, "Node", &parent, Some(runtime.did())).await;
            remove_native_owner(&node, "nodes", &parent).await;
            verify_native_owner(&runtime_access, "Node", &parent, None).await;
        } else {
            verify_native_owner(&runtime_access, "Node", &parent, Some(user.did())).await;
        }
        let recorded_parent = if case["name"] == "parent-mismatch" {
            "another-parent"
        } else {
            parent.as_str()
        };
        let linked = !case["pending"].as_bool().unwrap();
        let mutation = format!(
            r#"mutation {{ create_NodeReadiness(input: {{node_did: "{}", node_doc_id: "{}", parent_linked: {linked}, snapshot_json: "{{}}", updated_at: "2026-01-01T00:00:00Z"}}) {{_docID}} }}"#,
            escape_graphql_string(runtime.did()),
            escape_graphql_string(recorded_parent)
        );
        let created = match case["name"].as_str().unwrap() {
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
        let doc =
            gents_protocol::graphql::extract_mutation_doc_id(&created, "NodeReadiness").unwrap();
        if case["name"] == "owner-unknown" {
            verify_native_owner(&runtime_access, "NodeReadiness", &doc, Some(runtime.did())).await;
            remove_native_owner(&node, "operations", &doc).await;
            verify_native_owner(&runtime_access, "NodeReadiness", &doc, None).await;
        } else if case["name"] == "owner-mismatch" {
            verify_native_owner(&user_access, "NodeReadiness", &doc, Some(user.did())).await;
        } else {
            verify_native_owner(&runtime_access, "NodeReadiness", &doc, Some(runtime.did())).await;
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
            }],
        };
        let selected = if case["name"] == "foreign-runtime" {
            foreign.did()
        } else {
            runtime.did()
        };
        let result =
            upsert_node_readiness(&node, selected, &snapshot, "2026-01-01T00:00:01Z").await;
        let expected_ok = matches!(case["name"].as_str(), Some("success" | "already-complete"));
        assert_eq!(result.is_ok(), expected_ok, "{case}: {result:?}");
        let observation_access = if case["name"] == "owner-mismatch" {
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
        assert_eq!(
            observed["data"]["NodeReadiness"][0]["parent_linked"], case["expected_parent_linked"],
            "{case}"
        );
        let observation_name = match case["name"].as_str().unwrap() {
            "success" => Some("valid"),
            "owner-unknown" => Some("readiness-unregistered"),
            "owner-mismatch" => Some("foreign-readiness"),
            "parent-mismatch" => Some("replaced-parent"),
            "unregistered-parent" => Some("node-unregistered"),
            "already-complete" => Some("revoked-after-publication"),
            _ => None,
        };
        if let Some(observation_name) = observation_name {
            if !matches!(case["name"].as_str(), Some("success" | "already-complete")) {
                let mark = format!(
                    r#"mutation {{ update_NodeReadiness(docID: "{}", input: {{parent_linked: true}}) {{_docID}} }}"#,
                    escape_graphql_string(&doc)
                );
                match case["name"].as_str().unwrap() {
                    "owner-unknown" => {
                        ConfigAccess::write_local(&node, "test.readiness.mark_unregistered", &mark)
                            .await
                            .unwrap();
                    }
                    "owner-mismatch" => {
                        user_access
                            .write("test.readiness.mark_foreign", &mark)
                            .await
                            .unwrap();
                    }
                    _ => {
                        runtime_access
                            .write("test.readiness.mark_mismatch", &mark)
                            .await
                            .unwrap();
                    }
                }
            }
            if case["name"] == "owner-unknown" {
                verify_native_owner(&runtime_access, "NodeReadiness", &doc, None).await;
            }
            let model = gents_lean_vocab::lean_contract_snapshot()
                .identity_readiness_observation_cases
                .iter()
                .find(|model| model["name"] == observation_name)
                .unwrap();
            let actor = identity::Did::new(runtime.did()).unwrap();
            let actual = runtime_access
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
            assert_eq!(
                actual.is_some(),
                model["expected_usable"].as_bool().unwrap(),
                "{model}"
            );
        }
        if case["name"] == "success" {
            use gents_p2p::directory_projection::DirectoryStore;
            runtime_access.write("test.readiness.runtime_status", &format!(
                r#"mutation {{ create_NodeRuntime(input: {{node_did: "{}", reconcile_phase: "idle", last_reconcile_result: "applied"}}) {{_docID}} }}"#,
                escape_graphql_string(runtime.did()),
            )).await.unwrap();
            user_access.write("test.readiness.private_agent", &format!(
                r#"mutation {{ create_Agent(input: {{node_did: "{}", agent_did: "private-agent"}}) {{_docID}} }}"#,
                escape_graphql_string(runtime.did()),
            )).await.unwrap();
            let goal = gents_automation::goal::observe_goal_agent(
                &node,
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
                &node,
                foreign.did(),
                Some("general"),
                None
            )
            .await
            .is_err());
            let directory =
                gents_p2p::directory_projection::GraphqlDirectoryStore::new(node.clone());
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
        if case["name"] == "success" || case["name"] == "already-complete" {
            let visible = user_access
                .execute(&format!(
                    r#"{{ NodeReadiness(filter: {{_docID: {{_eq: "{}"}}}}) {{_docID}} }}"#,
                    escape_graphql_string(&doc)
                ))
                .await
                .unwrap();
            assert_eq!(
                !visible["data"]["NodeReadiness"]
                    .as_array()
                    .unwrap()
                    .is_empty(),
                case["expected_parent_granted"].as_bool().unwrap(),
                "{case}"
            );
        }
        node.shutdown().await.unwrap();
    }
    assert_eq!(checked, 7);

    let unbound = gents_lean_vocab::lean_contract_snapshot()
        .identity_readiness_preflight_cases
        .iter()
        .find(|case| case["name"] == "unbound")
        .unwrap();
    let node = Arc::new(
        gents_store::test_support::node_builder()
            .build()
            .await
            .unwrap(),
    );
    let unbound_sdl = gents_protocol::schemas::NODE_READINESS.replace(
        &format!(
            " @policy(id: \"{}\", resource: \"operations\")",
            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID
        ),
        "",
    );
    node.add_collection(&unbound_sdl).await.unwrap();
    let result = gents_store::schema::ensure_runtime_schemas(node.as_ref()).await;
    assert_eq!(result.is_ok(), unbound["expected"].as_bool().unwrap());
    assert!(node
        .get_collection("NodeReadiness")
        .unwrap()
        .unwrap()
        .policy
        .is_none());
    node.shutdown().await.unwrap();
}
