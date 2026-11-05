use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex};

use axum::{
    extract::State,
    http::{HeaderMap, Method},
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};

use super::{ConfigAccess, ConfigApplyTxn, GraphqlEndpoint, TxnBackend};

#[tokio::test]
async fn modeled_native_entity_sets_preserve_selection_sources() -> anyhow::Result<()> {
    use crate::store::identity::NodeIdentity;
    let case = crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .expect("configuration model");
    let directory = tempfile::tempdir()?;
    let root = crate::KeyIdentity::load_or_create(directory.path().join("node"), None)?;
    let binding = crate::store::identity::provision_agent_identity(&root)?;
    let profile_owner = crate::store::identity::resolve_agent_identity(&root, &binding)?;
    let agent = crate::KeyIdentity::load_or_create(directory.path().join("agent"), None)?;
    let sibling = crate::KeyIdentity::load_or_create(directory.path().join("sibling"), None)?;
    let runtime_path = directory.path().join("runtime");
    let runtime =
        tokio::task::spawn_blocking(move || crate::KeyIdentity::load_or_create(runtime_path, None))
            .await??;
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(root.did())
            .with_node_acp_owner(root.did().to_owned())
            .build()
            .await?,
    );
    let context = defra_core::RequestContext::default().with_identity(Some(root.did().to_owned()));
    let policy = node
        .add_dac_policy_with_counter_as(&context, &case.policy_yaml, 0)
        .await?;
    for did in [
        profile_owner.did(),
        agent.did(),
        sibling.did(),
        runtime.did(),
    ] {
        for permission in [
            acp::nac::NodePermission::DocumentRead,
            acp::nac::NodePermission::DocumentUpdate,
            acp::nac::NodePermission::CollectionGet,
        ] {
            node.add_nac_actor_relationship_as(&context, permission.as_str(), did)
                .await?;
        }
    }
    for permission in [
        acp::nac::NodePermission::DocumentUpdate,
        acp::nac::NodePermission::DacRelationAdd,
        acp::nac::NodePermission::DacRelationDelete,
    ] {
        node.add_nac_actor_relationship_as(&context, permission.as_str(), profile_owner.did())
            .await?;
    }
    node.add_collection(&format!("type SetAgent @policy(id: \"{policy}\", resource: \"agents\") {{ label: String }}\ntype SetProfile @policy(id: \"{policy}\", resource: \"configuration\") {{ label: String }}\ntype SetBackendMetadata @policy(id: \"{policy}\", resource: \"configuration\") {{ label: String }}\ntype SetBackendObservation @policy(id: \"{policy}\", resource: \"configuration\") {{ label: String }}\ntype SetSampling @policy(id: \"{policy}\", resource: \"configuration\") {{ label: String }}\ntype SetExecution @policy(id: \"{policy}\", resource: \"configuration\") {{ label: String }}\ntype SetRetry @policy(id: \"{policy}\", resource: \"configuration\") {{ label: String }}")).await?;
    let owner = ConfigAccess::local_as(node.clone(), identity::Did::new(root.did())?);
    let profile_access = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(profile_owner.did())?,
        profile_owner.as_ref(),
    )?;
    let create =
        |name: &str| format!("mutation {{ create_{name}(input: {{label: \"test\"}}) {{_docID}} }}");
    let agent_doc = crate::store::graphql::created_doc_id(
        &owner
            .write("test.userset.agent", &create("SetAgent"))
            .await?,
        "SetAgent",
    )?;
    let profile_doc = crate::store::graphql::created_doc_id(
        &profile_access
            .write("test.userset.profile", &create("SetProfile"))
            .await?,
        "SetProfile",
    )?;
    let sampling_doc = crate::store::graphql::created_doc_id(
        &owner
            .write("test.userset.sampling", &create("SetSampling"))
            .await?,
        "SetSampling",
    )?;
    let backend_doc = crate::store::graphql::created_doc_id(
        &owner
            .write("test.userset.backend", &create("SetBackendMetadata"))
            .await?,
        "SetBackendMetadata",
    )?;
    let execution_doc = crate::store::graphql::created_doc_id(
        &owner
            .write("test.userset.execution", &create("SetExecution"))
            .await?,
        "SetExecution",
    )?;
    let retry_doc = crate::store::graphql::created_doc_id(
        &owner
            .write("test.userset.retry", &create("SetRetry"))
            .await?,
        "SetRetry",
    )?;
    owner
        .add_actor_relationship(
            "SetAgent",
            &agent_doc,
            "executor",
            &identity::Did::new(agent.did())?,
        )
        .await?;
    owner
        .transact_write("test.userset.retry_links", |txn| {
            let (execution_doc, retry_doc, profile_doc) =
                (&execution_doc, &retry_doc, &profile_doc);
            Box::pin(async move {
                txn.add_entity_set_relationship(
                    "SetExecution",
                    execution_doc,
                    "selected_by",
                    "configuration",
                    profile_doc,
                    "selected_by",
                )
                .await?;
                txn.add_entity_set_relationship(
                    "SetRetry",
                    retry_doc,
                    "selected_by",
                    "configuration",
                    execution_doc,
                    "selected_by",
                )
                .await
            })
        })
        .await?;
    profile_access
        .transact_write("test.userset.profile_link", |txn| {
            let (profile_doc, agent_doc) = (&profile_doc, &agent_doc);
            Box::pin(async move {
                txn.add_entity_set_relationship(
                    "SetProfile",
                    profile_doc,
                    "selected_by",
                    "agents",
                    agent_doc,
                    "executor",
                )
                .await
            })
        })
        .await?;
    owner
        .transact_write("test.userset.sampling_link", |txn| {
            let (sampling_doc, profile_doc) = (&sampling_doc, &profile_doc);
            Box::pin(async move {
                txn.add_entity_set_relationship(
                    "SetSampling",
                    sampling_doc,
                    "selected_by",
                    "configuration",
                    profile_doc,
                    "selected_by",
                )
                .await
            })
        })
        .await?;
    let agent_did = identity::Did::new(agent.did())?;
    owner
        .transact_write("test.userset.backend_link", |txn| {
            let (backend_doc, profile_doc) = (&backend_doc, &profile_doc);
            Box::pin(async move {
                txn.add_entity_set_relationship(
                    "SetBackendMetadata",
                    backend_doc,
                    "selected_by",
                    "configuration",
                    profile_doc,
                    "selected_by",
                )
                .await
            })
        })
        .await?;
    let observation_doc = crate::store::graphql::created_doc_id(
        &owner
            .write(
                "test.userset.backend_observation",
                &create("SetBackendObservation"),
            )
            .await?,
        "SetBackendObservation",
    )?;
    owner
        .add_actor_relationship(
            "SetBackendObservation",
            &observation_doc,
            "writer",
            &identity::Did::new(runtime.did())?,
        )
        .await?;
    owner
        .transact_write("test.userset.observation_link", |txn| {
            let (observation_doc, backend_doc) = (&observation_doc, &backend_doc);
            Box::pin(async move {
                txn.add_entity_set_relationship(
                    "SetBackendObservation",
                    observation_doc,
                    "selected_by",
                    "configuration",
                    backend_doc,
                    "selected_by",
                )
                .await
            })
        })
        .await?;
    for item in &case.backend_observation_authority_cases {
        let actor = match item["actor"].as_str().unwrap() {
            "runtime" => runtime.did(),
            "backend-owner" => root.did(),
            "agent" => agent.did(),
            "sibling" => sibling.did(),
            other => anyhow::bail!("unknown modeled actor {other}"),
        };
        let (collection, doc) = match item["object"].as_str().unwrap() {
            "backend-metadata" => ("SetBackendMetadata", &backend_doc),
            "backend-observation" => ("SetBackendObservation", &observation_doc),
            other => anyhow::bail!("unknown modeled object {other}"),
        };
        let permission = item["permission"].as_str().unwrap();
        let observed = owner
            .transact_readonly("test.observation.authority", |txn| {
                Box::pin(async move {
                    txn.observe_document_access(
                        &identity::Did::new(actor)?,
                        collection,
                        std::slice::from_ref(doc),
                        defra_node::DocumentAccessMode::Named {
                            permission: permission.into(),
                            node_permission: if permission == "read" {
                                acp::nac::NodePermission::DocumentRead
                            } else {
                                acp::nac::NodePermission::DocumentUpdate
                            },
                        },
                    )
                    .await
                    .map_err(anyhow::Error::from)
                })
            })
            .await?;
        assert_eq!(
            matches!(
                observed.as_slice(),
                [defra_node::DocumentAccessDecision::Allowed]
            ),
            item["allowed"].as_bool().unwrap()
        );
    }
    for item in &case.configuration_backend_metadata_cases {
        let direct = item["direct"].as_bool().unwrap();
        let revoked = item["revoked"].as_bool().unwrap();
        if direct {
            owner
                .add_actor_relationship("SetBackendMetadata", &backend_doc, "reader", &agent_did)
                .await?;
        }
        if revoked {
            owner
                .transact_write("test.userset.backend_revoke", |txn| {
                    let (backend_doc, profile_doc) = (&backend_doc, &profile_doc);
                    Box::pin(async move {
                        txn.delete_entity_set_relationship(
                            "SetBackendMetadata",
                            backend_doc,
                            "selected_by",
                            "configuration",
                            profile_doc,
                            "selected_by",
                        )
                        .await
                    })
                })
                .await?;
        }
        let actor = match item["actor"].as_str().unwrap() {
            "agent" => agent.did(),
            "sibling" => sibling.did(),
            other => anyhow::bail!("unknown modeled actor {other}"),
        };
        let response = ConfigAccess::local_as(node.clone(), identity::Did::new(actor)?)
            .execute("{ SetBackendMetadata { _docID } }")
            .await?;
        let visible = response["data"]["SetBackendMetadata"]
            .as_array()
            .is_some_and(|rows| {
                rows.iter()
                    .any(|row| row["_docID"].as_str() == Some(backend_doc.as_str()))
            });
        assert_eq!(
            visible,
            item["read"].as_bool().unwrap(),
            "modeled {}",
            item["name"]
        );
        let observed = owner
            .transact_readonly("test.userset.backend_update", |txn| {
                let backend_doc = &backend_doc;
                Box::pin(async move {
                    txn.observe_document_access(
                        &identity::Did::new(actor)?,
                        "SetBackendMetadata",
                        std::slice::from_ref(backend_doc),
                        defra_node::DocumentAccessMode::Named {
                            permission: "update".into(),
                            node_permission: acp::nac::NodePermission::DocumentUpdate,
                        },
                    )
                    .await
                    .map_err(anyhow::Error::from)
                })
            })
            .await?;
        let expected = if item["update"].as_bool().unwrap() {
            defra_node::DocumentAccessDecision::Allowed
        } else {
            defra_node::DocumentAccessDecision::Denied
        };
        assert_eq!(
            observed.as_slice(),
            [expected],
            "modeled {} update",
            item["name"]
        );
        if revoked {
            owner
                .transact_write("test.userset.backend_restore", |txn| {
                    let (backend_doc, profile_doc) = (&backend_doc, &profile_doc);
                    Box::pin(async move {
                        txn.add_entity_set_relationship(
                            "SetBackendMetadata",
                            backend_doc,
                            "selected_by",
                            "configuration",
                            profile_doc,
                            "selected_by",
                        )
                        .await
                    })
                })
                .await?;
        }
        if direct {
            owner
                .delete_actor_relationship("SetBackendMetadata", &backend_doc, "reader", &agent_did)
                .await?;
        }
    }
    let aborted: anyhow::Result<()> = owner
        .transact_write("test.userset.abort", |txn| {
            let (sampling_doc, profile_doc) = (&sampling_doc, &profile_doc);
            Box::pin(async move {
                txn.delete_entity_set_relationship(
                    "SetSampling",
                    sampling_doc,
                    "selected_by",
                    "configuration",
                    profile_doc,
                    "selected_by",
                )
                .await?;
                anyhow::bail!("abort after staged removal")
            })
        })
        .await;
    assert!(aborted.is_err());
    let after_abort = ConfigAccess::local_as(node.clone(), agent_did.clone())
        .execute("{ SetSampling { _docID } }")
        .await?;
    assert!(after_abort["data"]["SetSampling"]
        .as_array()
        .is_some_and(|rows| rows.iter().any(|row| row["_docID"] == sampling_doc)));
    for item in &case.configuration_userset_cases {
        let direct = item["direct"].as_bool().unwrap();
        let revoked = item["revoked"].as_str().unwrap();
        if direct {
            owner
                .add_actor_relationship("SetSampling", &sampling_doc, "reader", &agent_did)
                .await?;
        }
        match revoked {
            "profile" => {
                profile_access
                    .transact_write("test.userset.remove_profile", |txn| {
                        let (profile_doc, agent_doc) = (&profile_doc, &agent_doc);
                        Box::pin(async move {
                            txn.delete_entity_set_relationship(
                                "SetProfile",
                                profile_doc,
                                "selected_by",
                                "agents",
                                agent_doc,
                                "executor",
                            )
                            .await
                        })
                    })
                    .await?
            }
            "sampling" => {
                owner
                    .transact_write("test.userset.remove_sampling", |txn| {
                        let (sampling_doc, profile_doc) = (&sampling_doc, &profile_doc);
                        Box::pin(async move {
                            txn.delete_entity_set_relationship(
                                "SetSampling",
                                sampling_doc,
                                "selected_by",
                                "configuration",
                                profile_doc,
                                "selected_by",
                            )
                            .await
                        })
                    })
                    .await?
            }
            "executor" => {
                owner
                    .delete_actor_relationship("SetAgent", &agent_doc, "executor", &agent_did)
                    .await?;
            }
            "execution" => {
                owner
                    .transact_write("test.userset.remove_execution", |txn| {
                        let (execution_doc, profile_doc) = (&execution_doc, &profile_doc);
                        Box::pin(async move {
                            txn.delete_entity_set_relationship(
                                "SetExecution",
                                execution_doc,
                                "selected_by",
                                "configuration",
                                profile_doc,
                                "selected_by",
                            )
                            .await
                        })
                    })
                    .await?
            }
            _ => {}
        }
        let actor = match item["actor"].as_str().unwrap() {
            "agent" => agent.did(),
            "sibling" => sibling.did(),
            "profile-owner" => profile_owner.did(),
            other => anyhow::bail!("unknown modeled actor {other}"),
        };
        let (name, doc) = if item["document"] == "retry" {
            ("SetRetry", &retry_doc)
        } else {
            ("SetSampling", &sampling_doc)
        };
        let response = ConfigAccess::local_as(node.clone(), identity::Did::new(actor)?)
            .execute(&format!("{{ {name} {{ _docID }} }}"))
            .await?;
        let visible = response["data"][name].as_array().is_some_and(|rows| {
            rows.iter()
                .any(|row| row["_docID"].as_str() == Some(doc.as_str()))
        });
        assert_eq!(
            visible,
            item["expected"].as_bool().unwrap(),
            "modeled {}",
            item["name"]
        );
        match revoked {
            "profile" => {
                profile_access
                    .transact_write("test.userset.restore_profile", |txn| {
                        let (profile_doc, agent_doc) = (&profile_doc, &agent_doc);
                        Box::pin(async move {
                            txn.add_entity_set_relationship(
                                "SetProfile",
                                profile_doc,
                                "selected_by",
                                "agents",
                                agent_doc,
                                "executor",
                            )
                            .await
                        })
                    })
                    .await?
            }
            "sampling" => {
                owner
                    .transact_write("test.userset.restore_sampling", |txn| {
                        let (sampling_doc, profile_doc) = (&sampling_doc, &profile_doc);
                        Box::pin(async move {
                            txn.add_entity_set_relationship(
                                "SetSampling",
                                sampling_doc,
                                "selected_by",
                                "configuration",
                                profile_doc,
                                "selected_by",
                            )
                            .await
                        })
                    })
                    .await?
            }
            "executor" => {
                owner
                    .add_actor_relationship("SetAgent", &agent_doc, "executor", &agent_did)
                    .await?;
            }
            "execution" => {
                owner
                    .transact_write("test.userset.restore_execution", |txn| {
                        let (execution_doc, profile_doc) = (&execution_doc, &profile_doc);
                        Box::pin(async move {
                            txn.add_entity_set_relationship(
                                "SetExecution",
                                execution_doc,
                                "selected_by",
                                "configuration",
                                profile_doc,
                                "selected_by",
                            )
                            .await
                        })
                    })
                    .await?
            }
            _ => {}
        }
        if direct {
            owner
                .delete_actor_relationship("SetSampling", &sampling_doc, "reader", &agent_did)
                .await?;
        }
    }
    let delegate = identity::Did::new(profile_owner.did())?;
    owner
        .add_actor_relationship("SetSampling", &sampling_doc, "reader", &delegate)
        .await?;
    for item in &case.configuration_selection_manager_cases {
        if item["grant"] == true {
            owner
                .add_actor_relationship(
                    "SetSampling",
                    &sampling_doc,
                    "selection_manager",
                    &delegate,
                )
                .await?;
        }
        if item["revoke"] == true {
            owner
                .delete_actor_relationship(
                    "SetSampling",
                    &sampling_doc,
                    "selection_manager",
                    &delegate,
                )
                .await?;
        }
        let result = profile_access
            .transact_write("test.userset.delegate", |txn| {
                let (sampling_doc, profile_doc) = (&sampling_doc, &profile_doc);
                Box::pin(async move {
                    txn.add_entity_set_relationship(
                        "SetSampling",
                        sampling_doc,
                        "selected_by",
                        "configuration",
                        profile_doc,
                        "selected_by",
                    )
                    .await
                })
            })
            .await;
        assert_eq!(
            result.is_ok(),
            item["selection"].as_bool().unwrap(),
            "modeled {}",
            item["name"]
        );
        let reader = profile_access
            .transact_write("test.userset.delegate_reader", |txn| {
                let (sampling_doc, delegate) = (&sampling_doc, &delegate);
                Box::pin(async move {
                    txn.add_actor_relationship("SetSampling", sampling_doc, "reader", delegate)
                        .await
                })
            })
            .await;
        assert_eq!(
            reader.is_ok(),
            item["reader"].as_bool().unwrap(),
            "modeled reader {}",
            item["name"]
        );
        let update = profile_access.write("test.userset.delegate_update", &format!(
            "mutation {{ update_SetSampling(docID: \"{}\", input: {{label:\"changed\"}}) {{_docID}} }}",
            crate::store::graphql::escape_graphql_string(&sampling_doc))).await?;
        let changed = update["data"]["update_SetSampling"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty());
        assert_eq!(
            changed,
            item["update"].as_bool().unwrap(),
            "modeled update {}",
            item["name"]
        );
    }
    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn http_relationships_and_ownership_use_the_transaction_actor() {
    type Requests = Arc<Mutex<Vec<(Method, HeaderMap, Value)>>>;
    async fn relationship(
        State(requests): State<Requests>,
        method: Method,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        requests.lock().unwrap().push((method, headers, body));
        Json(json!({}))
    }
    async fn ownership(
        State(requests): State<Requests>,
        method: Method,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let malformed = body["doc_ids"] == json!(["malformed"]);
        requests.lock().unwrap().push((method, headers, body));
        if malformed {
            return Json(json!({"documents": [
                {"doc_id": "malformed", "visible": false, "is_owner": true}
            ]}));
        }
        Json(json!({"documents": [
            {"doc_id": "owned", "visible": true, "is_owner": true, "owner_registered": true},
            {"doc_id": "other", "visible": true, "is_owner": false, "owner_registered": true},
            {"doc_id": "hidden", "visible": false, "is_owner": false, "owner_registered": false}
        ]}))
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
    let requests: Requests = Arc::default();
    let app = Router::new()
        .route(
            "/api/v0/acp/document/relationship",
            post(relationship).delete(relationship),
        )
        .route("/api/v0/acp/document/is-owner", post(ownership))
        .with_state(requests.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let actor = identity::Did::new("did:key:actor").unwrap();
    let other = identity::Did::new("did:key:other").unwrap();
    let endpoint =
        GraphqlEndpoint::with_authenticated_delegation(url, "Bearer signed-token", actor.clone())
            .unwrap();
    let txn = ConfigApplyTxn {
        backend: TxnBackend::Http {
            endpoint: &endpoint,
            id: "42".into(),
            client: reqwest::Client::new(),
        },
        rollback_on_drop: None,
        affected_documents: AtomicU64::new(0),
        operation: None,
        contends_with_request_siblings: AtomicBool::new(false),
        client_signed_write_dispatched: AtomicBool::new(false),
        relationship_grants: Mutex::new(Vec::new()),
        publication_observer: None,
    };
    txn.add_resource_relationship("Agent", "owned", "parent", "nodes", "node")
        .await
        .unwrap();
    txn.add_actor_relationship("Agent", "owned", "reader", &other)
        .await
        .unwrap();
    txn.delete_actor_relationship("Agent", "owned", "reader", &other)
        .await
        .unwrap();
    txn.add_entity_set_relationship(
        "Agent",
        "owned",
        "selected_by",
        "agents",
        "agent:1",
        "executor",
    )
    .await
    .unwrap();
    txn.delete_entity_set_relationship(
        "Agent",
        "owned",
        "selected_by",
        "agents",
        "agent:1",
        "executor",
    )
    .await
    .unwrap();
    txn.delete_resource_relationship("Agent", "owned", "parent", "nodes", "node")
        .await
        .unwrap();
    let ids = vec!["owned".into(), "other".into(), "hidden".into()];
    let owners = txn.document_owners("Agent", &ids).await.unwrap();
    assert_eq!(owners[0].owner.as_ref(), Some(&actor));
    assert!(owners[0].visible);
    assert!(owners[1].visible && owners[1].owner.is_none());
    assert!(!owners[2].visible && owners[2].owner.is_none());
    let registrations = txn
        .document_owner_registration("Agent", &ids)
        .await
        .unwrap();
    assert!(registrations[0].registered);
    assert!(registrations[1].visible && registrations[1].registered);
    assert!(!registrations[2].visible && !registrations[2].registered);
    assert!(txn.document_owners_as(&other, "Agent", &ids).await.is_err());
    assert!(matches!(
        txn.document_owners("Agent", &["malformed".into()]).await,
        Err(super::ConfigClientError::InvalidOwnerObservation)
    ));
    let entries = requests.lock().unwrap();
    assert_eq!(entries.len(), 9);
    assert_eq!(entries[0].2["target_actor"], "nodes:\"node\"");
    assert_eq!(entries[1].2["target_actor"], other.as_str());
    assert_eq!(entries[2].0, Method::DELETE);
    assert_eq!(entries[3].0, Method::POST);
    assert_eq!(entries[3].2["target_actor"], "agents:\"agent:1\"#executor");
    assert_eq!(entries[4].0, Method::DELETE);
    assert_eq!(entries[4].2["target_actor"], "agents:\"agent:1\"#executor");
    assert_eq!(entries[5].0, Method::DELETE);
    assert_eq!(entries[5].2["target_actor"], "nodes:\"node\"");
    assert_eq!(entries[6].2, json!({"collection":"Agent", "doc_ids": ids}));
    for (_, headers, _) in entries.iter() {
        assert_eq!(headers["authorization"], "Bearer signed-token");
        assert_eq!(headers["x-defradb-tx"], "42");
    }
    server.abort();
}

#[tokio::test]
async fn embedded_relationship_grants_and_revocations_follow_transaction_finality() {
    const OWNER: &str = "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK";
    const READER: &str = "did:key:z6MkfXG2FkNy3u7Eg3jm8e2YQpGz7Z1JqWgHDAP1hLk9r2bR";
    let node = Arc::new(defra_node::Node::builder().build().await.unwrap());
    let policy = node.add_dac_policy_as(
        &defra_core::RequestContext::default().with_identity(Some(OWNER.to_owned())),
        "name: TxnRelationships\nresources:\n  - name: rows\n    relations:\n      - name: reader\n    permissions:\n      - name: read\n        expr: reader\n      - name: update\n      - name: delete\n",
    ).await.unwrap();
    node.add_collection(&format!(
        "type TxnRelationshipRow @policy(id: \"{policy}\", resource: \"rows\") {{ value: String }}"
    ))
    .await
    .unwrap();
    let owner = identity::Did::new(OWNER).unwrap();
    let reader = identity::Did::new(READER).unwrap();
    let created = ConfigAccess::local_as(node.clone(), owner.clone())
        .write(
            "test.txn_relationship_create",
            "mutation { add_TxnRelationshipRow(input: {value: \"secret\"}) { _docID } }",
        )
        .await
        .unwrap();
    let doc = created["data"]["add_TxnRelationshipRow"][0]["_docID"]
        .as_str()
        .unwrap();
    let reader_access = ConfigAccess::local_as(node.clone(), reader.clone());
    for commit in [false, true] {
        let txn = ConfigApplyTxn::begin_local(&node, Some(owner.clone()))
            .await
            .unwrap();
        txn.add_actor_relationship("TxnRelationshipRow", doc, "reader", &reader)
            .await
            .unwrap();
        let visible = reader_access
            .execute("{ TxnRelationshipRow { _docID } }")
            .await
            .unwrap();
        assert!(visible["data"]["TxnRelationshipRow"]
            .as_array()
            .unwrap()
            .is_empty());
        if commit {
            txn.commit().await.unwrap();
        } else {
            txn.discard().await.unwrap();
        }
    }
    let visible = reader_access
        .execute("{ TxnRelationshipRow { _docID } }")
        .await
        .unwrap();
    assert_eq!(
        visible["data"]["TxnRelationshipRow"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let txn = ConfigApplyTxn::begin_local(&node, Some(owner))
        .await
        .unwrap();
    txn.delete_actor_relationship("TxnRelationshipRow", doc, "reader", &reader)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let visible = reader_access
        .execute("{ TxnRelationshipRow { _docID } }")
        .await
        .unwrap();
    assert!(visible["data"]["TxnRelationshipRow"]
        .as_array()
        .unwrap()
        .is_empty());
    node.shutdown().await.unwrap();
}
