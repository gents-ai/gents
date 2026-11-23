use super::*;
use crate::request::admission::AgentRequestAdmissionVerifier;
use crate::request::lifecycle::{ClaimOutcome, ExecutionOrigin, RequestLifecycle};
use gents_protocol::row::AgentRequest;
use gents_store::config_client::NodePermission;

type ClaimInput = gents_lean_vocab::families::cross_actor_authorization_signed_client_claim::ClaimInput;

/// The served node, identities and targets one claim case runs against. The
/// `owner` holds the Agent document; the `user` is the session's requester;
/// the request's signer is the user or a `collaborator` whose update on the
/// user's session is a relation the user grants.
struct ClaimWorld {
    db: gents_inference::test_support::TestDb,
    node: Arc<defra_node::Node>,
    runtime: Arc<dyn NodeIdentity>,
    admin: ConfigAccess,
    user: KeyIdentity,
    owner: KeyIdentity,
    collaborator: KeyIdentity,
    signer_is_collaborator: bool,
    agent_doc: String,
    session_doc: String,
}

impl ClaimWorld {
    fn signer(&self) -> &KeyIdentity {
        if self.signer_is_collaborator {
            &self.collaborator
        } else {
            &self.user
        }
    }

    fn access(&self, identity: &KeyIdentity) -> ConfigAccess {
        self.db.client_access(identity)
    }

    async fn client_update(
        &self,
        identity: &KeyIdentity,
        collection: &str,
        schema: &'static str,
        doc: &str,
        input: serde_json::Value,
    ) {
        let mutation = format!(
            r#"mutation($input: {collection}MutationInputArg!) {{ update_{collection}(docID: "{}", input: $input) {{ _docID }} }}"#,
            gents_store::graphql::escape_graphql_string(doc)
        );
        self.access(identity)
            .transact_write("test.claim.client_update", |txn| {
                let (mutation, input) = (&mutation, &input);
                Box::pin(async move {
                    txn.execute_client_signed_update(
                        mutation,
                        &serde_json::json!({ "input": input }),
                        schema,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await
            .unwrap();
    }

    async fn apply(&self, write: &str) {
        let signer = self.signer().did().parse().unwrap();
        let collaborator = self.collaborator.did().parse().unwrap();
        let (user, owner) = (self.access(&self.user), self.access(&self.owner));
        match write {
            "revoke_session_update" => user
                .delete_actor_relationship("AgentSession", &self.session_doc, "working_agent", &collaborator)
                .await
                .unwrap(),
            "grant_session_update" => user
                .add_actor_relationship("AgentSession", &self.session_doc, "working_agent", &collaborator)
                .await
                .unwrap(),
            "revoke_invoke" => owner
                .delete_actor_relationship("Agent", &self.agent_doc, "session_writer", &signer)
                .await
                .unwrap(),
            "grant_invoke" => owner
                .add_actor_relationship("Agent", &self.agent_doc, "session_writer", &signer)
                .await
                .unwrap(),
            "revoke_node_permission" => self
                .admin
                .revoke_node_permission(&signer, NodePermission::DocumentUpdate)
                .await
                .unwrap(),
            "grant_node_permission" => self
                .admin
                .grant_node_permission(&signer, NodePermission::DocumentUpdate)
                .await
                .unwrap(),
            "touch_agent" => {
                self.client_update(
                    &self.owner,
                    "Agent",
                    gents_protocol::schemas::AGENT,
                    &self.agent_doc,
                    serde_json::json!({ "inference_profile_id": "touched" }),
                )
                .await
            }
            other => panic!("unmodeled write {other}"),
        }
    }

    async fn apply_all(&self, writes: &[String]) {
        for write in writes {
            self.apply(write).await;
        }
    }

    /// Creates an AgentRequest from `input` as `creator`, with the request's
    /// ACP relationships unless the selected Agent must not read it.
    async fn raw_request(
        &self,
        creator: &KeyIdentity,
        input: &serde_json::Value,
        agent_doc: &str,
        agent_did: &str,
        related: bool,
    ) -> String {
        self.access(creator)
            .transact_write("test.claim.raw_request", |txn| {
                let session = self.session_doc.as_str();
                Box::pin(async move {
                    let response = txn
                        .execute_client_signed_create(
                            "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                            &serde_json::json!({ "input": input }),
                            gents_protocol::schemas::AGENT_REQUEST,
                        )
                        .await?;
                    let request = gents_store::graphql::created_doc_id(&response, "AgentRequest")?;
                    if related {
                        crate::request::lifecycle::client::stage_request_relationships_in_txn(
                            txn, &request, agent_doc, session, agent_did,
                        )
                        .await?;
                    }
                    Ok::<_, anyhow::Error>(request)
                })
            })
            .await
            .unwrap()
    }

    async fn request_row(&self, reader: &ConfigAccess, doc: &str) -> AgentRequest {
        let response = reader
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
                gents_store::graphql::escape_graphql_string(doc),
                gents_protocol::request_admission::SIGNED_REQUEST_FIELDS
            ))
            .await
            .unwrap();
        let row: gents_protocol::row::AgentRequestRow =
            serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
        AgentRequest::try_from(row).unwrap()
    }
}

async fn authorize_directly(
    agent_access: &ConfigAccess,
    admission: &crate::request::admission::ClientClaimAdmission,
    request: &AgentRequest,
) -> &'static str {
    use crate::request::admission::ClientClaimAuthorization::{Allowed, Denied, Unavailable};
    match agent_access
        .transact_readonly("test.claim.authorize", |txn| {
            Box::pin(async move { admission.authorize_in_txn(txn, request).await })
        })
        .await
    {
        Ok(Allowed) => "claimed",
        Ok(Denied) => "denied",
        Ok(Unavailable) => "unavailable",
        Err(_) => "error",
    }
}

/// Builds the case's world through the production writers, then replays its
/// claim against `ClientClaimAdmission::authorize_in_txn` directly (the
/// decision) and through the claim transaction (the request's effect).
#[tokio::test]
async fn signed_client_claim_uses_transaction_bound_intent_and_native_permissions() {
    for (index, case) in
        gents_lean_vocab::families::cross_actor_authorization_signed_client_claim::cases()
            .into_iter()
            .enumerate()
    {
        let input: &ClaimInput = &case.input;
        let name = format!("{}-{index}", input.name);
        let home = tempfile::tempdir().unwrap();
        let key = |file: &str| KeyIdentity::load_or_create(home.path().join(file), None).unwrap();
        let (user, owner, collaborator) = (key("user.key"), key("owner.key"), key("collaborator.key"));
        let (db, runtime) = serve_client_node(
            "signed-client-claim-uses-transaction-bou",
            crate::test_support::no_lifecycle_plugins(),
        )
        .await;
        let runtime: Arc<dyn NodeIdentity> = Arc::new(runtime);
        let node = db.node.clone();
        for identity in [&user, &owner, &collaborator] {
            grant_operator_user(&node, runtime.as_ref(), identity).await;
        }
        let binding = gents_store::identity::provision_agent_identity(runtime.as_ref()).unwrap();
        let admin = ConfigAccess::local_as(node.clone(), runtime.did().parse().unwrap());
        let subject = ::identity::Did::new(binding.agent_did.as_str()).unwrap();
        for permission in [
            NodePermission::CollectionGet,
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::DacRelationAdd,
        ] {
            admin
                .grant_node_permission(&subject, permission)
                .await
                .unwrap();
        }
        let owner_access = db.client_access(&owner);
        seed_owned_agent(&owner_access, runtime.did(), &binding).await;
        let agent_doc = owner_access.execute("{ Agent { _docID } }").await.unwrap()["data"]
            ["Agent"][0]["_docID"]
            .as_str()
            .unwrap()
            .to_owned();
        let mut world = ClaimWorld {
            db,
            node: node.clone(),
            runtime: runtime.clone(),
            admin,
            user,
            owner,
            collaborator,
            signer_is_collaborator: input.author == "collaborator",
            agent_doc,
            session_doc: String::new(),
        };
        let signer_did: ::identity::Did = world.signer().did().parse().unwrap();
        let invoke_holder = if input.agent_invoke { "session_writer" } else { "reader" };
        owner_access
            .add_actor_relationship("Agent", &world.agent_doc, invoke_holder, &signer_did)
            .await
            .unwrap();
        for identity in [&world.user, &world.collaborator] {
            if identity.did() != world.signer().did() {
                owner_access
                    .add_actor_relationship(
                        "Agent",
                        &world.agent_doc,
                        "session_writer",
                        &identity.did().parse().unwrap(),
                    )
                    .await
                    .unwrap();
            }
        }
        let user_access = world.access(&world.user);
        let scope = |session: String| {
            RequestScope::new(runtime.did(), world.user.did(), &binding.agent_did, session)
        };
        let mut create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            format!("model-{name}"),
            scope(format!("session-{name}")),
            "signed text",
            if input.scheduled { "scheduled" } else { "interactive" },
            "2026-11-04T00:00:00Z",
            AgentRequestAdmissionRecord::client(world.signer().did()),
        );
        if input.steer {
            create.input.queue = Some(gents_protocol::request_input::RequestQueue {
                delivery: gents_protocol::request_input::QueueDelivery::Steer,
                source: gents_protocol::request_input::QueueSource::User,
                policy: gents_protocol::request_input::QueuePolicy::Append,
                position: None,
                key: None,
                queued_after_request_id: None,
                interrupted_request_id: None,
                background_completion_wake_version: None,
            });
        }
        let reserve = |session: String| {
            let mut reservation = create.clone();
            reservation.session_id = session;
            reservation.admission = AgentRequestAdmissionRecord::client(world.user.did());
            reservation
        };
        world.session_doc = reserve_goal_session(&user_access, &reserve(create.session_id.clone()))
            .await
            .unwrap();
        if world.signer_is_collaborator || input.owner.as_deref() == Some("collaborator") {
            let relation = if input.session_update { "working_agent" } else { "reader" };
            user_access
                .add_actor_relationship(
                    "AgentSession",
                    &world.session_doc,
                    relation,
                    &world.collaborator.did().parse().unwrap(),
                )
                .await
                .unwrap();
        }
        create.session_doc_id = Some(if input.named_session_matches {
            world.session_doc.clone()
        } else {
            reserve_goal_session(&user_access, &reserve(format!("other-session-{name}")))
                .await
                .unwrap()
        });
        create.agent_doc_id = Some(if input.agent_row == "other" {
            let other = gents_store::identity::provision_agent_identity(runtime.as_ref()).unwrap();
            let other_doc = owner_access
                .transact_write("test.claim.other_agent", |txn| {
                    let (node_did, other) = (runtime.did(), &other);
                    Box::pin(async move {
                        let created = txn
                            .execute_client_signed_create(
                                "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID } }",
                                &serde_json::json!({"input": {
                                    "node_did": node_did, "agent_did": other.agent_did,
                                    "creation_nonce": other.creation_nonce,
                                    "inference_profile_id": "fixture", "enabled": true,
                                }}),
                                gents_protocol::schemas::AGENT,
                            )
                            .await?;
                        Ok::<_, anyhow::Error>(gents_store::graphql::created_doc_id(&created, "Agent")?)
                    })
                })
                .await
                .unwrap();
            for (relation, did) in [("reader", binding.agent_did.as_str()), ("session_writer", world.signer().did())] {
                owner_access
                    .add_actor_relationship("Agent", &other_doc, relation, &did.parse().unwrap())
                    .await
                    .unwrap();
            }
            other_doc
        } else {
            world.agent_doc.clone()
        });
        crate::request::admission::sign_agent_request_create(world.signer(), &mut create)
            .await
            .unwrap();
        let signed_session = create.session_doc_id.clone().unwrap();
        let signed_agent = create.agent_doc_id.clone().unwrap();
        let request_creator = if input.owner.as_deref() == Some("collaborator") {
            &world.collaborator
        } else {
            world.signer()
        };
        let request_doc = if request_creator.did() != world.signer().did() {
            let input = gents_store::config_client::client_request_input(&create).unwrap();
            world
                .raw_request(request_creator, &input, &signed_agent, &binding.agent_did, true)
                .await
        } else if input.named_session_matches && input.agent_row != "other" {
            let signer_access = world.access(world.signer());
            submit_client_request(&signer_access, &create).await.unwrap();
            signer_access
                .execute(&format!(
                    r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
                    gents_store::graphql::escape_graphql_string(&create.request_id)
                ))
                .await
                .unwrap()["data"]["AgentRequest"][0]["_docID"]
                .as_str()
                .unwrap()
                .to_owned()
        } else {
            world
                .access(world.signer())
                .transact_write("test.claim.stage", |txn| {
                    let (create, session, agent) = (&create, &signed_session, &signed_agent);
                    Box::pin(async move {
                        stage_client_request_in_txn(txn, create, session, agent).await
                    })
                })
                .await
                .unwrap()
        };
        let agent_identity =
            gents_store::identity::resolve_agent_identity(runtime.as_ref(), &binding).unwrap();
        let agent_access = ConfigAccess::local_managed_agent(
            node.clone(),
            binding.agent_did.parse().unwrap(),
            agent_identity.as_ref(),
        )
        .unwrap();
        let retained = world.request_row(&agent_access, &request_doc).await;
        let (_owner, authority) = gents_p2p::p2p_reconcile::enrollment_authority_channel();
        let verifier = AgentRequestAdmissionVerifier::new(node.clone(), runtime.clone(), authority);
        let (retained, admission) = verifier
            .verify_fresh_for_claim(&retained, &binding.agent_did)
            .await
            .unwrap();
        let mut admission = admission.expect("a client request retains its claim admission");
        if !input.signature_valid {
            admission.alter_signed_content_for_test();
        }
        let creator_access = world.access(request_creator);
        let (claimed, claimed_reader) = if input.claimed_document == "replayed" {
            let mut replay = gents_store::config_client::client_request_input(&create).unwrap();
            replay["relationships_pending"] = true.into();
            if !input.live_author_present {
                replay
                    .as_object_mut()
                    .unwrap()
                    .remove("admission_signer_did");
            }
            let replayed = world
                .raw_request(
                    request_creator,
                    &replay,
                    &signed_agent,
                    &binding.agent_did,
                    input.request_read != "absent",
                )
                .await;
            world
                .client_update(
                    request_creator,
                    "AgentRequest",
                    gents_protocol::schemas::AGENT_REQUEST,
                    &replayed,
                    serde_json::json!({ "relationships_pending": false }),
                )
                .await;
            (world.request_row(&creator_access, &replayed).await, replayed)
        } else {
            (retained, request_doc.clone())
        };
        if input.request_read == "pending" {
            world
                .client_update(
                    request_creator,
                    "AgentRequest",
                    gents_protocol::schemas::AGENT_REQUEST,
                    &claimed_reader,
                    serde_json::json!({ "relationships_pending": true }),
                )
                .await;
        }
        if input.session_read == "pending" {
            world
                .client_update(
                    &world.user,
                    "AgentSession",
                    gents_protocol::schemas::AGENT_SESSION,
                    &world.session_doc,
                    serde_json::json!({ "relationships_pending": true }),
                )
                .await;
        }
        match input.agent_row.as_str() {
            "absent" => owner_access
                .delete_actor_relationship("Agent", &world.agent_doc, "executor", &subject)
                .await
                .unwrap(),
            "disabled" => {
                world
                    .client_update(
                        &world.owner,
                        "Agent",
                        gents_protocol::schemas::AGENT,
                        &world.agent_doc,
                        serde_json::json!({ "enabled": false }),
                    )
                    .await
            }
            _ => {}
        }
        if !input.requester_node_permission {
            world.apply("revoke_node_permission").await;
        }
        world.apply_all(&input.before).await;
        let lifecycle_for = |request: &AgentRequest| {
            let mut lifecycle = RequestLifecycle::new_with_execution_binding(
                node.clone(),
                request.clone(),
                60,
                if input.scheduled {
                    ExecutionOrigin::Scheduled
                } else {
                    ExecutionOrigin::Interactive
                },
                String::new(),
            );
            lifecycle.bind_managed_agent(agent_identity.as_ref()).unwrap();
            lifecycle.set_client_claim_admission(Some(admission.clone()));
            lifecycle
        };
        if input.prior_claimed {
            assert!(matches!(
                lifecycle_for(&claimed).claim_with_identity().await.unwrap(),
                ClaimOutcome::Claimed
            ));
        }
        let read_failure = "relationships_pending";
        let decision = if input.transient_failure {
            gents_store::config_client::ConfigApplyTxn::with_read_storage_failure(
                read_failure,
                authorize_directly(&agent_access, &admission, &claimed),
            )
            .await
        } else {
            authorize_directly(&agent_access, &admission, &claimed).await
        };
        let mut lifecycle = lifecycle_for(&claimed);
        let result = if input.transient_failure {
            gents_store::config_client::ConfigApplyTxn::with_read_storage_failure(
                read_failure,
                lifecycle.claim_with_identity(),
            )
            .await
        } else if input.race.is_empty() {
            lifecycle.claim_with_identity().await
        } else {
            let (result, raced) =
                gents_store::config_client::ConfigApplyTxn::with_concurrent_write_before_commit(
                    lifecycle.claim_with_identity(),
                    world.apply_all(&input.race),
                )
                .await;
            assert!(raced, "{name}: the race lands inside the claim's first attempt");
            result
        };
        let claimed_now = matches!(result.as_ref(), Ok(ClaimOutcome::Claimed));
        let state = creator_access
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state failure_reason terminal_output }} }}"#,
                gents_store::graphql::escape_graphql_string(&claimed_reader)
            ))
            .await
            .unwrap();
        let row = &state["data"]["AgentRequest"][0];
        let terminal_output = (!row["terminal_output"].is_null()).then(|| {
            let output: gents_protocol::output::TerminalOutput =
                serde_json::from_value(row["terminal_output"].clone()).unwrap();
            match output {
                gents_protocol::output::TerminalOutput::NoMessage => "no_message".to_owned(),
                _ => "message".to_owned(),
            }
        });
        case.check(
            gents_lean_vocab::families::cross_actor_authorization_signed_client_claim::ClaimOutput {
                decision: decision.to_owned(),
                lifecycle: row["lifecycle_state"].as_str().unwrap().to_owned(),
                terminal_output,
                failure_reason: row["failure_reason"]
                    .as_str()
                    .filter(|reason| !reason.is_empty())
                    .map(str::to_owned),
            },
        );
        if claimed_now && input.name == "committed_claim" {
            assert_claimed_execution(&world, &mut lifecycle, &agent_access, agent_identity.clone(), input.scheduled).await;
        }
        drop(lifecycle);
        world.node.shutdown().await.expect("shut down test node");
    }
}

/// A committed claim owns its execution: the node itself cannot read the
/// request, recovery repairs its expired lease, and a scheduled claim's
/// background wake is ineligible.
async fn assert_claimed_execution(
    world: &ClaimWorld,
    lifecycle: &mut RequestLifecycle,
    agent_access: &ConfigAccess,
    agent_identity: Arc<dyn NodeIdentity>,
    scheduled: bool,
) {
    let node = &world.node;
    assert!(lifecycle.owns_execution().await.unwrap());
    if scheduled {
        let writer = crate::request::streaming::DefraStreamWriter::new_managed(
            node.clone(),
            agent_identity.as_ref(),
            std::time::Duration::ZERO,
        )
        .unwrap();
        lifecycle.begin_owned_execution(&writer).await.unwrap();
        lifecycle.validate_owned_execution().await.unwrap();
    }
    let node_view = gents_store::test_support::unrelated_document_reader(node)
        .await
        .execute("{ AgentRequest { _docID } }")
        .await
        .unwrap();
    assert!(node_view["data"]["AgentRequest"]
        .as_array()
        .unwrap()
        .is_empty());
    let doc_id = gents_store::graphql::escape_graphql_string(&lifecycle.request().doc_id);
    agent_access
        .write(
            "test.expire_claim",
            &format!(
                r#"mutation {{ update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                input: {{ execution_lease_expires_at: "2000-01-01T00:00:00Z" }}
            ) {{ _docID }} }}"#
            ),
        )
        .await
        .unwrap();
    let recovery = RequestLifecycle::repair_terminal_requests_with_identities(
        node,
        world.runtime.did(),
        std::slice::from_ref(&agent_identity),
    )
    .await
    .unwrap();
    assert_eq!(recovery.scanned, 1);
    assert_eq!(recovery.repaired, 1);
    assert_eq!(recovery.failed, 0);
    if scheduled {
        let wake = RequestLifecycle::redrive_failed_background_wakeups_with_identities(
            node,
            world.runtime.did(),
            std::slice::from_ref(&agent_identity),
        )
        .await
        .unwrap();
        assert_eq!(wake.scanned, 1);
        assert_eq!(wake.ineligible, 1);
    }
}

#[tokio::test]
async fn generated_tool_control_receipts_require_selected_agent_and_exact_intent() {
    use gents_protocol::process_control::{
        ProcessControlOutcome, ToolControlReceipt, ToolControlReceiptOutcome,
    };
    let home = tempfile::tempdir().unwrap();
    let agent = KeyIdentity::load_or_create(home.path().join("agent.key"), None).unwrap();
    let other = KeyIdentity::load_or_create(home.path().join("other.key"), None).unwrap();
    let expected = ToolControlReceipt {
        version: 1,
        intent_doc_id: "intent".into(),
        client_did: "client".into(),
        agent_did: agent.did().into(),
        node_did: "node".into(),
        session_id: "session".into(),
        session_doc_id: "session-doc".into(),
        request_doc_id: "request-doc".into(),
        tool_call_id: "call".into(),
        tool_call_doc_id: "doc".into(),
        action: "process_stop".into(),
        outcome: ToolControlReceiptOutcome::Process(ProcessControlOutcome::Cancelled {
            live_execution_cancelled: false,
        }),
        signature: Vec::new(),
    };
    for case in gents_lean_vocab::families::native_refinement_client_process_receipt::cases() {
        let input = &case.input;
        let mut receipt = expected.clone();
        receipt.intent_doc_id = input.intent_doc.clone();
        receipt.client_did = input.author.clone();
        receipt.agent_did = if input.agent == "agent" {
            agent.did()
        } else {
            other.did()
        }
        .into();
        receipt.tool_call_id = input.call.clone();
        receipt.tool_call_doc_id = input.document.clone();
        let signer = if input.agent == "agent" {
            &agent
        } else {
            &other
        };
        let outcome = |result: &str| {
            if result == "applied" {
                ToolControlReceiptOutcome::Process(ProcessControlOutcome::Cancelled {
                    live_execution_cancelled: false,
                })
            } else {
                ToolControlReceiptOutcome::Unconfirmed
            }
        };
        receipt.outcome = outcome(&input.signed_result);
        receipt.signature = signer
            .sign(&receipt.signing_payload().unwrap())
            .await
            .unwrap();
        receipt.outcome = outcome(&input.observed_result);
        if !input.signature_valid {
            receipt.signature[0] ^= 1;
        }
        let accepted = receipt.validate_for(&expected).is_ok()
            && agent
                .verify(
                    &expected.agent_did,
                    &receipt.signing_payload().unwrap(),
                    &receipt.signature,
                )
                .await
                .unwrap_or(false);
        case.check(accepted);
    }
    let mut signed = expected.clone();
    signed.signature = agent
        .sign(&signed.signing_payload().unwrap())
        .await
        .unwrap();
    signed.outcome = ToolControlReceiptOutcome::Process(ProcessControlOutcome::Lost);
    assert!(
        !agent
            .verify(
                &expected.agent_did,
                &signed.signing_payload().unwrap(),
                &signed.signature
            )
            .await
            .unwrap_or(false),
        "a result cannot be changed after signing"
    );
}

#[tokio::test]
async fn active_client_steer_rechecks_its_own_author_at_atomic_consumption() {
    let home = tempfile::tempdir().unwrap();
    let user = KeyIdentity::load_or_create(home.path().join("user.key"), None).unwrap();
    let collaborator =
        KeyIdentity::load_or_create(home.path().join("collaborator.key"), None).unwrap();
    let (db, runtime) = serve_client_node(
        "active-client-steer-rechecks-its-own-aut",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let runtime: Arc<dyn NodeIdentity> = Arc::new(runtime);
    let node = db.node.clone();
    grant_operator_user(&node, runtime.as_ref(), &user).await;
    let binding = gents_store::identity::provision_agent_identity(runtime.as_ref()).unwrap();
    let agent = gents_store::identity::resolve_agent_identity(runtime.as_ref(), &binding).unwrap();
    let admin = ConfigAccess::local_as(node.clone(), runtime.did().parse().unwrap());
    for did in [collaborator.did(), agent.did()] {
        for permission in [
            NodePermission::CollectionGet,
            NodePermission::DocumentRead,
            NodePermission::CollectionGet,
            NodePermission::DocumentUpdate,
            NodePermission::DacRelationAdd,
        ] {
            admin
                .grant_node_permission(&did.parse().unwrap(), permission)
                .await
                .unwrap();
        }
    }
    let client = |identity: &KeyIdentity| db.client_access(identity);
    let owner = client(&user);
    seed_owned_agent(&owner, runtime.did(), &binding).await;
    let agent_doc = owner.execute("{ Agent { _docID } }").await.unwrap()["data"]["Agent"][0]
        ["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    owner
        .add_actor_relationship(
            "Agent",
            &agent_doc,
            "session_writer",
            &collaborator.did().parse().unwrap(),
        )
        .await
        .unwrap();
    let user_access = client(&user);
    let collaborator_access = client(&collaborator);
    let mut head = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "steer-head",
        RequestScope::new(runtime.did(), user.did(), agent.did(), "steer-session"),
        "head",
        "interactive",
        "2026-11-04T00:00:00Z",
        AgentRequestAdmissionRecord::client(user.did()),
    );
    head.session_doc_id = Some(reserve_goal_session(&user_access, &head).await.unwrap());
    // A steering author holds update on the session it steers, as the modeled
    // committed claim requires of the immediate signer.
    owner
        .add_actor_relationship(
            "AgentSession",
            head.session_doc_id.as_deref().unwrap(),
            "working_agent",
            &collaborator.did().parse().unwrap(),
        )
        .await
        .unwrap();
    head.agent_doc_id = Some(agent_doc);
    crate::request::admission::sign_agent_request_create(&user, &mut head)
        .await
        .unwrap();
    submit_client_request(&user_access, &head).await.unwrap();
    let agent_access = ConfigAccess::local_managed_agent(
        node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let query = format!(
        "{{ AgentRequest(filter: {{ request_id: {{ _eq: \"steer-head\" }} }}) {{ {} }} }}",
        gents_protocol::request_admission::SIGNED_REQUEST_FIELDS
    );
    let response = agent_access.execute(&query).await.unwrap();
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
    let request = AgentRequest::try_from(row).unwrap();
    let (_owner, authority) = gents_p2p::p2p_reconcile::enrollment_authority_channel();
    let verifier = AgentRequestAdmissionVerifier::new(node.clone(), runtime.clone(), authority);
    let (request, admission) = verifier
        .verify_fresh_for_claim_with_identity(&request, agent.as_ref())
        .await
        .unwrap();
    assert!(admission.is_some());
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        request,
        60,
        ExecutionOrigin::Interactive,
        String::new(),
    );
    lifecycle.bind_managed_agent(agent.as_ref()).unwrap();
    lifecycle.set_client_claim_admission(admission);
    lifecycle.set_steering_admission(verifier.clone(), agent.clone());
    assert!(matches!(
        lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    ));
    let writer = crate::request::streaming::DefraStreamWriter::new_managed(
        node.clone(),
        agent.as_ref(),
        std::time::Duration::ZERO,
    )
    .unwrap();
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    let mut steer = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "steer-input",
        RequestScope::new(runtime.did(), user.did(), agent.did(), "steer-session"),
        "new signed input",
        "interactive",
        "2026-11-04T00:00:01Z",
        AgentRequestAdmissionRecord::client(collaborator.did()),
    );
    steer.session_doc_id = head.session_doc_id.clone();
    steer.agent_doc_id = head.agent_doc_id.clone();
    steer.input.queue = Some(gents_protocol::request_input::RequestQueue {
        source: gents_protocol::request_input::QueueSource::User,
        delivery: gents_protocol::request_input::QueueDelivery::Steer,
        policy: gents_protocol::request_input::QueuePolicy::Append,
        position: None,
        key: None,
        queued_after_request_id: None,
        interrupted_request_id: None,
        background_completion_wake_version: None,
    });
    crate::request::admission::sign_agent_request_create(&collaborator, &mut steer)
        .await
        .unwrap();
    submit_client_request(&collaborator_access, &steer)
        .await
        .unwrap();
    let snapshot = crate::request::lifecycle::queue::steering_snapshot(
        &node,
        lifecycle.request(),
        &verifier,
        Some(agent.as_ref()),
    )
    .await
    .unwrap();
    assert_eq!(snapshot.inputs.len(), 1);
    let input = &snapshot.inputs[0];
    let before = agent_access
        .execute("{ AgentMessage { _docID } }")
        .await
        .unwrap();
    admin
        .revoke_node_permission(
            &collaborator.did().parse().unwrap(),
            NodePermission::DocumentUpdate,
        )
        .await
        .unwrap();
    let blocked = crate::request::lifecycle::queue::steering_snapshot(
        &node,
        lifecycle.request(),
        &verifier,
        Some(agent.as_ref()),
    )
    .await
    .unwrap();
    assert!(
        blocked.inputs.is_empty(),
        "a revoked author is not repeatedly selected"
    );
    assert!(writer
        .publish_authored_message(&lifecycle, &input.key, &input.message)
        .await
        .is_err());
    let after = agent_access
        .execute("{ AgentMessage { _docID } }")
        .await
        .unwrap();
    assert_eq!(
        before["data"]["AgentMessage"], after["data"]["AgentMessage"],
        "denied consumption rolls back publication"
    );
    let pending = agent_access.execute("{ AgentRequest(filter: { request_id: { _eq: \"steer-input\" } }) { lifecycle_state } }").await.unwrap();
    assert_eq!(
        pending["data"]["AgentRequest"][0]["lifecycle_state"],
        "pending"
    );
    admin
        .grant_node_permission(
            &collaborator.did().parse().unwrap(),
            NodePermission::DocumentUpdate,
        )
        .await
        .unwrap();
    admin
        .revoke_node_permission(&user.did().parse().unwrap(), NodePermission::DocumentUpdate)
        .await
        .unwrap();
    writer
        .publish_authored_message(&lifecycle, &input.key, &input.message)
        .await
        .unwrap();
    let result = agent_access.execute("{ AgentRequest(filter: { request_id: { _eq: \"steer-input\" } }) { lifecycle_state superseded_by_request_doc_id } }").await.unwrap();
    assert_eq!(
        result["data"]["AgentRequest"][0]["lifecycle_state"],
        "superseded"
    );
    assert_eq!(
        result["data"]["AgentRequest"][0]["superseded_by_request_doc_id"],
        lifecycle.request().doc_id
    );
    admin
        .revoke_node_permission(
            &collaborator.did().parse().unwrap(),
            NodePermission::DocumentUpdate,
        )
        .await
        .unwrap();
    writer
        .publish_authored_message(&lifecycle, &input.key, &input.message)
        .await
        .unwrap();
    drop(lifecycle);
    node.shutdown().await.unwrap();
}
