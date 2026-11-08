use super::*;
use crate::store::identity::NodeIdentity;
use crate::tools::call::lifecycle::admission_fixture::{
    published_admission_with_owner, PublishedAdmissionOptions,
};

mod readers;
mod turn_chronology;

async fn grant_session_reader(
    client: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    reader: &str,
) -> String {
    let node_did = escape_graphql_string(node_did);
    let session_id = escape_graphql_string(session_id);
    let response = client.execute(&format!(
        r#"{{ AgentSession(filter: {{ node_did: {{ _eq: "{node_did}" }}, session_id: {{ _eq: "{session_id}" }} }}) {{ _docID }} }}"#,
    )).await.unwrap();
    let [row] = response["data"]["AgentSession"]
        .as_array()
        .unwrap()
        .as_slice()
    else {
        panic!("fixture User must see exactly one session");
    };
    let id = row["_docID"].as_str().unwrap().to_owned();
    client
        .add_actor_relationship(
            "AgentSession",
            &id,
            "reader",
            &identity::Did::new(reader).unwrap(),
        )
        .await
        .unwrap();
    id
}

async fn reserve_inventory_session(
    client: &ConfigAccess,
    node_did: &str,
    agent_did: &str,
    session_id: &str,
    created_at: &str,
) -> String {
    let requester = client.actor_did().expect("fixture holds the requester");
    let create = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        format!("reserve-inventory-{session_id}"),
        node_did,
        &requester,
        agent_did,
        session_id,
        "inventory",
        "interactive",
        created_at,
        gents_protocol::request_admission::AgentRequestAdmissionRecord::client(&requester),
    );
    let id = crate::request::lifecycle::reserve_goal_session(client, &create)
        .await
        .unwrap();
    let granted = grant_session_reader(client, node_did, session_id, node_did).await;
    assert_eq!(id, granted);
    id
}

#[tokio::test]
async fn uncaptured_terminal_is_unknown_and_an_empty_closure_is_known() {
    let (fixture, _request) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "inventory-source-unknown".into(),
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
    let requester = tool.requester_did().unwrap().to_owned();
    let session_id = tool.session_id().to_owned();
    let session =
        ConfigAccess::transact_local_readonly(&node, None, "test.inventory.session", |txn| {
            let (did, session_id, requester) = (&fixture.node_did, &session_id, &requester);
            Box::pin(async move {
                crate::store::config_client::agent_session::load_agent_session_row_in_txn(
                    txn,
                    did,
                    session_id,
                    Some(requester),
                )
                .await
            })
        })
        .await
        .unwrap()
        .unwrap();
    let mut admission = FileInventoryAdmission {
        session_doc_id: session.doc_id,
        request: HydrationRequest {
            request_key: "inventory-source-unknown".into(),
            peer_id: "fixture-peer".into(),
            requester_did: requester.clone(),
            node_did: fixture.node_did.clone(),
            session_id,
        },
    };
    let running = FileInventorySnapshot::load(&node, &admission)
        .await
        .unwrap();
    assert_eq!(running.binding.coverage.pending, 1);
    assert_eq!(running.binding.coverage.unknown, 0);
    tool.complete("no retained capture").await.unwrap();
    let terminal = FileInventorySnapshot::load(&node, &admission)
        .await
        .unwrap();
    assert_eq!(terminal.binding.coverage.pending, 0);
    assert_eq!(terminal.binding.coverage.unknown, 1);
    assert!(terminal.entries.is_empty());
    assert_ne!(terminal.binding.digest, running.binding.digest);
    let repeated = FileInventorySnapshot::load(&node, &admission)
        .await
        .unwrap();
    assert_eq!(repeated.binding, terminal.binding);
    reserve_inventory_session(
        &fixture.client,
        &fixture.node_did,
        tool.actor().as_str(),
        "empty-inventory-session",
        "2026-10-24T00:00:00Z",
    )
    .await;
    admission.request.session_id = "empty-inventory-session".into();
    let empty_session =
        ConfigAccess::transact_local_readonly(&node, None, "test.inventory.empty_session", |txn| {
            let admission = &admission;
            Box::pin(async move {
                crate::store::config_client::agent_session::load_agent_session_row_in_txn(
                    txn,
                    &admission.request.node_did,
                    &admission.request.session_id,
                    Some(&admission.request.requester_did),
                )
                .await
            })
        })
        .await
        .unwrap()
        .unwrap();
    admission.session_doc_id = empty_session.doc_id;
    let empty = FileInventorySnapshot::load(&node, &admission)
        .await
        .unwrap();
    assert_eq!(
        empty.binding.coverage,
        FileInventoryCoverage {
            entries: 0,
            pending: 0,
            unknown: 0
        }
    );
    assert!(empty.entries.is_empty());
    assert_ne!(empty.binding.digest, terminal.binding.digest);
    node.shutdown().await.expect("node shutdown");
    tokio::fs::remove_dir_all(fixture.path).await.unwrap();
}

#[tokio::test]
async fn generated_projection_cases_use_native_owners_and_scopes() {
    use base64::Engine as _;
    use gents_protocol::file_changes::{
        FileChangeImages, FileImage, FilePermissions, ToolFileChanges, FILE_CHANGES_VERSION,
    };
    use serde_json::{json, Value};
    let contracts: Value =
        tokio::task::spawn_blocking(gents_lean_contract::load_contract_snapshot::<Value>)
            .await
            .unwrap()
            .unwrap();
    let cases = contracts["file_inventory_source_projection_cases"]
        .as_array()
        .unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let (fixture, mut request_owner) =
            published_admission_with_owner(PublishedAdmissionOptions {
                name: format!("inventory-projection-{name}"),
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
        let model_tool = &case["source"]["tools"][0];
        if model_tool["state"] == "completed" {
            tool.complete("fixture terminal").await.unwrap();
        }
        let scope = FileImageScope {
            requester_did: tool.requester_did().unwrap().into(),
            node_did: tool.node_did().into(),
            session_id: tool.session_id().into(),
        };
        let tool_id = tool.doc_id().unwrap().to_owned();
        let request_id = tool.request_doc_id().unwrap().to_owned();
        let session = ConfigAccess::transact_local_readonly(
            &node,
            None,
            "test.inventory.model_session",
            |txn| {
                let scope = &scope;
                Box::pin(async move {
                    crate::store::config_client::agent_session::load_agent_session_row_in_txn(
                        txn,
                        &scope.node_did,
                        &scope.session_id,
                        Some(&scope.requester_did),
                    )
                    .await
                })
            },
        )
        .await
        .unwrap()
        .unwrap();
        let admission = FileInventoryAdmission {
            session_doc_id: session.doc_id,
            request: HydrationRequest {
                request_key: name.into(),
                peer_id: "fixture-peer".into(),
                requester_did: scope.requester_did.clone(),
                node_did: scope.node_did.clone(),
                session_id: scope.session_id.clone(),
            },
        };
        let root = fixture.path.to_str().unwrap().to_owned();
        let image = |bytes: &[u8]| FileImage::File {
            content_hash: format!("sha256:{:x}", Sha256::digest(bytes)),
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
            permissions: FilePermissions {
                readonly: false,
                unix_mode: None,
            },
            links: Some(1),
        };
        let change = FileChangeImages {
            root: root.clone(),
            path: "src/1.rs".into(),
            before: image(b"before"),
            after: image(b"after"),
        };
        let mut seen_paths = BTreeSet::new();
        let mut paths = Vec::new();
        for projection in case["source"]["projections"].as_array().unwrap() {
            for modeled in projection["changes"].as_array().unwrap() {
                let path = modeled["path"].as_str().unwrap().to_owned();
                if seen_paths.insert(path.clone()) {
                    paths.push(path);
                }
            }
        }
        if paths.is_empty() {
            paths.push(change.path.clone());
        }
        let batch = ToolFileChanges {
            version: FILE_CHANGES_VERSION,
            tool_call_doc_id: tool_id.clone(),
            changes: paths
                .into_iter()
                .map(|path| FileChangeImages {
                    path,
                    ..change.clone()
                })
                .collect(),
        };
        crate::tools::native::FileChanges::decode(&tool_id, serde_json::to_value(&batch).unwrap())
            .unwrap();
        for projection in case["source"]["projections"].as_array().unwrap() {
            let observation_input = json!({ "tool_call_doc_id": tool_id, "request_doc_id": request_id,
                "node_did": scope.node_did, "session_id": scope.session_id, "requester_did": scope.requester_did,
                "file_changes": batch });
            let observation = if projection["observation"].is_null() {
                None
            } else {
                Some(tool.access().unwrap().transact_write("test.inventory.model_observation", |txn| {
                let input = &observation_input;
                Box::pin(async move {
                    let response = txn.execute_with_variables(
                        "mutation($input: ToolFileChangeMutationInputArg!) { create_ToolFileChange(input: $input) { _docID } }",
                        &json!({"input": input})).await?;
                    crate::store::graphql::created_doc_id(&response, "ToolFileChange")
                })
            }).await.unwrap())
            };
            let mut changes = Vec::new();
            for modeled in projection["changes"].as_array().unwrap() {
                let mut metadata = FileChangeMetadata::from(&change);
                metadata.path = modeled["path"].as_str().unwrap().into();
                changes.push(metadata);
            }
            let actor = |value: &Value| match value.as_str().unwrap() {
                "node-1" => scope.node_did.clone(),
                "reader-1" => scope.requester_did.clone(),
                "session-1" => scope.session_id.clone(),
                other => other.to_owned(),
            };
            let projected_request = if projection["request"] == model_tool["request"] {
                request_id.clone()
            } else {
                "different-physical-request".into()
            };
            let input = json!({ "tool_call_doc_id": tool_id, "observation_doc_id": observation,
                "request_doc_id": projected_request, "node_did": actor(&projection["scope"]["node"]),
                "session_id": actor(&projection["scope"]["session"]),
                "requester_did": actor(&projection["scope"]["requester"]),
                "verified_unchanged": projection["verified_unchanged"].as_bool().filter(|value| *value),
                "changes": if changes.is_empty() { None } else { Some(changes) } });
            let foreign = if projection["owner"] != model_tool["agent_did"] {
                let key_path = fixture.path.join("foreign-projection.key");
                Some(
                    tokio::task::spawn_blocking(move || {
                        crate::store::identity::KeyIdentity::load_or_create(key_path, None)
                    })
                    .await
                    .unwrap()
                    .unwrap(),
                )
            } else {
                None
            };
            let creator = if let Some(foreign) = &foreign {
                use crate::store::config_client::{GraphqlEndpoint, NodePermission};
                let actor = identity::Did::new(foreign.did()).unwrap();
                let admin = ConfigAccess::local_as(
                    node.clone(),
                    identity::Did::new(&scope.node_did).unwrap(),
                );
                for permission in [
                    NodePermission::DocumentRead,
                    NodePermission::DocumentUpdate,
                    NodePermission::DacRelationAdd,
                    NodePermission::CollectionGet,
                ] {
                    admin
                        .grant_node_permission(&actor, permission)
                        .await
                        .unwrap();
                }
                let ConfigAccess::Graphql(endpoint) = &fixture.client else {
                    panic!("fixture User uses a client endpoint");
                };
                ConfigAccess::Graphql(
                    GraphqlEndpoint::as_principal(endpoint.url(), foreign.did())
                        .with_client_signing_capability(
                            foreign.client_signing_capability().unwrap(),
                        )
                        .unwrap()
                        .with_client_signed_agent_creates(),
                )
            } else {
                tool.access().unwrap()
            };
            creator.transact_write("test.inventory.model_projection", |txn| {
                let input = &input;
                let node_did = &scope.node_did;
                Box::pin(async move {
                    let response = txn.execute_client_signed_create(
                        "mutation($input: ToolFileInventoryMutationInputArg!) { create_ToolFileInventory(input: $input) { _docID } }",
                        &json!({"input": input}), gents_protocol::schemas::TOOL_FILE_INVENTORY).await?;
                    let id = crate::store::graphql::created_doc_id(&response, "ToolFileInventory")?;
                    txn.add_actor_relationship("ToolFileInventory", &id, "reader", &identity::Did::new(node_did)?).await?;
                    Ok::<_, anyhow::Error>(())
                })
            }).await.unwrap();
        }
        let result = FileInventorySnapshot::load(&node, &admission).await;
        let expected = &case["expected"];
        match expected["kind"].as_str().unwrap() {
            "rejected" => {
                let error = result.expect_err("accepted invalid source projection");
                assert_eq!(
                    serde_json::to_value(refusal_reason(&error)).unwrap(),
                    case["refusal"],
                    "{name}"
                );
            }
            "selected" => {
                let actual = result.unwrap_or_else(|error| panic!("{name}: {error:#}"));
                let facts = expected["snapshot"]["tools"].as_array().unwrap();
                let pending = facts
                    .iter()
                    .filter(|fact| fact["capture"]["kind"] == "pending")
                    .count();
                let unknown = facts
                    .iter()
                    .filter(|fact| fact["capture"]["kind"] == "unknown")
                    .count();
                let entries = facts
                    .iter()
                    .filter_map(|fact| fact["capture"]["projection"]["changes"].as_array())
                    .map(Vec::len)
                    .sum::<usize>();
                assert_eq!(
                    actual.binding.coverage,
                    FileInventoryCoverage {
                        entries: entries as u64,
                        pending: pending as u64,
                        unknown: unknown as u64
                    },
                    "{name}"
                );
                let mut cursor = None;
                let mut issued = None;
                for expected_page in case["pages"].as_array().unwrap() {
                    let page = actual.page(cursor.as_ref()).unwrap();
                    assert!(page.well_formed(), "{name}");
                    assert_eq!(
                        page.offset,
                        expected_page["offset"].as_u64().unwrap(),
                        "{name}"
                    );
                    let expected_entries = expected_page["entries"].as_array().unwrap();
                    assert_eq!(page.entries.len(), expected_entries.len(), "{name}");
                    assert_eq!(
                        page.entries
                            .iter()
                            .map(|entry| entry.change.path.as_str())
                            .collect::<Vec<_>>(),
                        expected_entries
                            .iter()
                            .map(|entry| entry["change"]["path"].as_str().unwrap())
                            .collect::<Vec<_>>(),
                        "{name}"
                    );
                    assert_eq!(
                        page.next.as_ref().map(|next| next.offset),
                        expected_page["next"]["offset"].as_u64(),
                        "{name}"
                    );
                    if issued.is_none() {
                        issued = page.next.clone();
                    }
                    cursor = page.next;
                }
                assert!(cursor.is_none(), "{name}");
                for offset in [0, 1, actual.binding.coverage.entries] {
                    let cursor = FileInventoryCursor {
                        binding: actual.binding.clone(),
                        offset,
                    };
                    assert_eq!(
                        actual.page(Some(&cursor)),
                        Err(FileInventoryPageError::InvalidCursor),
                        "{name}"
                    );
                }
                if let Some(issued) = issued {
                    crate::tools::call::lifecycle::admission_fixture::publish_accepted_on_claimed_request(
                        node.clone(), &mut request_owner, &scope.node_did, 1, "write_file",
                        "later-inventory-tool", json!({"path": "later.rs"}),
                        gents_protocol::tool_call::AwaitMode::Foreground, true,
                    ).await.unwrap();
                    let changed = FileInventorySnapshot::load(&node, &admission)
                        .await
                        .unwrap();
                    assert_eq!(
                        changed.page(Some(&issued)),
                        Err(FileInventoryPageError::StaleSnapshot),
                        "{name}"
                    );
                }
            }
            other => panic!("unhandled source projection outcome {other}"),
        }
        node.shutdown().await.expect("node shutdown");
        tokio::fs::remove_dir_all(fixture.path).await.unwrap();
    }
}

async fn retain_fork_fixture_capture(
    _node: &Arc<EmbeddedNode>,
    tool: &crate::tools::call::lifecycle::ToolCallLifecycle,
    path: &str,
) {
    use base64::Engine as _;
    use gents_protocol::file_changes::*;
    use serde_json::json;
    let bytes = b"fixture bytes";
    let change = FileChangeImages {
        root: "/fixture/workspace".into(),
        path: path.into(),
        before: FileImage::Missing,
        after: FileImage::File {
            content_hash: format!("sha256:{:x}", Sha256::digest(bytes)),
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
            permissions: FilePermissions {
                readonly: false,
                unix_mode: None,
            },
            links: Some(1),
        },
    };
    let changes = ToolFileChanges {
        version: FILE_CHANGES_VERSION,
        tool_call_doc_id: tool.doc_id().unwrap().into(),
        changes: vec![change.clone()],
    };
    crate::tools::native::FileChanges::decode(
        tool.doc_id().unwrap(),
        serde_json::to_value(&changes).unwrap(),
    )
    .unwrap();
    tool.access().unwrap().transact_write("test.inventory.fork_capture", |txn| {
        let changes = &changes;
        let change = &change;
        Box::pin(async move {
            let mut input = json!({"tool_call_doc_id": tool.doc_id().unwrap(), "request_doc_id": tool.request_doc_id().unwrap(),
                "node_did": tool.node_did(), "session_id": tool.session_id(), "requester_did": tool.requester_did(),
                "file_changes": changes});
            let response = txn.execute_with_variables(
                "mutation($input: ToolFileChangeMutationInputArg!) { create_ToolFileChange(input: $input) { _docID } }",
                &json!({"input": input})).await?;
            let observation = crate::store::graphql::created_doc_id(&response, "ToolFileChange")?;
            input.as_object_mut().unwrap().remove("file_changes");
            input["changes"] = json!([FileChangeMetadata::from(change)]);
            input["observation_doc_id"] = json!(observation);
            let response = txn.execute_with_variables(
                "mutation($input: ToolFileInventoryMutationInputArg!) { create_ToolFileInventory(input: $input) { _docID } }",
                &json!({"input": input})).await?;
            let projection = crate::store::graphql::created_doc_id(&response, "ToolFileInventory")?;
            txn.add_actor_relationship("ToolFileInventory", &projection, "reader", &identity::Did::new(tool.node_did())?).await?;
            // The fork's creating requester reads the source projection it copies.
            txn.add_actor_relationship("ToolFileInventory", &projection, "reader", &identity::Did::new(tool.requester_did().unwrap())?).await?;
            Ok::<_, anyhow::Error>(())
        })
    }).await.unwrap();
}

#[tokio::test]
async fn generated_fork_inventory_retains_only_selected_native_origins() {
    use crate::session::{fork_on, ForkBoundary, ForkParams};
    use crate::tools::call::lifecycle::admission_fixture::{
        pending_client_fixture_request, publish_accepted_on_claimed_request,
    };
    use serde_json::Value;
    let contracts: Value =
        tokio::task::spawn_blocking(gents_lean_contract::load_contract_snapshot::<Value>)
            .await
            .unwrap()
            .unwrap();
    let cases = contracts["file_inventory_fork_cases"].as_array().unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let (fixture, mut request_owner) =
            published_admission_with_owner(PublishedAdmissionOptions {
                name: "inventory-native-fork".into(),
                real_identity: true,
                ..Default::default()
            })
            .await
            .unwrap();
        let node = fixture.node;
        let mut origin_tool = fixture.tool;
        let parent = origin_tool.session_id().to_owned();
        let did = origin_tool.node_did().to_owned();
        let requester = origin_tool.requester_did().unwrap().to_owned();
        grant_session_reader(&fixture.client, &did, &parent, &did).await;
        let signer = origin_tool
            .access()
            .unwrap()
            .managed_agent_signer()
            .unwrap();
        origin_tool.complete("origin complete").await.unwrap();
        retain_fork_fixture_capture(&node, &origin_tool, "selected.rs").await;
        request_owner
            .terminalize_owned(
                crate::request::lifecycle::RequestTerminalOutcome::Failed,
                gents_protocol::output::TerminalOutput::Message {
                    message_doc_id: origin_tool.accepted_header_doc_id().unwrap().to_owned(),
                },
                Some("fixture finished"),
            )
            .await
            .unwrap();
        let headers = ConfigAccess::local(node.clone())
            .execute(&format!(
                "{{ AgentMessage(filter: {{ {} }}) {{ {AGENT_MESSAGE_FIELDS} }} }}",
                gents_protocol::session::session_scope_filter(&did, &parent, Some(&requester)),
            ))
            .await
            .unwrap();
        let turns = headers["data"]["AgentMessage"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| crate::session::is_user_turn(row).unwrap())
            .count();
        let child = fork_on(
            &fixture.client,
            ForkParams {
                source_session_id: &parent,
                boundary: ForkBoundary::UserTurn(turns as u32),
                caller_node_did: &did,
                caller_requester_did: Some(&requester),
                target_agent_did: None,
            },
        )
        .await
        .unwrap();
        grant_session_reader(&fixture.client, &did, &child.session_id, &did).await;
        crate::tools::call::lifecycle::admission_fixture::grant_runtime_projection_readers(
            &fixture.client,
            &did,
            &child.session_id,
        )
        .await
        .unwrap();
        let load_admission = |session_id: String| {
            let (node, did, requester) = (node.clone(), did.clone(), requester.clone());
            async move {
                let session = ConfigAccess::transact_local_readonly(
                    &node,
                    None,
                    "test.inventory.fork_session",
                    |txn| {
                        let (did, requester, session_id) = (&did, &requester, &session_id);
                        Box::pin(async move {
                            crate::store::config_client::agent_session::load_agent_session_row_in_txn(
                                txn,
                                did,
                                session_id,
                                Some(requester),
                            )
                            .await
                        })
                    },
                )
                .await
                .unwrap()
                .unwrap();
                FileInventoryAdmission {
                    session_doc_id: session.doc_id,
                    request: HydrationRequest {
                        request_key: format!("fork-inventory-{session_id}"),
                        peer_id: "fixture-peer".into(),
                        node_did: did,
                        requester_did: requester,
                        session_id,
                    },
                }
            }
        };
        let child_admission = load_admission(child.session_id.clone()).await;
        let before_later_parent_work = FileInventorySnapshot::load(&node, &child_admission)
            .await
            .unwrap();
        assert_modeled_resource_order(
            &contracts,
            "forked_prefix_resource",
            &before_later_parent_work,
            &[(origin_tool.doc_id().unwrap(), 90)],
        );
        let make_request = |request_id: &str| {
            gents_protocol::request_admission::AgentRequestCreate::base(
                gents_protocol::request_admission::RequestPurpose::Normal,
                request_id,
                &did,
                &requester,
                signer.did(),
                &parent,
                "spawn",
                "interactive",
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                gents_protocol::request_admission::AgentRequestAdmissionRecord::client(&requester),
            )
        };
        let mut later_request = pending_client_fixture_request(
            &node,
            &fixture.client,
            fixture.runtime.clone(),
            fixture.requester_identity.as_ref(),
            &signer,
            make_request("later-parent-request"),
        )
        .await
        .unwrap();
        assert_eq!(
            later_request.claim_with_identity().await.unwrap(),
            crate::request::lifecycle::ClaimOutcome::Claimed
        );
        let mut later_tool = publish_accepted_on_claimed_request(
            node.clone(),
            &mut later_request,
            &did,
            0,
            "write_file",
            "later-parent-tool",
            serde_json::json!({"path": "selected.rs"}),
            gents_protocol::tool_call::AwaitMode::Foreground,
            true,
        )
        .await
        .unwrap();
        later_tool.complete("later complete").await.unwrap();
        retain_fork_fixture_capture(&node, &later_tool, "selected.rs").await;
        later_request
            .terminalize_owned(
                crate::request::lifecycle::RequestTerminalOutcome::Failed,
                gents_protocol::output::TerminalOutput::Message {
                    message_doc_id: later_tool.accepted_header_doc_id().unwrap().to_owned(),
                },
                Some("fixture finished"),
            )
            .await
            .unwrap();
        let snapshot = FileInventorySnapshot::load(&node, &child_admission)
            .await
            .unwrap();
        let expected = &case["expected"];
        assert_eq!(expected["kind"], "selected");
        let selected = expected["snapshot"]["tools"].as_array().unwrap();
        let expected_entries = selected
            .iter()
            .map(|fact| {
                fact["capture"]["projection"]["changes"]
                    .as_array()
                    .unwrap()
                    .len()
            })
            .sum::<usize>();
        assert_eq!(snapshot.entries.len(), expected_entries);
        assert!(!snapshot.entries.is_empty());
        let source_projections = case["source"]["projections"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|projection| {
                case["source"]["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|tool| tool["scope"]["session"] == projection["scope"]["session"])
            })
            .count();
        for entry in &snapshot.entries {
            assert_eq!(entry.tool_call_doc_id, origin_tool.doc_id().unwrap());
            assert_eq!(entry.scope.session_id, child.session_id);
            assert_ne!(entry.scope.session_id, parent);
            assert_eq!(entry.change.path, "selected.rs");
        }
        let parent_snapshot =
            FileInventorySnapshot::load(&node, &load_admission(parent.clone()).await)
                .await
                .unwrap();
        assert_eq!(parent_snapshot.entries.len(), source_projections);
        assert!(parent_snapshot
            .entries
            .iter()
            .any(|entry| entry.tool_call_doc_id == later_tool.doc_id().unwrap()));
        let physical_tools = [
            (origin_tool.doc_id().unwrap(), 90),
            (later_tool.doc_id().unwrap(), 2),
        ];
        assert_modeled_resource_order(
            &contracts,
            "sequential_resource",
            &parent_snapshot,
            &physical_tools,
        );
        assert_modeled_resource_order(
            &contracts,
            "forked_prefix_resource",
            &snapshot,
            &physical_tools[..1],
        );
        assert_eq!(
            snapshot.binding, before_later_parent_work.binding,
            "unselected parent work must not alter the child's inventory snapshot"
        );
        let headers = ConfigAccess::local(node.clone())
            .execute(&format!(
                "{{ AgentMessage(filter: {{ {} }}) {{ {AGENT_MESSAGE_FIELDS} }} }}",
                gents_protocol::session::session_scope_filter(&did, &parent, Some(&requester)),
            ))
            .await
            .unwrap();
        let turns = headers["data"]["AgentMessage"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| crate::session::is_user_turn(row).unwrap())
            .count();
        let complete_child = fork_on(
            &fixture.client,
            ForkParams {
                source_session_id: &parent,
                boundary: ForkBoundary::UserTurn(turns as u32),
                caller_node_did: &did,
                caller_requester_did: Some(&requester),
                target_agent_did: None,
            },
        )
        .await
        .unwrap();
        grant_session_reader(&fixture.client, &did, &complete_child.session_id, &did).await;
        crate::tools::call::lifecycle::admission_fixture::grant_runtime_projection_readers(
            &fixture.client,
            &did,
            &complete_child.session_id,
        )
        .await
        .unwrap();
        let complete = FileInventorySnapshot::load(
            &node,
            &load_admission(complete_child.session_id.clone()).await,
        )
        .await
        .unwrap();
        assert_modeled_resource_order(
            &contracts,
            "forked_repeated_resource",
            &complete,
            &physical_tools,
        );
        assert!(complete.entries.iter().all(|entry| {
            entry.scope.session_id == complete_child.session_id && entry.scope.session_id != parent
        }));
        let mut unknown_request = pending_client_fixture_request(
            &node,
            &fixture.client,
            fixture.runtime.clone(),
            fixture.requester_identity.as_ref(),
            &signer,
            make_request("unknown-parent-request"),
        )
        .await
        .unwrap();
        assert_eq!(
            unknown_request.claim_with_identity().await.unwrap(),
            crate::request::lifecycle::ClaimOutcome::Claimed
        );
        let mut unknown_tool = publish_accepted_on_claimed_request(
            node.clone(),
            &mut unknown_request,
            &did,
            0,
            "external_tool",
            "uncaptured-tool",
            serde_json::json!({}),
            gents_protocol::tool_call::AwaitMode::Foreground,
            true,
        )
        .await
        .unwrap();
        unknown_tool.complete("uncaptured operation").await.unwrap();
        let with_unknown =
            FileInventorySnapshot::load(&node, &load_admission(parent.clone()).await)
                .await
                .unwrap();
        assert_eq!(with_unknown.binding.coverage.unknown, 1);
        let modeled = contracts["file_inventory_order_comparison_cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["name"] == "unknown_other_request")
            .unwrap();
        for entry in &with_unknown.entries {
            let (_, label) = physical_tools
                .iter()
                .find(|(id, _)| *id == entry.tool_call_doc_id)
                .unwrap();
            let expected = modeled["request_coverage"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["tool"].as_u64() == Some(*label))
                .unwrap();
            assert_eq!(
                entry.request_coverage,
                Some(serde_json::from_value(expected["coverage"].clone()).unwrap())
            );
        }
        assert_eq!(
            FileInventorySnapshot::load(&node, &child_admission)
                .await
                .unwrap()
                .binding,
            snapshot.binding
        );
        // The fork's copies are retained when the source's projection is deleted.
        let origin_projection = parent_snapshot
            .entries
            .iter()
            .find(|entry| entry.tool_call_doc_id == origin_tool.doc_id().unwrap())
            .unwrap()
            .projection_doc_id
            .clone();
        let agent = origin_tool.access().unwrap();
        ConfigAccess::local_as(node.clone(), identity::Did::new(&did).unwrap())
            .grant_node_permission(
                &identity::Did::new(agent.actor_did().unwrap()).unwrap(),
                crate::store::config_client::NodePermission::DocumentDelete,
            )
            .await
            .unwrap();
        let deleted = agent
            .write(
                "test.inventory.delete_source_projection",
                &format!(
                    r#"mutation {{ delete_ToolFileInventory(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
                    crate::store::graphql::escape_graphql_string(&origin_projection),
                ),
            )
            .await
            .unwrap();
        assert_eq!(
            deleted["data"]["delete_ToolFileInventory"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let source_after_delete =
            FileInventorySnapshot::load(&node, &load_admission(parent.clone()).await)
                .await
                .unwrap();
        assert!(source_after_delete
            .entries
            .iter()
            .all(|entry| entry.tool_call_doc_id != origin_tool.doc_id().unwrap()));
        assert_eq!(
            FileInventorySnapshot::load(&node, &child_admission)
                .await
                .unwrap()
                .binding,
            snapshot.binding
        );
        node.shutdown().await.expect("node shutdown");
        tokio::fs::remove_dir_all(fixture.path).await.unwrap();
    }
}

fn assert_modeled_resource_order(
    contracts: &serde_json::Value,
    name: &str,
    snapshot: &FileInventorySnapshot,
    physical_tools: &[(&str, u64)],
) {
    let case = contracts["file_inventory_order_comparison_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == name)
        .unwrap();
    let expected = case["expected"].as_array().unwrap();
    assert_eq!(snapshot.entries.len(), expected.len(), "{name}");
    for entry in &snapshot.entries {
        let (_, label) = physical_tools
            .iter()
            .find(|(document, _)| *document == entry.tool_call_doc_id)
            .unwrap();
        let index = expected
            .iter()
            .position(|id| id.as_u64() == Some(*label))
            .unwrap();
        assert_eq!(
            entry.order,
            Some(gents_protocol::file_inventory::FileInventoryOrdinal {
                index: index as u64,
                entries: expected.len() as u64,
            }),
            "{name}"
        );
    }
}

#[tokio::test]
async fn canonical_terminal_publications_order_real_inventory_rows() {
    check_real_inventory_chronology(false, false).await;
}

#[tokio::test]
async fn background_receipt_cannot_order_real_inventory_rows() {
    check_real_inventory_chronology(true, false).await;
}

#[tokio::test]
async fn completion_notification_orders_real_inventory_rows() {
    check_real_inventory_chronology(true, true).await;
}

async fn check_real_inventory_chronology(background: bool, notify: bool) {
    use crate::tools::call::lifecycle::admission_fixture::publish_accepted_on_claimed_request;
    let contracts: serde_json::Value = tokio::task::spawn_blocking(
        gents_lean_contract::load_contract_snapshot::<serde_json::Value>,
    )
    .await
    .unwrap()
    .unwrap();
    let modeled = contracts["file_inventory_order_comparison_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| {
            case["name"]
                == if notify {
                    "background_completion_reference"
                } else if background {
                    "background_receipt_resource"
                } else {
                    "sequential_resource"
                }
        })
        .unwrap();
    let expected = modeled["expected"].as_array();
    let (fixture, mut request) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "inventory-terminal-chronology".into(),
        real_identity: true,
        await_mode: if background {
            gents_protocol::tool_call::AwaitMode::Background
        } else {
            gents_protocol::tool_call::AwaitMode::Foreground
        },
        ..Default::default()
    })
    .await
    .unwrap();
    let node = fixture.node;
    let mut first = fixture.tool;
    grant_session_reader(
        &fixture.client,
        &fixture.node_did,
        first.session_id(),
        &fixture.node_did,
    )
    .await;
    if background {
        assert!(first
            .publish_background_receipt("running in background")
            .await
            .unwrap());
    }
    first.complete("first retained output").await.unwrap();
    if notify {
        crate::tools::background::completion::append_background_tool_completion(
            &node,
            &first.access().unwrap(),
            first.session_id(),
            &request.request().request_id,
            first.doc_id().unwrap(),
            "write_file",
            "completed",
            "first retained output",
            None,
            gents_protocol::request_lifecycle::RequestHopCause::Continuation,
        )
        .await
        .unwrap();
    }
    retain_fork_fixture_capture(&node, &first, "repeated.rs").await;
    let mut second = publish_accepted_on_claimed_request(
        node.clone(),
        &mut request,
        &fixture.node_did,
        1,
        "write_file",
        "second-chronology-tool",
        serde_json::json!({"path":"repeated.rs"}),
        gents_protocol::tool_call::AwaitMode::Foreground,
        true,
    )
    .await
    .unwrap();
    second.complete("second retained output").await.unwrap();
    retain_fork_fixture_capture(&node, &second, "repeated.rs").await;
    let requester = first.requester_did().unwrap().to_owned();
    let session = ConfigAccess::transact_local_readonly(
        &node,
        Some(identity::Did::new(requester.clone()).unwrap()),
        "test.inventory.chronology_session",
        |txn| {
            let (did, session, requester) = (&fixture.node_did, first.session_id(), &requester);
            Box::pin(async move {
                crate::store::config_client::agent_session::load_agent_session_row_in_txn(
                    txn,
                    did,
                    session,
                    Some(requester),
                )
                .await
            })
        },
    )
    .await
    .unwrap()
    .unwrap();
    let admission = FileInventoryAdmission {
        session_doc_id: session.doc_id,
        request: HydrationRequest {
            request_key: "inventory-terminal-chronology".into(),
            peer_id: "fixture-peer".into(),
            requester_did: requester,
            node_did: fixture.node_did.clone(),
            session_id: first.session_id().into(),
        },
    };
    let inventory = FileInventorySnapshot::load(&node, &admission)
        .await
        .unwrap();
    assert_eq!(
        inventory.entries.len(),
        modeled["snapshot"]["tools"].as_array().unwrap().len()
    );
    for (tool, model_id) in [(&first, 90u64), (&second, 2)] {
        let entry = inventory
            .entries
            .iter()
            .find(|entry| entry.tool_call_doc_id == tool.doc_id().unwrap())
            .unwrap();
        let ordinal = expected.map(|ids| {
            let index = ids
                .iter()
                .position(|id| id.as_u64() == Some(model_id))
                .unwrap();
            gents_protocol::file_inventory::FileInventoryOrdinal {
                index: index.try_into().unwrap(),
                entries: ids.len().try_into().unwrap(),
            }
        });
        assert_eq!(entry.order, ordinal);
    }
    let repeated = FileInventorySnapshot::load(&node, &admission)
        .await
        .unwrap();
    assert_eq!(inventory.binding, repeated.binding);
    node.shutdown().await.expect("node shutdown");
    tokio::fs::remove_dir_all(fixture.path).await.unwrap();
}

#[test]
fn generated_request_coverage_uses_only_selected_physical_scope() {
    use serde_json::Value;
    let contracts: Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let document = |value: &Value| format!("document-{:020}", value.as_u64().unwrap());
    let cases = contracts["file_inventory_order_comparison_cases"]
        .as_array()
        .unwrap();
    for case in cases {
        let mut facts = Vec::new();
        let mut entries = Vec::new();
        for row in case["snapshot"]["tools"].as_array().unwrap() {
            let tool = &row["tool"];
            let scope = FileImageScope {
                node_did: tool["scope"]["node"].as_str().unwrap().into(),
                session_id: tool["scope"]["session"].as_str().unwrap().into(),
                requester_did: tool["scope"]["requester"].as_str().unwrap().into(),
            };
            let capture = match row["capture"]["kind"].as_str().unwrap() {
                "pending" => Capture::Pending,
                "unknown" => Capture::Unknown,
                "observed" => {
                    let value = &row["capture"]["projection"];
                    let projection = Projection {
                        doc_id: document(&value["id"]),
                        tool_call_doc_id: document(&tool["id"]),
                        observation_doc_id: (!value["observation"].is_null())
                            .then(|| document(&value["observation"])),
                        verified_unchanged: value["verified_unchanged"]
                            .as_bool()
                            .filter(|value| *value),
                        request_doc_id: document(&tool["request"]),
                        node_did: scope.node_did.clone(),
                        session_id: scope.session_id.clone(),
                        requester_did: scope.requester_did.clone(),
                        changes: serde_json::from_value(value["changes"].clone()).unwrap(),
                    };
                    for change in &projection.changes {
                        entries.push(FileInventoryEntry {
                            tool_call_doc_id: projection.tool_call_doc_id.clone(),
                            request_doc_id: projection.request_doc_id.clone(),
                            scope: scope.clone(),
                            projection_doc_id: projection.doc_id.clone(),
                            observation_doc_id: projection.observation_doc_id.clone().unwrap(),
                            change: change.clone(),
                            order: None,
                            request_coverage: None,
                        });
                    }
                    Capture::Observed {
                        projection: Box::new(projection),
                        owner: scope.node_did.clone(),
                    }
                }
                other => panic!("unknown modeled capture {other}"),
            };
            facts.push(ToolFact {
                tool_call_doc_id: document(&tool["id"]),
                request_doc_id: document(&tool["request"]),
                scope,
                state: tool["state"].as_str().unwrap().into(),
                accepted_header_doc_id: Some("accepted".into()),
                execution_generation: Some("generation".into()),
                capture,
                order: None,
            });
        }
        apply_request_coverage(&facts, &mut entries);
        for entry in entries {
            let expected = case["request_coverage"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| document(&row["tool"]) == entry.tool_call_doc_id)
                .unwrap();
            assert_eq!(
                entry.request_coverage,
                Some(serde_json::from_value(expected["coverage"].clone()).unwrap()),
                "{}",
                case["name"]
            );
        }
    }
    assert_eq!(cases.len(), 13);
}
