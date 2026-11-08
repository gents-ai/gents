use super::*;
use serde_json::Value;

fn check_case(
    cases: &[Value],
    exercised: &mut BTreeSet<String>,
    name: &str,
    result: Result<FileInventorySnapshot>,
) -> Option<FileInventorySnapshot> {
    let case = cases.iter().find(|case| case["name"] == name).unwrap();
    exercised.insert(name.into());
    match case["expected"]["kind"].as_str().unwrap() {
        "rejected" => {
            let error = result.expect_err(name);
            assert_eq!(
                refusal_reason(&error),
                gents_protocol::file_inventory::FileInventoryRefusalReason::Rejected,
                "{name}: {error:#}"
            );
            None
        }
        "selected" => {
            let snapshot = result.unwrap_or_else(|error| panic!("{name}: {error:#}"));
            assert_eq!(
                serde_json::to_value(&snapshot.binding.coverage).unwrap(),
                case["coverage"],
                "{name}"
            );
            Some(snapshot)
        }
        other => panic!("unhandled reader model outcome {other}"),
    }
}

#[tokio::test]
async fn shared_reader_metadata_preserves_canonical_authorship_and_image_privacy() {
    let contracts: Value =
        tokio::task::spawn_blocking(gents_lean_contract::load_contract_snapshot::<Value>)
            .await
            .unwrap()
            .unwrap();
    let cases = contracts["file_inventory_reader_cases"].as_array().unwrap();
    assert!(!cases.is_empty());
    let mut exercised = BTreeSet::new();
    let (fixture, _request) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "inventory-independent-reader".into(),
        real_identity: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let node = fixture.node;
    let mut tool = fixture.tool;
    grant_session_reader(
        &fixture.client,
        &fixture.node_did,
        tool.session_id(),
        &fixture.node_did,
    )
    .await;
    tool.complete("source terminal").await.unwrap();
    let key_path = fixture.path.join("independent-reader.key");
    let reader =
        tokio::task::spawn_blocking(move || crate::KeyIdentity::load_or_create(key_path, None))
            .await
            .unwrap()
            .unwrap();
    let reader_did: identity::Did = reader.did().parse().unwrap();
    // A shared reader holds a document relation and the node's read permission;
    // native ACP observes both, with no Node fallback.
    ConfigAccess::local_as(node.clone(), fixture.node_did.parse().unwrap())
        .grant_node_permission(
            &reader_did,
            crate::store::config_client::NodePermission::DocumentRead,
        )
        .await
        .unwrap();
    grant_session_reader(
        &fixture.client,
        &fixture.node_did,
        tool.session_id(),
        reader.did(),
    )
    .await;
    let session_id = tool.session_id().to_owned();
    let session =
        ConfigAccess::transact_local_readonly(&node, None, "test.inventory.shared_root", |txn| {
            let (did, session_id, reader_did) = (&fixture.node_did, &session_id, &reader_did);
            Box::pin(async move {
                let session =
                    crate::store::config_client::agent_session::load_agent_session_owner_in_txn(
                        txn, did, session_id,
                    )
                    .await?
                    .unwrap();
                let observed = txn
                    .document_owners_as(
                        reader_did,
                        "AgentSession",
                        std::slice::from_ref(&session.doc_id),
                    )
                    .await?;
                assert!(observed[0].visible);
                // The requester owns its session; a shared reader observes
                // that canonical owner and holds no ownership itself.
                assert_eq!(
                    observed[0].owner.as_ref().map(|owner| owner.as_str()),
                    session.session.requester_did.as_deref()
                );
                assert_ne!(observed[0].owner.as_ref(), Some(reader_did));
                Ok::<_, anyhow::Error>(session)
            })
        })
        .await
        .unwrap();
    let mut admission = FileInventoryAdmission {
        session_doc_id: session.doc_id,
        request: HydrationRequest {
            request_key: "shared-reader-inventory".into(),
            peer_id: "independent-reader-peer".into(),
            requester_did: reader.did().into(),
            node_did: fixture.node_did.clone(),
            session_id,
        },
    };
    check_case(
        cases,
        &mut exercised,
        "shared_missing_capture_is_not_empty_complete",
        FileInventorySnapshot::load(&node, &admission).await,
    );
    retain_fork_fixture_capture(&node, &tool, "src/1.rs").await;
    let source = ConfigAccess::transact_local_readonly(
        &node,
        None,
        "test.inventory.shared_capture",
        |txn| {
            let tool = &tool;
            Box::pin(async move {
                let rows = txn
                    .cursor_rows(
                        "ToolFileInventory",
                        "_docID observation_doc_id",
                        &format!(
                            r#"tool_call_doc_id: {{_eq: "{}"}}"#,
                            escape_graphql_string(tool.doc_id().unwrap())
                        ),
                    )
                    .await?;
                assert_eq!(rows.len(), 1);
                Ok::<_, anyhow::Error>(rows[0].clone())
            })
        },
    )
    .await
    .unwrap();
    let projection = source["_docID"].as_str().unwrap();
    let image = source["observation_doc_id"].as_str().unwrap().to_owned();
    // The owning Agent toggles the reader on its projection. Its baseline
    // grant set adds relationships; revoking one needs the node's relation
    // delete permission, which the node admin grants for this toggle.
    let owning_agent: identity::Did = tool
        .access()
        .unwrap()
        .actor_did()
        .unwrap()
        .parse()
        .unwrap();
    ConfigAccess::local_as(node.clone(), fixture.node_did.parse().unwrap())
        .grant_node_permission(
            &owning_agent,
            crate::store::config_client::NodePermission::DacRelationDelete,
        )
        .await
        .unwrap();
    for step in 0..4 {
        let permitted = step % 2 == 1;
        if step > 0 {
            if permitted {
                tool.access()
                    .unwrap()
                    .add_actor_relationship("ToolFileInventory", projection, "reader", &reader_did)
                    .await
                    .unwrap();
            } else {
                tool.access()
                    .unwrap()
                    .delete_actor_relationship(
                        "ToolFileInventory",
                        projection,
                        "reader",
                        &reader_did,
                    )
                    .await
                    .unwrap();
            }
        }
        let name = if permitted {
            "shared_native_root_read"
        } else {
            "shared_unreadable_projection_owner_is_not_trusted"
        };
        let snapshot = check_case(
            cases,
            &mut exercised,
            name,
            FileInventorySnapshot::load(&node, &admission).await,
        );
        if let Some(snapshot) = snapshot {
            assert_eq!(snapshot.binding.admission, admission);
            for entry in snapshot.entries {
                assert_eq!(entry.scope.requester_did, tool.requester_did().unwrap());
                assert_ne!(entry.scope.requester_did, reader.did());
                assert_eq!(entry.tool_call_doc_id, tool.doc_id().unwrap());
                assert_eq!(entry.request_doc_id, tool.request_doc_id().unwrap());
                assert_eq!(entry.observation_doc_id, image);
            }
        }
        ConfigAccess::transact_local_readonly(
            &node,
            None,
            "test.inventory.no_shared_image_read",
            |txn| {
                let (reader, image) = (&reader_did, &image);
                Box::pin(async move {
                    let observed = txn
                        .document_owners_as(reader, "ToolFileChange", std::slice::from_ref(image))
                        .await?;
                    assert!(!observed[0].visible);
                    assert!(observed[0].owner.is_none());
                    Ok::<_, anyhow::Error>(())
                })
            },
        )
        .await
        .unwrap();
    }
    let real_session = std::mem::replace(
        &mut admission.session_doc_id,
        "another-physical-root".into(),
    );
    check_case(
        cases,
        &mut exercised,
        "shared_root_physical_identity_changed",
        FileInventorySnapshot::load(&node, &admission).await,
    );
    admission.session_doc_id = real_session;
    reserve_inventory_session(
        &fixture.client,
        &fixture.node_did,
        tool.actor().as_str(),
        "empty-shared-inventory",
        "2026-10-28T00:00:00Z",
    )
    .await;
    grant_session_reader(
        &fixture.client,
        &fixture.node_did,
        "empty-shared-inventory",
        reader.did(),
    )
    .await;
    admission.request.session_id = "empty-shared-inventory".into();
    admission.session_doc_id = ConfigAccess::transact_local_readonly(
        &node,
        None,
        "test.inventory.shared_empty_root",
        |txn| {
            let request = &admission.request;
            Box::pin(async move {
                Ok::<_, anyhow::Error>(
                    crate::store::config_client::agent_session::load_agent_session_owner_in_txn(
                        txn,
                        &request.node_did,
                        &request.session_id,
                    )
                    .await?
                    .unwrap()
                    .doc_id,
                )
            })
        },
    )
    .await
    .unwrap();
    check_case(
        cases,
        &mut exercised,
        "shared_authorized_empty_root",
        FileInventorySnapshot::load(&node, &admission).await,
    );
    assert_eq!(
        exercised,
        cases
            .iter()
            .map(|case| case["name"].as_str().unwrap().to_owned())
            .collect()
    );
    node.shutdown().await.expect("node shutdown");
    tokio::fs::remove_dir_all(fixture.path).await.unwrap();
}
