use super::*;
use crate::store::config_client::{GraphqlEndpoint, NodePermission};
use crate::store::identity::KeyIdentity;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::{QueueDelivery, QueuePolicy, QueueSource, RequestQueue};
use gents_protocol::session_input_edit::{PendingMessageEdit, PendingQueueEdit};

#[tokio::test]
async fn signed_user_graph_and_agent_effect_share_the_claim_transaction() -> anyhow::Result<()> {
    let cases =
        &crate::lean_vocab_test::lean_contract_snapshot().session_input_edit_native_claim_cases;
    for case in cases {
        let directory = tempfile::tempdir()?;
        let root: Arc<dyn NodeIdentity> = Arc::new(KeyIdentity::load_or_create(
            directory.path().join("root.key"),
            None,
        )?);
        let user = KeyIdentity::load_or_create(directory.path().join("user.key"), None)?;
        let collaborator =
            KeyIdentity::load_or_create(directory.path().join("collaborator.key"), None)?;
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let node = Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(root.did())
                .with_node_acp_enabled()
                .with_http(defra_node::HttpConfig::with_listener(listener)?)
                .build()
                .await?,
        );
        crate::ensure_runtime_schemas(&node).await?;
        let binding = crate::store::identity::provision_agent_identity(root.as_ref())?;
        let agent = crate::store::identity::resolve_agent_identity(root.as_ref(), &binding)?;
        let admin = ConfigAccess::local_as(node.clone(), root.did().parse()?);
        for did in [user.did(), collaborator.did(), binding.agent_did.as_str()] {
            for permission in [
                NodePermission::DocumentRead,
                NodePermission::DocumentUpdate,
                NodePermission::DacRelationAdd,
                NodePermission::DacRelationDelete,
            ] {
                admin
                    .grant_node_permission(&::identity::Did::new(did)?, permission)
                    .await?;
            }
        }
        let endpoint =
            GraphqlEndpoint::as_principal(format!("http://{addr}/api/v0/graphql"), user.did())
                .with_client_signing_capability(user.client_signing_capability()?)?;
        let access = ConfigAccess::Graphql(endpoint);
        let (node_doc, agent_doc) = access.transact_write("test.edit.seed", |txn| {
            let root = &root;
            let binding = &binding;
            Box::pin(async move {
            let node_response = txn.execute_client_signed_create(
                "mutation($input: NodeMutationInputArg!) { create_Node(input: $input) { _docID } }",
                &serde_json::json!({"input":{"node_did":root.did()}}), gents_protocol::schemas::NODE,
            ).await?;
            let node_doc = crate::store::graphql::created_doc_id(&node_response, "Node")?;
            let agent_response = txn.execute_client_signed_create(
                "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID } }",
                &serde_json::json!({"input":{"node_did":root.did(),"agent_did":binding.agent_did,"creation_nonce":binding.creation_nonce,"inference_profile_id":"fixture","enabled":true}}),
                gents_protocol::schemas::AGENT,
            ).await?;
            let agent_doc = crate::store::graphql::created_doc_id(&agent_response, "Agent")?;
            Ok::<_, anyhow::Error>((node_doc, agent_doc))
        })}).await?;
        access
            .add_actor_relationship(
                "Node",
                &node_doc,
                "runtime_reader",
                &::identity::Did::new(root.did())?,
            )
            .await?;
        access
            .add_resource_relationship("Agent", &agent_doc, "parent", "nodes", &node_doc)
            .await?;
        access
            .add_actor_relationship(
                "Agent",
                &agent_doc,
                "executor",
                &::identity::Did::new(&binding.agent_did)?,
            )
            .await?;
        access
            .add_actor_relationship(
                "Agent",
                &agent_doc,
                "session_writer",
                &::identity::Did::new(collaborator.did())?,
            )
            .await?;
        let mut original = AgentRequestCreate::base(
            RequestPurpose::Normal,
            "original",
            root.did(),
            user.did(),
            &binding.agent_did,
            "session",
            "before",
            "interactive",
            "2026-10-10T00:00:00Z",
            AgentRequestAdmissionRecord::client(user.did()),
        );
        original.input.queue = Some(RequestQueue {
            delivery: QueueDelivery::Queue,
            position: None,
            source: QueueSource::User,
            policy: QueuePolicy::Append,
            key: None,
            queued_after_request_id: None,
            interrupted_request_id: None,
            background_completion_wake_version: None,
        });
        original.session_doc_id =
            Some(crate::request::lifecycle::reserve_goal_session(&access, &original).await?);
        original.agent_doc_id = Some(agent_doc.clone());
        crate::sign_agent_request_create(&user, &mut original).await?;
        crate::request::lifecycle::submit_client_request(&access, &original).await?;
        let old = access
            .execute("{ AgentRequest { _docID request_id } }")
            .await?;
        let old_doc = old["data"]["AgentRequest"][0]["_docID"]
            .as_str()
            .context("original request missing")?
            .to_owned();
        let edit = PendingQueueEdit {
            expected_request_doc_ids: vec![old_doc.clone()],
            selected_request_doc_ids: vec![old_doc.clone()],
            messages: vec![PendingMessageEdit {
                request_doc_id: old_doc.clone(),
                content: "after".into(),
                authored_input: None,
            }],
        };
        let author: &dyn NodeIdentity = if case["selectedAllowed"].as_bool().unwrap() {
            &user
        } else {
            &collaborator
        };
        let edit_access = ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(format!("http://{addr}/api/v0/graphql"), author.did())
                .with_client_signing_capability(author.client_signing_capability()?)?
                .with_client_signed_agent_creates(),
        );
        let replacements = crate::request::lifecycle::queue::prepare_pending_user_edit(
            &edit_access,
            author,
            AgentRequestAdmissionRecord::client(author.did()),
            root.did(),
            "session",
            user.did(),
            &edit,
        )
        .await?;
        let (session_doc_id, agent_did, agent_doc_id, replacement_evidence) =
            test_signed_replacement_fields(
                &edit_access,
                author,
                root.did(),
                "session",
                &replacements,
            )
            .await;
        let mut command = SessionInputEdit {
            version: SESSION_INPUT_EDIT_VERSION,
            command_id: uuid::Uuid::new_v4().to_string(),
            requester_did: author.did().to_owned(),
            requester_peer_id: None,
            node_did: root.did().to_owned(),
            session_id: "session".into(),
            session_doc_id,
            agent_did,
            agent_doc_id,
            issued_at: (Utc::now() - chrono::Duration::seconds(1)).to_rfc3339(),
            expires_at: (Utc::now() + chrono::Duration::minutes(5)).to_rfc3339(),
            edit,
            replacements,
            replacement_evidence,
            signature: vec![],
        };
        command.signature = author.sign(&command.signing_payload()).await?;
        let mut altered = command.replacement_evidence[0].clone();
        let signature = altered["composite_signature"]
            .as_array_mut()
            .context("signature evidence missing")?;
        let byte = signature[0].as_u64().context("signature byte missing")?;
        signature[0] = serde_json::json!(byte ^ 1);
        let input = crate::store::config_client::client_request_input(&command.replacements[0])?;
        let tampered_accepted =
            crate::store::config_client::verify_request_evidence(input, altered)
                .await
                .is_ok();
        assert_eq!(
            tampered_accepted,
            case["tamperedAccepted"].as_bool().unwrap()
        );
        let command_doc =
            crate::request::lifecycle::queue::publish_edit_command(&edit_access, &command).await?;
        let command_row = access.execute(&format!(r#"{{ AgentSessionInputEdit(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _version {{ cid height fieldName }} }} }}"#, escape_graphql_string(&command_doc))).await?;
        let command_version = crate::store::graphql::document_composite_version(
            &command_row["data"]["AgentSessionInputEdit"][0],
            "edit command",
        )?
        .context("command version missing")?;
        assert_eq!(
            node.verified_block_signer_did(&command_version.cid).await?,
            author.did()
        );
        if !case["selectedAllowed"]
            .as_bool()
            .context("modeled selection missing")?
        {
            let selected_agent = ConfigAccess::local_managed_agent(
                node.clone(),
                ::identity::Did::new(agent.did())?,
                agent.as_ref(),
            )?;
            let subject = ::identity::Did::new(author.did())?;
            let observed = selected_agent
                .transact_readonly("test.edit.selected_permission", |txn| {
                    let subject = &subject;
                    let session = &command.session_doc_id;
                    let agent_doc = &command.agent_doc_id;
                    let old_doc = &old_doc;
                    Box::pin(async move {
                        let named = |permission: &str| defra_node::DocumentAccessMode::Named {
                            permission: permission.into(),
                            node_permission: NodePermission::DocumentUpdate,
                        };
                        let session = txn
                            .observe_document_access(
                                subject,
                                "AgentSession",
                                std::slice::from_ref(session),
                                named("update"),
                            )
                            .await?;
                        let agent = txn
                            .observe_document_access(
                                subject,
                                "Agent",
                                std::slice::from_ref(agent_doc),
                                named("invoke"),
                            )
                            .await?;
                        let selected = txn
                            .observe_document_access(
                                subject,
                                "AgentRequest",
                                std::slice::from_ref(old_doc),
                                named("update"),
                            )
                            .await?;
                        Ok::<_, anyhow::Error>((session, agent, selected))
                    })
                })
                .await?;
            assert_eq!(
                observed.0.as_slice(),
                [defra_node::DocumentAccessDecision::Allowed]
            );
            assert_eq!(
                observed.1.as_slice(),
                [defra_node::DocumentAccessDecision::Allowed]
            );
            assert_eq!(
                observed.2.as_slice(),
                [defra_node::DocumentAccessDecision::Denied]
            );
        }
        if case["selectedAllowed"].as_bool().unwrap() {
            let (failure, mutations) = ConfigApplyTxn::with_successful_mutation_failure_at(
                Some(1),
                process_command(&node, &root, &command_doc, agent.as_ref()),
            )
            .await;
            assert!(failure.is_err());
            assert_eq!(mutations, 1);
            let after_abort = access.execute("{ AgentRequest { _docID lifecycle_state } AgentSessionInputEdit { receipt_json } }").await?;
            let rows = after_abort["data"]["AgentRequest"]
                .as_array()
                .context("requests unavailable after abort")?;
            assert_eq!(
                rows.len() > 1,
                case["rollbackReplacement"].as_bool().unwrap()
            );
            assert_eq!(rows[0]["lifecycle_state"], "pending");
            assert_eq!(
                !after_abort["data"]["AgentSessionInputEdit"][0]["receipt_json"].is_null(),
                case["rollbackReceipt"].as_bool().unwrap()
            );
        }
        process_command(&node, &root, &command_doc, agent.as_ref()).await?;
        let response = access.execute(&format!(r#"{{ AgentSessionInputEdit(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ receipt_json }} }}"#, escape_graphql_string(&command_doc))).await?;
        let receipt: SessionInputEditReceipt = serde_json::from_str(
            response["data"]["AgentSessionInputEdit"][0]["receipt_json"]
                .as_str()
                .context("receipt missing")?,
        )?;
        receipt.validate_for(&command)?;
        assert!(verified_receipt(
            root.as_ref(),
            &command,
            response["data"]["AgentSessionInputEdit"][0]["receipt_json"].as_str()
        )
        .await?
        .is_some());
        assert_eq!(
            receipt.outcome == SessionInputEditOutcome::Applied,
            case["applied"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
        let old = access.execute(&format!(r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state superseded_by_request_doc_id }} }}"#, escape_graphql_string(&old_doc))).await?;
        if receipt.outcome == SessionInputEditOutcome::Applied {
            assert_eq!(
                old["data"]["AgentRequest"][0]["lifecycle_state"],
                "superseded"
            );
            assert_eq!(
                old["data"]["AgentRequest"][0]["superseded_by_request_doc_id"],
                receipt.request_doc_ids[0]
            );
            let replacement = access.execute(&format!(r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _version {{ cid height fieldName }} }} }}"#, escape_graphql_string(&receipt.request_doc_ids[0]))).await?;
            let version = crate::store::graphql::document_composite_version(
                &replacement["data"]["AgentRequest"][0],
                "replacement",
            )?
            .context("replacement version missing")?;
            assert_eq!(
                node.verified_block_signer_did(&version.cid).await?,
                user.did()
            );
            let agent_access = ConfigAccess::local_managed_agent(
                node.clone(),
                ::identity::Did::new(&binding.agent_did)?,
                agent.as_ref(),
            )?;
            let request = agent_access
                .execute(&format!(
                    r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
                    escape_graphql_string(&receipt.request_doc_ids[0]),
                    crate::request::watcher::AGENT_REQUEST_FIELDS,
                ))
                .await?;
            let row: gents_protocol::row::AgentRequestRow =
                serde_json::from_value(request["data"]["AgentRequest"][0].clone())?;
            let request = crate::request::watcher::AgentRequest::try_from(row)?;
            let (_owner, authority) =
                crate::runtime::agent::p2p_reconcile::enrollment_authority_channel();
            let verifier = crate::request::admission::AgentRequestAdmissionVerifier::new(
                node.clone(),
                root.clone(),
                authority,
            );
            let (request, admission) = verifier
                .verify_fresh_for_claim(&request, agent.did())
                .await?;
            assert!(
                admission.is_some(),
                "User-authored replacement requires native client claim admission"
            );
            let mut lifecycle =
                crate::request::lifecycle::RequestLifecycle::new_with_execution_binding(
                    node.clone(),
                    agent.did(),
                    root.did(),
                    request,
                    60,
                    crate::request::lifecycle::ExecutionOrigin::Interactive,
                    String::new(),
                );
            lifecycle.bind_managed_agent(agent.as_ref())?;
            lifecycle.set_client_claim_admission(admission);
            assert!(matches!(
                lifecycle.claim_with_identity().await?,
                crate::request::lifecycle::ClaimOutcome::Claimed
            ));
            let claimed = agent_access.execute(&format!(
                r#"{{ AgentRequest(docID: "{}") {{ lifecycle_state _version {{ cid height fieldName }} }} }}"#,
                escape_graphql_string(&receipt.request_doc_ids[0]),
            )).await?;
            assert_eq!(
                claimed["data"]["AgentRequest"][0]["lifecycle_state"],
                "claimed"
            );
            let claim_version = crate::store::graphql::document_composite_version(
                &claimed["data"]["AgentRequest"][0],
                "claimed replacement",
            )?
            .context("claim version missing")?;
            assert_eq!(
                node.verified_block_signer_did(&claim_version.cid).await?,
                agent.did()
            );
            let mut removed_request = original.clone();
            removed_request.request_id = "removal-only".into();
            removed_request.content = "remove this pending message".into();
            crate::sign_agent_request_create(&user, &mut removed_request).await?;
            crate::request::lifecycle::submit_client_request(&access, &removed_request).await?;
            let pending = access
                .execute(
                    r#"{ AgentRequest(filter: {request_id: {_eq: "removal-only"}}) {_docID} }"#,
                )
                .await?;
            let removed_doc = pending["data"]["AgentRequest"][0]["_docID"]
                .as_str()
                .context("removal request missing")?
                .to_owned();
            let mut removal = SessionInputEdit {
                version: SESSION_INPUT_EDIT_VERSION,
                command_id: uuid::Uuid::new_v4().to_string(),
                requester_did: user.did().into(),
                requester_peer_id: None,
                node_did: root.did().into(),
                session_id: command.session_id.clone(),
                session_doc_id: command.session_doc_id.clone(),
                agent_did: command.agent_did.clone(),
                agent_doc_id: command.agent_doc_id.clone(),
                issued_at: (Utc::now() - chrono::Duration::seconds(1)).to_rfc3339(),
                expires_at: (Utc::now() + chrono::Duration::minutes(5)).to_rfc3339(),
                edit: PendingQueueEdit {
                    expected_request_doc_ids: vec![removed_doc.clone()],
                    selected_request_doc_ids: vec![removed_doc.clone()],
                    messages: vec![],
                },
                replacements: vec![],
                replacement_evidence: vec![],
                signature: vec![],
            };
            removal.signature = user.sign(&removal.signing_payload()).await?;
            let removal_doc =
                crate::request::lifecycle::queue::publish_edit_command(&access, &removal).await?;
            process_command(&node, &root, &removal_doc, agent.as_ref()).await?;
            let removed = access
                .execute(&format!(
                    r#"{{ AgentRequest(docID: "{}") {{ lifecycle_state failure_reason }} }}"#,
                    escape_graphql_string(&removed_doc)
                ))
                .await?;
            assert_eq!(
                removed["data"]["AgentRequest"][0]["lifecycle_state"],
                "interrupted"
            );
            agent_access.write("test.edit.lost_removal_receipt", &format!(r#"mutation {{ update_AgentSessionInputEdit(docID: "{}", input: {{ receipt_json: "{{}}" }}) {{ _docID }} }}"#, escape_graphql_string(&removal_doc))).await?;
            process_command(&node, &root, &removal_doc, agent.as_ref()).await?;
            let uncertain = access.execute(&format!(r#"{{ AgentSessionInputEdit(docID: "{}") {{ receipt_json }} AgentRequest(docID: "{}") {{ lifecycle_state failure_reason }} }}"#, escape_graphql_string(&removal_doc), escape_graphql_string(&removed_doc))).await?;
            assert_eq!(
                uncertain["data"]["AgentSessionInputEdit"][0]["receipt_json"],
                "{}"
            );
            assert_eq!(
                uncertain["data"]["AgentRequest"][0],
                removed["data"]["AgentRequest"][0]
            );
            assert_eq!(case["unprovenRemovalPending"], true);
            access
                .delete_actor_relationship(
                    "AgentRequest",
                    &receipt.request_doc_ids[0],
                    "working_agent",
                    &::identity::Did::new(&binding.agent_did)?,
                )
                .await?;
            process_command(&node, &root, &command_doc, agent.as_ref()).await?;
            let decision = agent_access
                .transact_readonly("test.edit.revoke", |txn| {
                    let doc = &receipt.request_doc_ids[0];
                    let agent_did = &binding.agent_did;
                    Box::pin(async move {
                        let decisions = txn
                            .observe_document_access(
                                &::identity::Did::new(agent_did)?,
                                "AgentRequest",
                                std::slice::from_ref(doc),
                                defra_node::DocumentAccessMode::Named {
                                    permission: "update".into(),
                                    node_permission: NodePermission::DocumentUpdate,
                                },
                            )
                            .await?;
                        Ok::<_, anyhow::Error>(decisions)
                    })
                })
                .await?;
            let no_regrant = decision.as_slice() == [defra_node::DocumentAccessDecision::Denied];
            assert_eq!(no_regrant, case["noRegrant"].as_bool().unwrap());
        } else {
            assert_eq!(old["data"]["AgentRequest"][0]["lifecycle_state"], "pending");
            assert!(receipt.request_doc_ids.is_empty());
        }
        node.shutdown().await?;
    }
    Ok(())
}
