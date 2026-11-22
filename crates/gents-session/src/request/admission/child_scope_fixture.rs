use gents_lean_vocab::families::session_child_child_scope_binding::ChildScopeInput;
use gents_protocol::output::{OutputSource, OutputWriter};
use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose, RequestScope,
};
use gents_store::config_client::session_children::{
    agent_session_child_binding_in_txn, session_child_binding_for_request_in_txn,
    SessionChildSource,
};
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn, NodePermission};
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::{KeyIdentity, NodeIdentity};
use serde_json::{json, Value};
use std::sync::Arc;

/// One node holding a session, its signed parent and auxiliary requests and a
/// decoy request, written from a modeled child-scope case. Model symbols name
/// fixture values: `node` and `user` the runtime's own key, `agent` the
/// selected Agent, `other` a second Agent of the node or a decoy document.
pub(super) struct ChildScopeWorld {
    pub node: Arc<defra_node::Node>,
    root: KeyIdentity,
    agent: Arc<dyn NodeIdentity>,
    foreign: Arc<dyn NodeIdentity>,
    agent_access: ConfigAccess,
    foreign_access: ConfigAccess,
    reader_access: ConfigAccess,
    stranger_access: ConfigAccess,
    session_id: String,
    session_doc: String,
    parent_id: String,
    parent_doc: String,
    aux_id: String,
    aux_doc: String,
    decoy_doc: String,
    _home: tempfile::TempDir,
}

pub(super) struct RepairObservation {
    pub parent_granted: bool,
    pub parent_linked: bool,
}

async fn create_document(access: &ConfigAccess, collection: &str, input: Value) -> String {
    let input = gents_protocol::graphql::graphql_input_literal(&input).unwrap();
    let result = access
        .write(
            "test.child_scope.fixture",
            &format!("mutation {{ create_{collection}(input: {input}) {{ _docID }} }}"),
        )
        .await
        .unwrap();
    gents_store::graphql::created_doc_id(&result, collection).unwrap()
}

/// Signs exactly the fields production verifies, so an absent requester is
/// signed as absent; an invalid signature covers different content.
async fn write_request(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    mut create: AgentRequestCreate,
    requester: Option<String>,
    signature_valid: bool,
) -> String {
    let placeholder = signer.did().to_owned();
    create.requester_did = requester.clone().unwrap_or_else(|| placeholder.clone());
    let payload = {
        let mut fields = create.signing_fields();
        fields.requester_did = requester.as_deref();
        create.admission.signing_payload(&fields)
    };
    create.admission.signature = signer.sign(&payload).await.unwrap();
    if !signature_valid {
        create.content.push_str(" tampered");
    }
    let mut mutation = create.graphql_mutation().unwrap();
    if requester.is_none() {
        mutation = mutation.replacen(
            &format!(
                "requester_did: \"{}\"",
                escape_graphql_string(&placeholder)
            ),
            "requester_did: null",
            1,
        );
    }
    let response = access
        .write("test.child_scope.request", &mutation)
        .await
        .unwrap();
    gents_store::graphql::created_doc_id(&response, "AgentRequest").unwrap()
}

fn absent_doc() -> String {
    format!("bae-{}", uuid::Uuid::new_v4())
}

fn capture_kind(source: &str) -> CaptureScopeKind {
    match source {
        "title" => CaptureScopeKind::Title,
        "access-review" => CaptureScopeKind::AccessReview,
        "ordinary" => CaptureScopeKind::Inference,
        value => panic!("unknown modeled source {value}"),
    }
}

impl ChildScopeWorld {
    pub async fn build(input: &ChildScopeInput) -> Self {
        let b = &input.binding;
        let home = tempfile::tempdir().unwrap();
        let node_db = gents_inference::test_support::test_db(
            "child-scope-",
            crate::test_support::no_lifecycle_plugins(),
        )
        .await;
        let node = node_db.node.clone();
        let root = gents_store::test_support::held_runtime_identity(node_db.node_identity.did());
        let root_access = ConfigAccess::local_as(node.clone(), root.did().parse().unwrap());
        let reader = KeyIdentity::load_or_create(home.path().join("reader.key"), None).unwrap();
        let stranger =
            KeyIdentity::load_or_create(home.path().join("stranger.key"), None).unwrap();
        let binding = gents_store::identity::provision_agent_identity(&root).unwrap();
        let agent = gents_store::identity::resolve_agent_identity(&root, &binding).unwrap();
        let foreign_binding = gents_store::identity::provision_agent_identity(&root).unwrap();
        let foreign =
            gents_store::identity::resolve_agent_identity(&root, &foreign_binding).unwrap();
        for (actor, may_relate) in [(agent.did(), input.grant_ok), (foreign.did(), true)] {
            for permission in [
                NodePermission::DocumentRead,
                NodePermission::CollectionGet,
                NodePermission::DocumentUpdate,
                NodePermission::DacRelationAdd,
                NodePermission::DacRelationDelete,
            ] {
                if matches!(permission, NodePermission::DacRelationAdd) && !may_relate {
                    continue;
                }
                root_access
                    .grant_node_permission(&actor.parse().unwrap(), permission)
                    .await
                    .unwrap();
            }
        }
        for observer in [reader.did(), stranger.did()] {
            root_access
                .grant_node_permission(&observer.parse().unwrap(), NodePermission::DocumentRead)
                .await
                .unwrap();
        }
        let agent_access = ConfigAccess::local_managed_agent(
            node.clone(),
            agent.did().parse().unwrap(),
            agent.as_ref(),
        )
        .unwrap();
        let foreign_access = ConfigAccess::local_managed_agent(
            node.clone(),
            foreign.did().parse().unwrap(),
            foreign.as_ref(),
        )
        .unwrap();
        let reader_access = ConfigAccess::local_as(node.clone(), reader.did().parse().unwrap());
        let stranger_access =
            ConfigAccess::local_as(node.clone(), stranger.did().parse().unwrap());
        let mut world = Self {
            node,
            root,
            agent,
            foreign,
            agent_access,
            foreign_access,
            reader_access,
            stranger_access,
            session_id: format!("session-{}", uuid::Uuid::new_v4()),
            session_doc: String::new(),
            parent_id: format!("parent-{}", uuid::Uuid::new_v4()),
            parent_doc: absent_doc(),
            aux_id: format!("aux-{}", uuid::Uuid::new_v4()),
            aux_doc: absent_doc(),
            decoy_doc: String::new(),
            _home: home,
        };
        let agent_doc = create_document(
            &root_access,
            "Agent",
            json!({"node_did":world.root.did(), "agent_did":world.agent.did(),
                "creation_nonce":binding.creation_nonce}),
        )
        .await;
        world.session_doc = create_document(
            &root_access,
            "AgentSession",
            json!({"session_id":world.session(&b.session_id), "node_did":world.did(&b.session_node),
                "agent_did":world.did(&b.session_agent),
                "requester_did":world.requester(b.session_requester.as_deref()),
                "created_at":"2026-10-21T00:00:00Z"}),
        )
        .await;
        for (relation, actor) in [
            ("working_agent", world.agent.did()),
            ("reader", reader.did()),
        ] {
            root_access
                .add_actor_relationship(
                    "AgentSession",
                    &world.session_doc,
                    relation,
                    &actor.parse().unwrap(),
                )
                .await
                .unwrap();
        }
        let parent = |world: &Self,
                      request_id: String,
                      purpose: &str,
                      node: &str,
                      agent: &str,
                      session: &str| {
            let mut create = AgentRequestCreate::base(
                RequestPurpose::try_from(purpose).unwrap(),
                request_id,
                RequestScope::new(
                    world.did(node),
                    world.root.did(),
                    world.did(agent),
                    world.session(session),
                ),
                "parent",
                "interactive",
                "2026-10-21T00:00:00Z",
                AgentRequestAdmissionRecord::client(world.root.did()),
            );
            create.session_doc_id = Some(world.session_doc.clone());
            create.agent_doc_id = Some(agent_doc.clone());
            create
        };
        let decoy = parent(
            &world,
            format!("decoy-{}", uuid::Uuid::new_v4()),
            "normal",
            "node",
            "agent",
            "session",
        );
        world.decoy_doc = write_request(
            &root_access,
            &world.root,
            decoy,
            Some(world.root.did().to_owned()),
            true,
        )
        .await;
        if b.parent_present {
            let create = parent(
                &world,
                world.request_id(&b.parent_id),
                &b.parent_purpose,
                &b.parent_node,
                &b.parent_agent,
                &b.parent_session,
            );
            let requester = world.requester(b.parent_requester.as_deref());
            world.parent_doc = write_request(
                &root_access,
                &world.root,
                create,
                requester,
                b.parent_signature_valid,
            )
            .await;
        }
        let mut granted = vec![world.decoy_doc.clone()];
        if b.parent_present {
            granted.push(world.parent_doc.clone());
        }
        for doc in granted {
            root_access
                .add_actor_relationship(
                    "AgentRequest",
                    &doc,
                    "working_agent",
                    &world.agent.did().parse().unwrap(),
                )
                .await
                .unwrap();
        }
        if b.auxiliary_present {
            let purpose = RequestPurpose::try_from(b.auxiliary_purpose.as_str()).unwrap();
            let signer_did = world.did(&b.auxiliary_agent);
            let mut create = AgentRequestCreate::base(
                purpose,
                world.request_id(&b.auxiliary_id),
                RequestScope::new(
                    world.did(&b.auxiliary_node),
                    signer_did.clone(),
                    signer_did.clone(),
                    world.session(&b.auxiliary_session),
                ),
                "audit",
                "interactive",
                "2026-10-21T00:00:00Z",
                AgentRequestAdmissionRecord::runtime_local_control(
                    &signer_did,
                    world.request_id(&b.source_parent_id),
                ),
            );
            create.caused_by_parent_request_id = Some(world.request_id(&b.linked_parent_id));
            create.caused_by_parent_request_doc_id = Some(world.doc(&b.linked_parent_doc));
            create.valid_until = (purpose == RequestPurpose::AccessReview)
                .then(|| "2030-01-01T00:00:00Z".to_owned());
            if !b.auxiliary_shape_valid {
                create.request_hop = 1;
            }
            let (access, signer): (&ConfigAccess, &dyn NodeIdentity) =
                if signer_did == world.agent.did() {
                    (&world.agent_access, world.agent.as_ref())
                } else {
                    (&world.foreign_access, world.foreign.as_ref())
                };
            let requester = world.requester(b.auxiliary_requester.as_deref());
            world.aux_doc =
                write_request(access, signer, create, requester, b.auxiliary_signature_valid)
                    .await;
        }
        world
    }

    fn did(&self, symbol: &str) -> String {
        match symbol {
            "node" | "user" => self.root.did().to_owned(),
            "agent" => self.agent.did().to_owned(),
            "other" => self.foreign.did().to_owned(),
            value => panic!("unknown modeled identity {value}"),
        }
    }

    fn requester(&self, symbol: Option<&str>) -> Option<String> {
        symbol.map(|symbol| self.did(symbol))
    }

    fn session(&self, symbol: &str) -> String {
        match symbol {
            "session" => self.session_id.clone(),
            "other" => "foreign-session".to_owned(),
            value => panic!("unknown modeled session {value}"),
        }
    }

    fn request_id(&self, symbol: &str) -> String {
        match symbol {
            "parent-id" => self.parent_id.clone(),
            "aux-id" => self.aux_id.clone(),
            "other" => "foreign-request".to_owned(),
            value => panic!("unknown modeled request {value}"),
        }
    }

    fn doc(&self, symbol: &str) -> String {
        match symbol {
            "session-doc" => self.session_doc.clone(),
            "parent-doc" => self.parent_doc.clone(),
            "aux-doc" => self.aux_doc.clone(),
            "other" => self.decoy_doc.clone(),
            value => panic!("unknown modeled document {value}"),
        }
    }

    fn access(&self, symbol: Option<&str>) -> ConfigAccess {
        match symbol {
            Some("agent") => self.agent_access.clone(),
            Some("other") => self.foreign_access.clone(),
            None => ConfigAccess::local(self.node.clone()),
            Some(value) => panic!("unknown modeled actor {value}"),
        }
    }

    fn output_source(&self, input: &ChildScopeInput) -> OutputSource {
        OutputSource::ProviderTurn {
            scope: CaptureScope {
                kind: capture_kind(&input.binding.source),
                seq: 1,
            },
            turn_index: 0,
            attempt: 0,
        }
    }

    /// Asks production's binding for the modeled child as the modeled actor;
    /// the child is bound when the binding names its own physical session and
    /// selected Agent.
    pub async fn bound(&self, input: &ChildScopeInput) -> bool {
        let b = &input.binding;
        let node_did = self.did(&b.child_node);
        let session_id = self.session(&b.child_session);
        let requester = self.requester(b.child_requester.as_deref());
        let request_doc = self.doc(&b.child_request_doc);
        let output = self.output_source(input);
        let scope = CaptureScope {
            kind: capture_kind(&b.source),
            seq: 1,
        };
        let collection = b.collection.as_str();
        let actual = self
            .access(Some(b.actor.as_str()))
            .transact_readonly("test.child_scope.binding", |txn| {
                let (node_did, session_id, requester, request_doc) =
                    (&node_did, &session_id, &requester, &request_doc);
                let (output, scope) = (&output, &scope);
                Box::pin(async move {
                    let source = match collection {
                        "AgentOutputSegment" => SessionChildSource::Output(output),
                        "RenderedRequest" => SessionChildSource::Capture(scope),
                        "AgentMessage" => {
                            return agent_session_child_binding_in_txn(
                                txn,
                                node_did,
                                session_id,
                                requester.as_deref(),
                            )
                            .await
                        }
                        value => panic!("unrepresented collection {value}"),
                    };
                    session_child_binding_for_request_in_txn(
                        txn,
                        node_did,
                        session_id,
                        requester.as_deref(),
                        request_doc,
                        source,
                    )
                    .await
                })
            })
            .await;
        matches!(actual, Ok((physical, selected))
            if physical == self.doc(&b.child_session_doc) && selected == self.did(&b.child_agent))
    }

    fn child_input(&self, input: &ChildScopeInput) -> Value {
        let b = &input.binding;
        let common = json!({
            "node_did": self.did(&b.child_node),
            "session_id": self.session(&b.child_session),
            "session_doc_id": self.doc(&b.child_session_doc),
            "agent_did": self.did(&b.child_agent),
            "requester_did": self.requester(b.child_requester.as_deref()),
            "created_at": "2026-10-21T00:00:00Z",
        });
        let mut row = common.as_object().unwrap().clone();
        let creator = input.creator.as_deref().map(|symbol| self.did(symbol));
        match b.collection.as_str() {
            "AgentOutputSegment" => {
                row.insert("creator_did".into(), creator.into());
                row.insert("parent_linked".into(), false.into());
                row.insert("request_doc_id".into(), self.doc(&b.child_request_doc).into());
                row.insert(
                    "source".into(),
                    serde_json::to_value(self.output_source(input)).unwrap(),
                );
                row.insert("ordinal".into(), 0.into());
                row.insert(
                    "writer".into(),
                    serde_json::to_value(OutputWriter::RequestExecution {
                        execution_generation: "fixture".into(),
                    })
                    .unwrap(),
                );
                row.insert("payload".into(), "audit".into());
            }
            "AgentMessage" => {
                row.insert("creator_did".into(), creator.into());
                row.insert("parent_linked".into(), false.into());
                row.insert("message_key".into(), "child-scope".into());
                row.insert("sequence".into(), 1.into());
                row.insert("role".into(), "assistant".into());
            }
            "RenderedRequest" => {
                let kind = capture_kind(&b.source);
                row.insert("capture_key".into(), "child-scope".into());
                row.insert("request_doc_id".into(), self.doc(&b.child_request_doc).into());
                row.insert("request_commit_cid".into(), "".into());
                row.insert("request_id".into(), self.aux_id.clone().into());
                row.insert("capture_scope".into(), format!("{kind}.1").into());
                row.insert("turn_index".into(), 0.into());
                row.insert("attempt".into(), 0.into());
                row.insert("capture_version".into(), 1.into());
                row.insert("model_name".into(), "fixture".into());
                row.insert("source".into(), "openai-chat".into());
                row.insert("request_json".into(), "{}".into());
                row.insert("provenance_json".into(), "{}".into());
            }
            value => panic!("unrepresented collection {value}"),
        }
        Value::Object(row)
    }

    /// Writes the modeled child as its native owner, completes and revokes its
    /// link when the case starts there, runs production's detached repair as
    /// the authenticated repairer, and observes the outcome. A child is granted
    /// when the session's reader can read it and a node member without
    /// relationships cannot.
    pub async fn repair(&self, input: &ChildScopeInput) -> RepairObservation {
        let collection = input.binding.collection.clone();
        let creator = self.access(input.native_owner.as_deref());
        let row = self.child_input(input);
        let create = creator.transact_write("test.child_scope.create", |txn| {
            let (collection, row) = (&collection, &row);
            Box::pin(async move {
                let response = txn
                    .execute_with_variables(
                        &format!(
                            "mutation($input: {collection}MutationInputArg!) {{ create_{collection}(input: $input) {{ _docID }} }}"
                        ),
                        &json!({"input": row}),
                    )
                    .await?;
                Ok::<_, anyhow::Error>(gents_store::graphql::created_doc_id(
                    &response, collection,
                )?)
            })
        });
        let child = if input.committed {
            Some(create.await.unwrap())
        } else {
            let (result, _) =
                ConfigApplyTxn::with_successful_mutation_failure_at(Some(1), create).await;
            assert!(result.is_err(), "{}", input.name);
            None
        };
        if let Some(child) = child.as_deref().filter(|_| input.initial_parent_linked) {
            gents_store::test_support::relationships::resource_in_txn(
                &creator,
                &collection,
                child,
                "parent",
                "sessions",
                &self.session_doc,
                false,
            )
            .await
            .unwrap();
            creator
                .write(
                    "test.child_scope.mark",
                    &format!(
                        r#"mutation {{ update_{collection}(docID: "{}", input: {{ parent_linked: true }}) {{ _docID }} }}"#,
                        escape_graphql_string(child)
                    ),
                )
                .await
                .unwrap();
            if !input.initial_parent_granted {
                gents_store::test_support::relationships::resource_in_txn(
                    &creator,
                    &collection,
                    child,
                    "parent",
                    "sessions",
                    &self.session_doc,
                    true,
                )
                .await
                .unwrap();
            }
        }
        let authenticated = input
            .authenticated
            .as_deref()
            .expect("repair runs as an authenticated actor");
        let repairer = self.access(Some(authenticated));
        let repairer_did: identity::Did = self.did(authenticated).parse().unwrap();
        let repair = gents_store::config_client::session_children::recover_agent_session_child_relationships_with_access(
            &repairer,
            self.root.did(),
            &repairer_did,
        );
        let result = if input.mark_ok {
            repair.await
        } else {
            ConfigApplyTxn::with_successful_mutation_failure_at(Some(1), repair)
                .await
                .0
        };
        if let Err(error) = result {
            tracing::debug!(case = %input.name, %error, "child repair refused");
        }
        let marker = if collection == "RenderedRequest" {
            ""
        } else {
            "parent_linked"
        };
        let query = format!("{{ {collection} {{ _docID {marker} }} }}");
        let rows = |response: Value| {
            response["data"][collection.as_str()]
                .as_array()
                .cloned()
                .unwrap_or_default()
        };
        let readable = rows(self.reader_access.execute(&query).await.unwrap());
        let public = rows(self.stranger_access.execute(&query).await.unwrap());
        let owned = rows(creator.execute(&query).await.unwrap());
        RepairObservation {
            parent_granted: !readable.is_empty() && public.is_empty(),
            parent_linked: owned
                .iter()
                .any(|row| row["parent_linked"].as_bool() == Some(true)),
        }
    }
}
