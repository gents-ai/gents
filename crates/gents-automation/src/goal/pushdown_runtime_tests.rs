use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
use std::sync::Arc;
use std::time::Duration;

use crate::goal::{
    seed_goal_from_access, session_token_usage, CreateGoalForSessionError,
    CreateGoalForSessionOutcome,
};
use crate::test_support::doubles::MockUpdateSubscriptionSource;
use crate::{
    triggers::engine::goal_source::GoalSource, triggers::engine::FireIntent,
    triggers::engine::TriggerSource, triggers::runtime_snapshot::ActiveRuntimeSnapshot,
};
use gents_inference::backends::provider::BackendProviderKind;
use gents_inference::resolved::ResolvedAgent;
use gents_protocol::goal_status::GoalStatus;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose, RequestScope,
};
use gents_session::request::lifecycle::{
    build_signed_client_request, submit_client_request, ExecutionOrigin, RequestIdentity,
    RequestSigner, RequestSpec,
};
use gents_session::test_support::AgentHome;
use gents_store::config_client::{ConfigAccess, NodePermission};
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::{NodeIdentity, RuntimeNode};
use gents_store::subscription_source::UpdateSubscriptionSource;
use serde::Deserialize;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

const RESCAN: Duration = Duration::from_millis(20);
const SETTLED_PASSES: usize = 8;
const RENDERED_LIMIT: &str =
    "provider usage limit reached (resets at 2026-07-15T05:00:00Z): capped";

struct GoalFixture {
    home: AgentHome,
}

impl GoalFixture {
    async fn start(name: &str) -> Self {
        let home = AgentHome::start_with(name, crate::lifecycle_hooks::composition()).await;
        let admin =
            ConfigAccess::local_as(home.node.clone(), home.node_identity.did().parse().unwrap());
        for actor in [home.user.did(), home.agent_did()] {
            for permission in [
                NodePermission::DocumentDelete,
                NodePermission::CollectionGet,
                NodePermission::DacRelationDelete,
            ] {
                admin
                    .grant_node_permission(&actor.parse().unwrap(), permission)
                    .await
                    .unwrap();
            }
        }
        Self { home }
    }

    fn did(&self) -> &str {
        self.home.node_identity.did()
    }

    fn agent_did(&self) -> &str {
        self.home.agent_did()
    }

    fn agent_access(&self) -> &ConfigAccess {
        &self.home.agent_access
    }

    fn client_access(&self) -> &ConfigAccess {
        &self.home.user_access
    }

    async fn reserve_session(&self, session: &str) -> String {
        let create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            format!("goal-session-reservation-{session}"),
            RequestScope::new(self.did(), self.home.user.did(), self.agent_did(), session),
            "reserve session",
            "interactive",
            "2026-07-14T00:00:00Z",
            AgentRequestAdmissionRecord::client(self.home.user.did()),
        );
        gents_session::request::lifecycle::reserve_goal_session(self.client_access(), &create)
            .await
            .expect("reserve user-owned goal session")
    }

    async fn clear_goal(&self, session: &str) {
        let escaped = escape_graphql_string(session);
        let del_goal = format!(
            r#"mutation {{ delete_Goal(filter: {{ session_id: {{ _eq: "{escaped}" }} }}) {{ _docID }} }}"#
        );
        let del_claim = format!(
            r#"mutation {{ delete_GoalCreationClaim(filter: {{ session_id: {{ _eq: "{escaped}" }} }}) {{ _docID }} }}"#
        );
        for access in [self.agent_access(), self.client_access()] {
            let _ = access.write("test.goal_clear", &del_goal).await;
            let _ = access.write("test.goal_claim_clear", &del_claim).await;
        }
    }

    async fn load_goal(
        &self,
        access: &ConfigAccess,
        session: &str,
    ) -> Option<crate::goal::GoalDocument> {
        let did = self.did();
        access
            .transact_readonly("test.load_client_goal", |txn| {
                Box::pin(crate::goal::load_canonical_goal_in_txn(txn, did, session))
            })
            .await
            .unwrap()
    }

    async fn seed_request(&self, session: &str, request_id: &str, state: &str) -> String {
        self.seed_request_at(session, request_id, state, "2026-07-15T00:00:00Z")
            .await
    }

    async fn seed_request_at(
        &self,
        session: &str,
        request_id: &str,
        state: &str,
        created_at: &str,
    ) -> String {
        let spec = RequestSpec::new(
            RequestPurpose::Normal,
            RequestIdentity {
                node_did: self.did().into(),
                requester_did: Some(self.home.user.did().into()),
                agent_did: self.agent_did().into(),
                session_id: session.into(),
                request_id: request_id.into(),
                content: "completed baseline request".into(),
                execution_origin: ExecutionOrigin::Interactive,
                created_at: created_at.into(),
            },
            AgentRequestAdmissionRecord::client(self.home.user.did()),
        );
        let create = build_signed_client_request(
            self.client_access(),
            spec,
            RequestSigner::Identity(&self.home.user),
        )
        .await
        .unwrap();
        submit_client_request(self.client_access(), &create)
            .await
            .unwrap();
        let request = escape_graphql_string(request_id);
        let rows = self
            .client_access()
            .execute(&format!(
                r#"{{AgentRequest(filter: {{request_id: {{_eq: "{request}"}}}}) {{_docID}}}}"#
            ))
            .await
            .unwrap();
        let doc = rows["data"]["AgentRequest"][0]["_docID"]
            .as_str()
            .unwrap()
            .to_owned();
        set_request_state_as(self.agent_access(), &doc, state).await;
        doc
    }

    async fn seed_recovery_parent(&self, session: &str, request_id: &str, state: &str) -> String {
        self.reserve_session(session).await;
        let doc = self.seed_request(session, request_id, state).await;
        if state == "completed" {
            publish_owned_terminal_activity(
                self.agent_access(),
                self.home.agent_identity.as_ref(),
                &doc,
                "durable progress",
            )
            .await;
        }
        doc
    }

    async fn seed_usage(
        &self,
        session: &str,
        request_id: &str,
        request_doc: &str,
        call_id: &str,
        tokens: (i64, i64, i64),
    ) {
        let (prompt_tokens, completion_tokens, cached_input_tokens) = tokens;
        let node_did = self.did();
        let agent_did = self.agent_did();
        self.agent_access().transact_write("test.goal_baseline_usage", |txn| {
            Box::pin(async move {
                let rows = txn.execute(&format!(
                    r#"{{AgentSession(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}}}) {{_docID}}}}"#,
                    escape_graphql_string(node_did),
                    escape_graphql_string(session)
                )).await?;
                let session_doc = rows["data"]["AgentSession"][0]["_docID"]
                    .as_str()
                    .expect("baseline session document");
                let input = serde_json::json!({
                    "call_id": call_id, "runtime_instance_id": "goal-baseline-test",
                    "request_id": request_id, "request_doc_id": request_doc,
                    "session_id": session, "call_seq": 1, "backend_id": "backend-test",
                    "agent_did": agent_did, "node_did": node_did, "call_kind": "inference",
                    "attempt": 1, "call_state": "completed",
                    "queued_at": "2026-07-15T00:00:00Z", "started_at": "2026-07-15T00:00:00Z",
                    "ended_at": "2026-07-15T00:00:01Z", "priority": 0, "queue_depth_at_enqueue": 0,
                    "controller_generation": 1, "backend_config_fingerprint": "goal-baseline-test",
                    "prompt_tokens": prompt_tokens, "completion_tokens": completion_tokens,
                    "cached_input_tokens": cached_input_tokens,
                });
                let created = txn.execute_with_variables(
                    "mutation($input: InferenceCallMutationInputArg!) { add_InferenceCall(input: $input) { _docID } }",
                    &serde_json::json!({"input": input}),
                ).await?;
                let doc = gents_store::graphql::created_doc_id(&created, "InferenceCall")?;
                txn.add_resource_relationship("InferenceCall", &doc, "parent", "sessions", session_doc).await?;
                Ok::<_, anyhow::Error>(())
            })
        }).await.expect("seed agent-owned usage linked to the session");
    }

    async fn seed_failed_call(
        &self,
        session: &str,
        request_id: &str,
        request_doc: &str,
        session_doc: &str,
        call: FailedInferenceCall<'_>,
    ) {
        let node_did = self.did();
        let agent_did = self.agent_did();
        let (_, provider_limit, provider_limit_resets_at) =
            crate::goal::blocked_turn::permit_recorded(call.failure_reason, chrono::Utc::now());
        let (provider_limit, provider_limit_resets_at) =
            (&provider_limit, &provider_limit_resets_at);
        self.agent_access().transact_write("test.goal_failed_call", |txn| {
            Box::pin(async move {
                let response = txn.execute_with_variables(
                    "mutation($input: InferenceCallMutationInputArg!) {add_InferenceCall(input: $input) {_docID}}",
                    &serde_json::json!({"input": {
                        "call_id": call.call_id, "node_did": node_did, "request_id": request_id,
                        "request_doc_id": request_doc, "session_id": session, "agent_did": agent_did,
                        "backend_id": format!("{agent_did}:backend"), "call_seq": 1,
                        "call_kind": call.call_kind, "attempt": call.attempt, "call_state": "failed",
                        "queued_at": "2026-07-15T00:00:00Z", "started_at": "2026-07-15T00:00:00Z",
                        "ended_at": call.ended_at, "failure_reason": call.failure_reason,
                        "provider_limit": provider_limit,
                        "provider_limit_resets_at": provider_limit_resets_at,
                    }}),
                ).await?;
                let doc = gents_store::graphql::created_doc_id(&response, "InferenceCall")?;
                txn.add_resource_relationship("InferenceCall", &doc, "parent", "sessions", session_doc).await?;
                Ok::<_, anyhow::Error>(())
            })
        }).await.unwrap();
    }

    async fn publish_readiness(
        &self,
        process_state: gents_protocol::row::NodeReadinessProcessState,
        behavior: Option<gents_protocol::row::AgentReadinessUnavailableReason>,
    ) {
        use gents_protocol::row::{
            AgentReadinessEntry, AgentReadinessState, NodeReadinessSnapshot,
            NODE_READINESS_FORMAT_VERSION,
        };
        let snapshot = NodeReadinessSnapshot {
            format_version: NODE_READINESS_FORMAT_VERSION,
            process_state,
            active_generation: 1,
            router_generation: 1,
            default_agent_did: self.agent_did().to_string(),
            agents: vec![AgentReadinessEntry {
                effective_inference: None,
                agent_did: self.agent_did().to_string(),
                state: if behavior.is_some() {
                    AgentReadinessState::Unavailable
                } else {
                    AgentReadinessState::Ready
                },
                reason: behavior,
            }],
        };
        let did = escape_graphql_string(self.did());
        let snapshot_json = escape_graphql_string(&serde_json::to_string(&snapshot).unwrap());
        let updated_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
        let access = ConfigAccess::local_as(self.home.node.clone(), self.did().parse().unwrap());
        let response = access
            .execute(&format!(
                r#"{{ Node(filter: {{node_did: {{_eq: "{did}"}}}}, limit: 2) {{_docID}}
                NodeReadiness(filter: {{node_did: {{_eq: "{did}"}}}}, limit: 2) {{_docID}} }}"#
            ))
            .await
            .unwrap();
        let parent = response["data"]["Node"][0]["_docID"]
            .as_str()
            .expect("visible readiness Node");
        let readiness = response["data"]["NodeReadiness"].as_array().unwrap();
        if let Some(existing) = readiness.first() {
            assert_eq!(readiness.len(), 1);
            let doc = escape_graphql_string(existing["_docID"].as_str().unwrap());
            let response = access
                .write(
                    "test.goal_readiness.update",
                    &format!(
                        r#"mutation {{update_NodeReadiness(docID: "{doc}", input: {{snapshot_json: "{snapshot_json}", updated_at: "{updated_at}"}}) {{_docID}}}}"#
                    ),
                )
                .await
                .unwrap();
            assert_eq!(
                response["data"]["update_NodeReadiness"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
        } else {
            let escaped_parent = escape_graphql_string(parent);
            let response = access
                .write(
                    "test.goal_readiness.create",
                    &format!(
                        r#"mutation {{create_NodeReadiness(input: {{node_did: "{did}", node_doc_id: "{escaped_parent}", parent_linked: false, snapshot_json: "{snapshot_json}", updated_at: "{updated_at}"}}) {{_docID}}}}"#
                    ),
                )
                .await
                .unwrap();
            let doc = gents_store::graphql::created_doc_id(&response, "NodeReadiness").unwrap();
            access
                .add_resource_relationship("NodeReadiness", &doc, "parent", "nodes", parent)
                .await
                .unwrap();
            let doc = escape_graphql_string(&doc);
            let response = access
                .write(
                    "test.goal_readiness.linked",
                    &format!(
                        r#"mutation {{update_NodeReadiness(docID: "{doc}", input: {{parent_linked: true}}) {{_docID}}}}"#
                    ),
                )
                .await
                .unwrap();
            assert_eq!(
                response["data"]["update_NodeReadiness"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
        }
    }

    fn agent_snapshot(&self) -> Arc<ActiveRuntimeSnapshot> {
        let node = Arc::new(RuntimeNode {
            node_did: self.did().to_string(),
            identity: self.home.node_identity.clone(),
            default_agent_did: self.agent_did().to_string(),
            display_name: None,
            enabled: true,
        });
        let agent = test_agent_with_identity(node.clone(), self.home.agent_identity.clone());
        let mut agents = RapidHashMap::new();
        agents.insert(self.agent_did().to_owned(), Arc::new(agent));
        Arc::new(ActiveRuntimeSnapshot {
            placement_basis: Default::default(),
            generation: 1,
            node: Some(node),
            local_did: self.did().to_string(),
            default_agent_did: self.agent_did().to_string(),
            tool_configs: Default::default(),
            agents,
            tool_surfaces: RapidHashMap::new(),
            backend_admission_configs: RapidHashMap::new(),
            unavailable_agents: RapidHashMap::new(),
            active_schedules: RapidHashMap::new(),
            unavailable_schedules: RapidHashSet::new(),
            active_event_triggers: RapidHashMap::new(),
            unavailable_event_triggers: RapidHashSet::new(),
            active_tasks: RapidHashMap::new(),
            dispatchers: RapidHashMap::new(),
            agent_executor_capacities: RapidHashMap::new(),
            agent_executor_queue_capacities: RapidHashMap::new(),
        })
    }

    fn unready_source(&self) -> (GoalSource, watch::Sender<Arc<ActiveRuntimeSnapshot>>) {
        let (tx, rx) = watch::channel(self.agent_snapshot());
        let subscriptions: Arc<dyn UpdateSubscriptionSource> =
            Arc::new(MockUpdateSubscriptionSource::new());
        (
            GoalSource::with_subscription_source(
                subscriptions,
                rx,
                self.home.node.clone(),
                CancellationToken::new(),
            )
            .with_rescan_interval(RESCAN),
            tx,
        )
    }

    async fn ready_source(&self) -> (GoalSource, watch::Sender<Arc<ActiveRuntimeSnapshot>>) {
        self.publish_readiness(gents_protocol::row::NodeReadinessProcessState::Ready, None)
            .await;
        self.unready_source()
    }

    async fn signed_goal_backed_request(
        &self,
        session: &str,
        request_id: &str,
    ) -> AgentRequestCreate {
        let did = self.did();
        let agent_did = self.agent_did();
        let mut create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            request_id,
            RequestScope::new(did, agent_did, agent_did, session),
            "Begin durable work",
            "interactive",
            "2026-07-15T00:00:00Z",
            AgentRequestAdmissionRecord::local_self(agent_did),
        );
        create.retry_key = Some(format!("goal-submit:{session}"));
        create.input.goal_assignment = Some(gents_protocol::request_input::GoalAssignmentInput {
            goal_id: gents_protocol::goal::deterministic_goal_id(did, session),
            root_request_doc_id: None,
        });
        create.input.queue = Some(gents_protocol::request_input::RequestQueue {
            source: gents_protocol::request_input::QueueSource::User,
            policy: gents_protocol::request_input::QueuePolicy::Append,
            delivery: gents_protocol::request_input::QueueDelivery::Queue,
            position: None,
            key: None,
            queued_after_request_id: None,
            interrupted_request_id: None,
            background_completion_wake_version: None,
        });
        create.session_doc_id = Some(
            gents_session::request::lifecycle::reserve_goal_session(self.agent_access(), &create)
                .await
                .expect("reserve Agent-owned session"),
        );
        create.agent_doc_id = Some(
            self.agent_access()
                .transact_readonly("test.goal_backed.agent", |txn| {
                    Box::pin(async move {
                        Ok::<_, anyhow::Error>(
                            gents_documents::read_desired_state_record_in_txn(
                                txn,
                                gents_store::collection::Collection::Agent,
                                did,
                                agent_did,
                            )
                            .await?
                            .expect("goal-backed request Agent")
                            .0,
                        )
                    })
                })
                .await
                .expect("read goal-backed request Agent"),
        );
        gents_session::request::admission::sign_agent_request_create(
            self.home.agent_identity.as_ref(),
            &mut create,
        )
        .await
        .expect("sign goal-backed request");
        create
    }

    async fn request_doc_and_session(&self, request_id: &str) -> (String, String) {
        let escaped = escape_graphql_string(request_id);
        let rows = self
            .agent_access()
            .execute(&format!(
                r#"{{AgentRequest(filter: {{request_id: {{_eq: "{escaped}"}}}}) {{_docID session_doc_id}}}}"#
            ))
            .await
            .unwrap();
        let row = &rows["data"]["AgentRequest"][0];
        (
            row["_docID"].as_str().unwrap().to_owned(),
            row["session_doc_id"].as_str().unwrap().to_owned(),
        )
    }

    async fn wait_for_goal(
        &self,
        session: &str,
        source: &mut GoalSource,
        description: &str,
        reached: impl Fn(&crate::goal::GoalDocument) -> bool,
    ) -> crate::goal::GoalDocument {
        for _ in 0..SETTLED_PASSES {
            if source.rescan_to_settled(true).await.is_some() {
                panic!("unexpected goal fire while waiting for {description}");
            }
            let goal = self
                .load_goal(self.agent_access(), session)
                .await
                .expect("Goal exists");
            if reached(&goal) {
                return goal;
            }
        }
        panic!("goal transition did not complete in {SETTLED_PASSES} settled passes: {description}")
    }
}

fn test_agent_with_identity(
    principal: Arc<RuntimeNode>,
    agent_identity: Arc<dyn NodeIdentity>,
) -> ResolvedAgent {
    let agent_did = agent_identity.did().to_owned();
    ResolvedAgent {
        configuration: gents_inference::resolved::ResolvedConfiguration::Static,
        profile_id: None,
        skills: Vec::new(),
        oauth_pool: Vec::new(),
        agent_identity,
        agent_did,
        node: principal,
        backend_id: None,
        backend_provider_kind: BackendProviderKind::OpenAiCompatible,
        openai_wire_api: gents_loop::openai_wire::OpenAiWireApi::ChatCompletions,
        backend_endpoint: "http://localhost:8000/v1".to_string(),
        claude_platform: None,
        client_version: None,
        backend_auth: gents_documents::BackendAuth::Unauthenticated,
        model_name: gents_inference::resolved::DEFAULT_MODEL_NAME.to_string(),
        service_tier: None,
        resolved_reasoning_efforts: None,
        context_window: gents_inference::resolved::DEFAULT_CONTEXT_WINDOW,
        max_output_tokens: gents_inference::resolved::DEFAULT_MAX_OUTPUT_TOKENS,
        max_turns: gents_inference::resolved::DEFAULT_MAX_TURNS,
        max_turns_provenance: gents_inference::resolved::MaxTurnsProvenance::Default,
        system_prompt: String::new(),
        compaction: None,
        compaction_inference: None,
        max_total_tokens: None,
        max_invalid_tool_calls: gents_loop::loop_stream::DEFAULT_MAX_INVALID_TOOL_CALLS,
        repeated_tool_failure_limit: gents_loop::loop_stream::DEFAULT_REPEATED_TOOL_FAILURE_LIMIT,
        max_request_retries: gents_documents::inference_defaults::DEFAULT_REQUEST_MAX_RETRIES,
        stream_batch_ms: gents_inference::resolved::DEFAULT_STREAM_BATCH_MS,
        stream_liveness_timeout: Duration::from_secs(
            gents_documents::inference_defaults::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS,
        ),
        deadline_duration: Duration::from_secs(
            gents_documents::inference_defaults::DEFAULT_DEADLINE_DURATION_SECS,
        ),
        title_timeout: Duration::from_secs(gents_inference::resolved::DEFAULT_TITLE_TIMEOUT_SECS),
        provider_idle_timeout: Duration::from_secs(
            gents_inference::resolved::DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS,
        ),
        completion_retry: gents_loop::completion_retry::CompletionRetryProfileFields::default(),
        sampling: gents_inference::resolved::SamplingConfig::default(),
    }
}

async fn verified_block_signer_as(node: &defra_node::Node, reader: &str, cid: &str) -> String {
    let (block, signature) = node
        .authorized_signed_block_bytes(cid, Some(reader))
        .await
        .unwrap_or_else(|error| panic!("read {cid} as {reader}: {error:#}"));
    let block = defra_core::block::Block::from_dag_cbor(&block).expect("decode signed block");
    let signature =
        defra_core::block::Signature::from_dag_cbor(&signature).expect("decode block signature");
    db::block::verify::verified_signature_signer_did(&block, &signature)
        .unwrap_or_else(|error| panic!("verify {cid}: {error}"))
}

#[derive(Debug, Deserialize)]
struct ChildRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    request_id: String,
    session_id: String,
    agent_did: Option<String>,
    caused_by_parent_request_id: Option<String>,
    caused_by_trigger_id: Option<String>,
    caused_by_trigger_kind: Option<String>,
    input: Option<serde_json::Value>,
    lifecycle_state: Option<String>,
    retry_key: Option<String>,
    request_hop: Option<i64>,
    workspace_id: Option<String>,
    workspace_authority: Option<String>,
    workspace_owner_node_did: Option<String>,
    workspace_seal_hash: Option<String>,
}

struct FailedInferenceCall<'a> {
    call_id: &'a str,
    call_kind: &'a str,
    attempt: i64,
    ended_at: &'a str,
    failure_reason: &'a str,
}

async fn goal_children_in_session(access: &ConfigAccess, session: &str) -> Vec<ChildRow> {
    let escaped = escape_graphql_string(session);
    let response = access
        .execute(&format!(
            r#"{{
                AgentRequest(filter: {{ caused_by_trigger_kind: {{ _eq: "goal" }}, session_id: {{ _eq: "{escaped}" }} }}) {{
                    _docID request_id node_did requester_did session_id agent_did
                    caused_by_parent_request_id caused_by_parent_request_doc_id
                    caused_by_trigger_id caused_by_trigger_kind input lifecycle_state
                    retry_key request_hop workspace_id workspace_authority
                    workspace_owner_node_did workspace_seal_hash
                }}
            }}"#
        ))
        .await
        .expect("query selected Agent's goal children");
    serde_json::from_value(response["data"]["AgentRequest"].clone()).expect("decode goal children")
}

async fn seed_goal_fields_from_access(
    access: &ConfigAccess,
    goal: &crate::goal::GoalDocument,
    fields: &str,
) {
    let doc = escape_graphql_string(&goal.doc_id);
    let response = access
        .write(
            "test.goal_fixture_fields",
            &format!(r#"mutation {{update_Goal(docID: "{doc}", input: {{{fields}}}) {{_docID}}}}"#),
        )
        .await
        .unwrap();
    assert_eq!(response["data"]["update_Goal"].as_array().unwrap().len(), 1);
}

async fn set_request_state_as(access: &ConfigAccess, doc: &str, state: &str) {
    let response = access
        .write(
            "test.goal_request_state",
            &format!(
                r#"mutation {{update_AgentRequest(docID: "{}", input: {{lifecycle_state: "{}"}}) {{_docID}}}}"#,
                escape_graphql_string(doc),
                escape_graphql_string(state),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        response["data"]["update_AgentRequest"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

async fn reject_before_claim(access: &ConfigAccess, doc_id: &str, reason: &str) {
    let doc_id = escape_graphql_string(doc_id);
    let reason = escape_graphql_string(reason);
    let terminalized_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let response = access.write("test.goal_readiness_rejection", &format!(r#"mutation {{ update_AgentRequest(
        filter: {{ _docID: {{ _eq: "{doc_id}" }}, lifecycle_state: {{ _eq: "pending" }} }},
        input: {{ lifecycle_state: "failed", failure_reason: "{reason}", terminalized_at: "{terminalized_at}" }}
    ) {{ _docID execution_generation }} }}"#)).await.unwrap();
    let rows = response["data"]["update_AgentRequest"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0]["execution_generation"].is_null());
}

async fn next_intent(source: &mut GoalSource, description: &str) -> FireIntent {
    for _ in 0..SETTLED_PASSES {
        if let Some(intent) = source.rescan_to_settled(true).await {
            return intent;
        }
    }
    panic!("{description}: the goal owner settled {SETTLED_PASSES} times without firing")
}

async fn assert_settles_without_fire(source: &mut GoalSource, description: &str) {
    for _ in 0..2 {
        assert!(
            source.rescan_to_settled(true).await.is_none(),
            "{description}"
        );
    }
}

async fn operator_resume_child_row_from_access(
    access: &ConfigAccess,
    doc_id: &str,
) -> serde_json::Value {
    let doc = escape_graphql_string(doc_id);
    let response = access.execute(&format!(r#"{{AgentRequest(docID: "{doc}") {{
        _docID request_id node_did requester_did session_id agent_did lifecycle_state
        content created_at execution_origin retry_key input admission_kind admission_signer_did
        admission_signature runtime_issuer_did runtime_source_request_id runtime_source_kind
        caused_by_trigger_id caused_by_trigger_doc_id caused_by_trigger_kind caused_by_correlation
        caused_by_source_doc_id caused_by_trigger_context caused_by_parent_request_id caused_by_parent_request_doc_id
        caused_by_parent_tool_call_id caused_by_parent_tool_call_doc_id request_hop workspace_id workspace_authority
        workspace_owner_node_did workspace_seal_hash
    }}}}"#)).await.unwrap();
    response["data"]["AgentRequest"][0].clone()
}

async fn publish_owned_terminal_activity(
    access: &ConfigAccess,
    agent: &dyn NodeIdentity,
    request_doc_id: &str,
    content: &str,
) {
    use gents_protocol::output::rows::{
        output_segment_create_variables, transcript_message_create_variables,
        CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    use gents_protocol::output::{
        MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
        OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, SegmentRun, SourceClose,
        StreamDeclaration, StreamPayload, TerminalOutput, TranscriptMessage,
    };
    use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};
    let escaped = escape_graphql_string(request_doc_id);
    let response = access.execute(&format!(r#"{{AgentRequest(docID: "{escaped}") {{node_did requester_did session_id session_doc_id agent_did}}}}"#)).await.unwrap();
    let row = &response["data"]["AgentRequest"][0];
    let node_did = row["node_did"].as_str().unwrap();
    let requester_did = row["requester_did"].as_str();
    let session_id = row["session_id"].as_str().unwrap();
    let session_doc = row["session_doc_id"].as_str().unwrap();
    assert_eq!(row["agent_did"], agent.did());
    let existing = access.execute(&format!(r#"{{AgentMessage(filter: {{session_doc_id: {{_eq: "{}"}}}}, order: {{sequence: DESC}}, limit: 1) {{sequence}}}}"#, escape_graphql_string(session_doc))).await.unwrap();
    let sequence = existing["data"]["AgentMessage"]
        .as_array()
        .unwrap()
        .first()
        .and_then(|row| row["sequence"].as_u64())
        .unwrap_or(0) as u32
        + 1;
    let generation = format!("goal-observation-{request_doc_id}");
    let source = OutputSource::ProviderTurn {
        scope: CaptureScope {
            kind: CaptureScopeKind::Inference,
            seq: u64::from(sequence),
        },
        turn_index: 0,
        attempt: 0,
    };
    let segment = OutputSegment {
        node_did: node_did.into(),
        requester_did: requester_did.map(Into::into),
        session_id: session_id.into(),
        request_doc_id: request_doc_id.into(),
        source,
        writer: OutputWriter::RequestExecution {
            execution_generation: generation.clone(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: content.len().try_into().unwrap(),
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::Text,
            }),
        }],
        payload: content.into(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![content.len() as u64],
        }),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    access.transact_write("test.goal_owned_terminal_output", |txn| {
        let generation = &generation;
        let segment = &segment;
        let escaped = &escaped;
        Box::pin(async move {
            let mut variables = output_segment_create_variables(segment)?;
            variables["input"]["session_doc_id"] = serde_json::json!(session_doc);
            variables["input"]["agent_did"] = serde_json::json!(agent.did());
            variables["input"]["creator_did"] = serde_json::json!(agent.did());
            variables["input"]["parent_linked"] = serde_json::json!(true);
            let created = txn.execute_with_variables(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION, &variables).await?;
            let close_doc_id = gents_store::graphql::created_doc_id(&created, "AgentOutputSegment")?;
            txn.add_resource_relationship("AgentOutputSegment", &close_doc_id, "parent", "sessions", session_doc).await?;
            txn.add_actor_relationship("AgentOutputSegment", &close_doc_id, "working_agent", &agent.did().parse()?).await?;
            let message = TranscriptMessage {
                native_request_id: None,
                message_key: gents_session::session::sequence_message_key(node_did, session_id, requester_did, sequence),
                session_id: session_id.into(),
                node_did: node_did.into(),
                requester_did: requester_did.map(Into::into),
                request_doc_id: Some(request_doc_id.into()),
                publication: MessagePublication::RequestExecution { execution_generation: generation.clone() },
                outcome: OutputOutcome::Complete,
                sequence,
                role: MessageRole::Assistant,
                native_id: Some(format!("goal-terminal-{sequence}")),
                blocks: vec![MessageBlock::Text { text: PresentedPayload { output: PayloadRef { close_doc_id, stream: 0 }, presentation: PayloadPresentation::Full } }],
                created_at: chrono::Utc::now().to_rfc3339(),
            };
            let mut variables = transcript_message_create_variables(&message)?;
            variables["input"]["session_doc_id"] = serde_json::json!(session_doc);
            variables["input"]["agent_did"] = serde_json::json!(agent.did());
            variables["input"]["creator_did"] = serde_json::json!(agent.did());
            variables["input"]["parent_linked"] = serde_json::json!(true);
            let created = txn.execute_with_variables(CREATE_AGENT_MESSAGE_MUTATION, &variables).await?;
            let message_doc_id = gents_store::graphql::created_doc_id(&created, "AgentMessage")?;
            txn.add_resource_relationship("AgentMessage", &message_doc_id, "parent", "sessions", session_doc).await?;
            txn.add_actor_relationship("AgentMessage", &message_doc_id, "working_agent", &agent.did().parse()?).await?;
            let terminal = gents_protocol::graphql::graphql_input_literal(&serde_json::to_value(TerminalOutput::Message { message_doc_id })?)?;
            txn.execute(&format!(r#"mutation {{update_AgentRequest(docID: "{escaped}", input: {{execution_generation: "{}", terminal_output: {terminal}}}) {{_docID}}}}"#, escape_graphql_string(generation))).await?;
            Ok::<_, anyhow::Error>(())
        })
    }).await.unwrap();
}

async fn seed_same_second_canonical_goal_child(
    f: &GoalFixture,
    session: &str,
    parent_id: &str,
    paused: bool,
) -> (String, String) {
    use sha2::{Digest, Sha256};
    f.reserve_session(session).await;
    let did = f.did();
    let parent_doc = f
        .seed_request_at(session, parent_id, "completed", "2026-07-15T00:00:00Z")
        .await;
    let goal = seed_goal_from_access(
        f.agent_access(),
        did,
        session,
        Some("Continue from the actual causal head"),
        Some(if paused {
            GoalStatus::Paused
        } else {
            GoalStatus::Active
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let digest = Sha256::digest(format!("{}\0{parent_id}", goal.goal_id).as_bytes());
    let suffix = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let child_id = format!("goal-cont-{:020}-{suffix}", 1);
    assert!(
        child_id.as_str() < parent_id,
        "fixture must defeat lexical head selection"
    );
    let mut spec = RequestSpec::new(
        RequestPurpose::Normal,
        RequestIdentity {
            requester_did: Some(f.home.user.did().into()),
            request_id: child_id.clone(),
            node_did: did.to_owned(),
            agent_did: f.agent_did().into(),
            session_id: session.to_owned(),
            content: "The canonical continuation made durable progress".to_owned(),
            execution_origin: ExecutionOrigin::Scheduled,
            created_at: "2026-07-15T00:00:00Z".into(),
        },
        AgentRequestAdmissionRecord::client(f.home.user.did()),
    );
    spec.trigger_lineage.trigger_id = Some(goal.goal_id.clone());
    spec.trigger_lineage.trigger_kind = Some("goal".to_owned());
    spec.parent = Some(gents_session::request::lifecycle::ParentLink {
        parent_request_id: parent_id.to_owned(),
        parent_request_doc_id: parent_doc.clone(),
        ..Default::default()
    });
    spec.retry_key = Some(format!("goal-continuation:{suffix}"));
    spec.input = serde_json::from_value(serde_json::json!({
        "queue": {"source":"goal", "policy":"coalesce", "key":format!("goal:{suffix}"), "queued_after_request_id":parent_id},
        "goal_continuation": {"sequence":1, "wrapup":false},
        "goal_assignment": {"goal_id":goal.goal_id, "root_request_doc_id":goal.assignment_root_request_doc_id}
    })).unwrap();
    let child = build_signed_client_request(
        f.client_access(),
        spec,
        RequestSigner::Identity(&f.home.user),
    )
    .await
    .unwrap();
    submit_client_request(f.client_access(), &child)
        .await
        .unwrap();
    let children = goal_children_in_session(f.agent_access(), session).await;
    assert_eq!(children.len(), 1);
    let child_doc = children[0].doc_id.clone();
    set_request_state_as(
        f.agent_access(),
        &child_doc,
        if paused { "interrupted" } else { "completed" },
    )
    .await;
    if paused {
        let terminal = gents_protocol::graphql::graphql_input_literal(
            &serde_json::to_value(gents_protocol::output::TerminalOutput::NoMessage).unwrap(),
        )
        .unwrap();
        let changed = f
            .agent_access()
            .write(
                "test.goal_no_terminal_message",
                &format!(
                    r#"mutation {{update_AgentRequest(docID: "{}", input: {{terminal_output: {terminal}}}) {{_docID}}}}"#,
                    escape_graphql_string(&child_doc)
                ),
            )
            .await
            .unwrap();
        assert_eq!(
            changed["data"]["update_AgentRequest"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    } else {
        publish_owned_terminal_activity(
            f.agent_access(),
            f.home.agent_identity.as_ref(),
            &child_doc,
            "canonical child produced durable activity",
        )
        .await;
    }
    let root = goal
        .assignment_root_request_doc_id
        .clone()
        .unwrap_or_else(|| child_doc.clone());
    seed_goal_fields_from_access(
        f.agent_access(),
        &goal,
        &format!(
            "continuation_sequence: 1, last_continued_from_request_id: \"{}\", assignment_root_request_doc_id: \"{}\"",
            escape_graphql_string(parent_id),
            escape_graphql_string(&root)
        ),
    )
    .await;
    (child_id, child_doc)
}

async fn create_visible_foreign_collision(
    f: &GoalFixture,
    request_id: &str,
    retry_key: Option<&str>,
) -> String {
    let directory = tempfile::tempdir().unwrap();
    let foreign_node = gents_store::identity::KeyIdentity::load_or_create(
        directory.path().join("foreign-node.key"),
        None,
    )
    .unwrap();
    let binding = gents_store::identity::provision_agent_identity(&foreign_node).unwrap();
    let foreign_agent =
        gents_store::identity::resolve_agent_identity(&foreign_node, &binding).unwrap();
    let foreign_user = gents_store::identity::KeyIdentity::load_or_create(
        directory.path().join("foreign-user.key"),
        None,
    )
    .unwrap();
    let admin = ConfigAccess::local_as(f.home.node.clone(), f.did().parse().unwrap());
    for actor in [foreign_user.did(), foreign_agent.did()] {
        for permission in [
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::CollectionGet,
            NodePermission::DacRelationAdd,
            NodePermission::DacRelationDelete,
        ] {
            admin
                .grant_node_permission(&actor.parse().unwrap(), permission)
                .await
                .unwrap();
        }
    }
    let client = f.home.client_access(&foreign_user);
    let backend_id = format!("{}:backend", foreign_agent.did());
    let profile_id = format!("{}:inference", foreign_agent.did());
    let plan = gents_documents::DesiredStateApplyPlan::new(
        [
            (
                gents_store::collection::Collection::Node,
                serde_json::json!({"node_did": foreign_node.did()}),
            ),
            (
                gents_store::collection::Collection::InferenceBackend,
                serde_json::json!({"node_did": foreign_node.did(), "backend_id": backend_id,
                    "name": "Foreign inference", "provider_kind": "OpenAiCompatible",
                    "endpoint": "http://127.0.0.1:1/v1", "auth": {"kind": "unauthenticated"}}),
            ),
            (
                gents_store::collection::Collection::InferenceProfile,
                serde_json::json!({"node_did": foreign_node.did(), "profile_id": profile_id,
                    "backend_id": backend_id, "model_name": "test-model"}),
            ),
            (
                gents_store::collection::Collection::Agent,
                serde_json::json!({"node_did": foreign_node.did(), "agent_did": foreign_agent.did(),
                    "creation_nonce": binding.creation_nonce, "display_name": "Foreign collision Agent",
                    "inference_profile_id": profile_id, "enabled": true}),
            ),
        ]
        .into_iter()
        .map(
            |(collection, value)| gents_documents::DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            },
        )
        .collect(),
    )
    .unwrap();
    client
        .transact_write("test.foreign_collision.configuration", |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .unwrap();
    let mut spec = RequestSpec::new(
        RequestPurpose::Normal,
        RequestIdentity {
            node_did: foreign_node.did().into(),
            requester_did: Some(foreign_user.did().into()),
            agent_did: foreign_agent.did().into(),
            session_id: "foreign-session".into(),
            request_id: request_id.into(),
            content: "foreign collision".into(),
            execution_origin: ExecutionOrigin::Interactive,
            created_at: "2026-07-15T00:00:01Z".into(),
        },
        AgentRequestAdmissionRecord::client(foreign_user.did()),
    );
    spec.retry_key = retry_key.map(str::to_owned);
    let create = build_signed_client_request(&client, spec, RequestSigner::Identity(&foreign_user))
        .await
        .unwrap();
    submit_client_request(&client, &create).await.unwrap();
    let request = escape_graphql_string(request_id);
    let result = client.execute(&format!(r#"{{AgentRequest(filter: {{request_id: {{_eq: "{request}"}}}}, limit: 2) {{_docID node_did session_doc_id agent_doc_id}}}}"#)).await.unwrap();
    let [row] = result["data"]["AgentRequest"]
        .as_array()
        .unwrap()
        .as_slice()
    else {
        panic!("one foreign collision request")
    };
    let doc = row["_docID"].as_str().unwrap().to_owned();
    assert_eq!(
        row["session_doc_id"],
        create.session_doc_id.as_deref().unwrap()
    );
    assert_eq!(row["agent_doc_id"], create.agent_doc_id.as_deref().unwrap());
    client
        .add_actor_relationship(
            "AgentRequest",
            &doc,
            "reader",
            &f.agent_did().parse().unwrap(),
        )
        .await
        .unwrap();
    let query = format!(
        r#"{{AgentRequest(docID: "{}") {{_docID node_did}}}}"#,
        escape_graphql_string(&doc)
    );
    let visible = f.agent_access().execute(&query).await.unwrap();
    assert_eq!(
        visible["data"]["AgentRequest"][0]["node_did"],
        foreign_node.did()
    );
    doc
}

async fn seed_lineage_parent(
    f: &GoalFixture,
    session: &str,
    request_id: &str,
    terminal: &str,
    workspace_id: &str,
    seal_hash: &str,
) -> (String, String) {
    f.reserve_session(session).await;
    let root_id = "upstream-root";
    let root = f
        .seed_request_at(session, root_id, "completed", "2026-07-14T23:59:58Z")
        .await;
    let mut predecessor_id = root_id.to_owned();
    let mut predecessor_doc = root.clone();
    for (id, created_at, hop, tool_id) in [
        (
            "upstream-middle",
            "2026-07-14T23:59:59Z",
            1,
            "upstream-root-call",
        ),
        (
            request_id,
            "2026-07-15T00:00:00Z",
            2,
            "upstream-middle-call",
        ),
    ] {
        let resp = f
            .agent_access()
            .execute(&format!(
                r#"{{AgentRequest(docID: "{}") {{session_doc_id}}}}"#,
                escape_graphql_string(&predecessor_doc)
            ))
            .await
            .unwrap();
        let session_doc = resp["data"]["AgentRequest"][0]["session_doc_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let node_did = f.did();
        let user_did = f.home.user.did();
        let agent_did = f.agent_did();
        let pred_id = predecessor_id.as_str();
        let pred_doc = predecessor_doc.as_str();
        let tool_doc = f.agent_access().transact_write("test.goal_lineage_tool", |txn| {
            let session_doc = &session_doc;
            Box::pin(async move {
                let input = serde_json::json!({
                    "tool_call_key": tool_id, "tool_call_id": tool_id, "node_did": node_did,
                    "requester_did": user_did, "session_id": session, "session_doc_id": session_doc,
                    "agent_did": agent_did, "creator_did": agent_did, "parent_linked": true,
                    "request_id": pred_id, "request_doc_id": pred_doc, "tool_name": "agent_message",
                    "status": "completed", "lifecycle_state": "completed"
                });
                let result = txn.execute_with_variables(
                    "mutation($input: AgentToolCallMutationInputArg!) {create_AgentToolCall(input:$input) {_docID}}",
                    &serde_json::json!({"input": input}),
                ).await?;
                let doc = gents_store::graphql::created_doc_id(&result, "AgentToolCall")?;
                txn.add_resource_relationship("AgentToolCall", &doc, "parent", "sessions", session_doc).await?;
                txn.add_actor_relationship("AgentToolCall", &doc, "working_agent", &agent_did.parse()?).await?;
                Ok::<_, anyhow::Error>(doc)
            })
        }).await.unwrap();
        let spec = RequestSpec::new(
            RequestPurpose::Normal,
            RequestIdentity {
                requester_did: Some(f.home.user.did().into()),
                request_id: id.into(),
                node_did: f.did().into(),
                agent_did: f.agent_did().into(),
                session_id: session.into(),
                content: "Continue the original graph work".into(),
                execution_origin: ExecutionOrigin::Interactive,
                created_at: created_at.into(),
            },
            AgentRequestAdmissionRecord::client(f.home.user.did()),
        );
        let mut request = build_signed_client_request(
            f.client_access(),
            spec,
            RequestSigner::Identity(&f.home.user),
        )
        .await
        .unwrap();
        request.request_hop = hop;
        request.caused_by_parent_request_id = Some(predecessor_id.clone());
        request.caused_by_parent_request_doc_id = Some(predecessor_doc.clone());
        request.caused_by_parent_tool_call_id = Some(tool_id.to_owned());
        request.caused_by_parent_tool_call_doc_id = Some(tool_doc);
        if hop == 2 {
            request.caused_by_trigger_id = Some("original-graph-trigger".into());
            request.caused_by_trigger_kind = Some("event".into());
            request.caused_by_correlation = Some("original-graph-correlation".into());
            request.caused_by_source_doc_id = Some(root.clone());
            request.caused_by_trigger_context =
                Some(r#"{"source_fields":{"artifact":"original"}}"#.into());
            request.workspace_id = Some(workspace_id.into());
            request.workspace_authority = Some("readOnly".into());
            request.workspace_owner_node_did = Some(f.did().into());
            request.workspace_seal_hash = Some(seal_hash.into());
        }
        gents_session::request::admission::sign_agent_request_create(&f.home.user, &mut request)
            .await
            .unwrap();
        submit_client_request(f.client_access(), &request)
            .await
            .unwrap();
        let response = f
            .client_access()
            .execute(&format!(
                r#"{{AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}, limit: 2) {{_docID}}}}"#,
                escape_graphql_string(id)
            ))
            .await
            .unwrap();
        let rows = response["data"]["AgentRequest"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        predecessor_doc = rows[0]["_docID"].as_str().unwrap().to_owned();
        set_request_state_as(
            f.agent_access(),
            &predecessor_doc,
            if hop == 2 { terminal } else { "completed" },
        )
        .await;
        predecessor_id = id.to_owned();
    }
    (predecessor_doc, root)
}

#[tokio::test]
async fn model_goal_create_is_idempotent_and_never_mutates_a_conflict() {
    let f = GoalFixture::start("goal-model-create").await;
    let session = "goal-model-create";
    f.reserve_session(session).await;
    let client = f.client_access();
    let created = crate::goal::create_goal_for_session_from_access(
        client,
        f.did(),
        session,
        "Ship the feature",
        Some(1_000),
    )
    .await
    .expect("create model goal");
    assert!(matches!(created, CreateGoalForSessionOutcome::Created(_)));
    assert_ne!(f.home.user.did(), f.did());
    assert_ne!(f.home.user.did(), f.agent_did());
    let rows = client.execute("{ Goal { _docID session_doc_id agent_did creator_did parent_linked } GoalCreationClaim { _docID session_doc_id agent_did creator_did parent_linked } }").await.unwrap();
    for case in gents_lean_vocab::families::goal_links_session_goal::cases() {
        let collection = case.input.collection.as_str();
        let row = &rows["data"][collection][0];
        assert_eq!(row["agent_did"], f.agent_did());
        assert_eq!(row["creator_did"], f.home.user.did());
        case.check(row["parent_linked"].as_bool().unwrap());
        let doc = row["_docID"].as_str().unwrap().to_owned();
        let owners = client
            .transact_readonly("test.client_goal_owner", |txn| {
                let doc = &doc;
                Box::pin(async move {
                    txn.document_owners(collection, std::slice::from_ref(doc))
                        .await
                })
            })
            .await
            .unwrap();
        assert_eq!(
            owners[0].owner.as_ref().map(|owner| owner.as_str()),
            Some(f.home.user.did())
        );
        let escaped = escape_graphql_string(&doc);
        let history = client.execute(&format!(r#"{{ _commits(docID: "{escaped}", filter: {{fieldName: {{_eq: "_C"}}}}, order: {{height: ASC}}, limit: 1) {{cid}} }}"#)).await.unwrap();
        let cid = history["data"]["_commits"][0]["cid"].as_str().unwrap();
        assert_eq!(
            verified_block_signer_as(&f.home.node, f.home.user.did(), cid).await,
            f.home.user.did()
        );
    }

    let duplicate = crate::goal::create_goal_for_session_from_access(
        client,
        f.did(),
        session,
        " Ship the feature ",
        Some(1_000),
    )
    .await
    .expect("idempotent model goal retry");
    assert!(matches!(
        duplicate,
        CreateGoalForSessionOutcome::Idempotent(_)
    ));
    assert_eq!(
        client.execute("{ Goal { _docID } }").await.unwrap()["data"]["Goal"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let error = crate::goal::create_goal_for_session_from_access(
        client,
        f.did(),
        session,
        "Different objective",
        Some(1_000),
    )
    .await
    .expect_err("conflicting create must fail");
    assert!(
        matches!(error, CreateGoalForSessionError::Conflict),
        "{error:?}"
    );
    let unchanged = f.load_goal(client, session).await.expect("goal remains");
    assert_eq!(unchanged.objective, "Ship the feature");
    assert_eq!(unchanged.token_budget, Some(1_000));
}

#[tokio::test]
async fn set_goal_over_http_transactions_computes_the_same_baseline() {
    let f = GoalFixture::start("http-goal-baseline").await;
    let session = "http-goal-session";
    f.reserve_session(session).await;
    let access = f.client_access();
    let request_doc = f
        .seed_request(session, "http-baseline-request", "completed")
        .await;
    f.seed_usage(
        session,
        "http-baseline-request",
        &request_doc,
        "http-baseline-call",
        (1000, 50, 0),
    )
    .await;
    let (_, session_doc) = f.request_doc_and_session("http-baseline-request").await;
    let goal = seed_goal_from_access(
        access,
        f.did(),
        session,
        Some("HTTP baseline"),
        None,
        None,
        None,
    )
    .await
    .expect("set goal over HTTP");
    assert_eq!(goal.token_usage_baseline, Some(1050));
    assert_eq!(goal.tokens_used, Some(0));
    let escaped = escape_graphql_string(&goal.doc_id);
    let rows = access.execute(&format!(r#"{{ Goal(filter: {{_docID: {{_eq: "{escaped}"}}}}) {{session_doc_id agent_did creator_did parent_linked}} _commits(docID: "{escaped}", filter: {{fieldName: {{_eq: "_C"}}}}, order: {{height: ASC}}, limit: 1) {{cid}} }}"#)).await.unwrap();
    let goal_row = &rows["data"]["Goal"][0];
    assert_eq!(goal_row["session_doc_id"], session_doc);
    assert_eq!(goal_row["agent_did"], f.agent_did());
    assert_eq!(goal_row["creator_did"], f.home.user.did());
    for case in gents_lean_vocab::families::goal_links_seeded_goal::cases() {
        assert_eq!(case.input.collection, "Goal");
        case.check(goal_row["parent_linked"].as_bool().unwrap());
    }
    let cid = rows["data"]["_commits"][0]["cid"].as_str().unwrap();
    assert_eq!(
        verified_block_signer_as(&f.home.node, f.home.user.did(), cid).await,
        f.home.user.did()
    );

    let client_session = "http-client-goal";
    let client_session_doc = f.reserve_session(client_session).await;
    let created = crate::goal::create_goal_for_session_from_access(
        access,
        f.did(),
        client_session,
        "Client-authored Goal",
        Some(500),
    )
    .await
    .expect("create client Goal over HTTP");
    assert!(matches!(created, CreateGoalForSessionOutcome::Created(_)));
    let retried = crate::goal::create_goal_for_session_from_access(
        access,
        f.did(),
        client_session,
        "Client-authored Goal",
        Some(500),
    )
    .await
    .expect("retry client Goal over HTTP");
    assert!(matches!(
        retried,
        CreateGoalForSessionOutcome::Idempotent(_)
    ));
    assert_eq!(created.goal().doc_id, retried.goal().doc_id);
    for case in gents_lean_vocab::families::goal_links_client_session_goal::cases() {
        let collection = case.input.collection.as_str();
        let session_escaped = escape_graphql_string(&client_session_doc);
        let rows = access.execute(&format!(r#"{{ {collection}(filter: {{session_doc_id: {{_eq: "{session_escaped}"}}}}) {{_docID session_doc_id agent_did creator_did parent_linked}} }}"#)).await.unwrap();
        let rows = rows["data"][collection].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row["session_doc_id"], client_session_doc);
        assert_eq!(row["agent_did"], f.agent_did());
        assert_eq!(row["creator_did"], f.home.user.did());
        case.check(row["parent_linked"].as_bool().unwrap());
        let doc = escape_graphql_string(row["_docID"].as_str().unwrap());
        let history = access.execute(&format!(r#"{{_commits(docID: "{doc}", filter: {{fieldName: {{_eq: "_C"}}}}, order: {{height: ASC}}, limit: 1) {{cid}}}}"#)).await.unwrap();
        let cid = history["data"]["_commits"][0]["cid"].as_str().unwrap();
        assert_eq!(
            verified_block_signer_as(&f.home.node, f.home.user.did(), cid).await,
            f.home.user.did()
        );
    }
}

#[tokio::test]
async fn goal_backed_submission_commits_goal_before_request_visibility_and_retries_idempotently() {
    let f = GoalFixture::start("goal-backed-submit").await;
    let access = f.agent_access();

    // Atomic create + Lean publication links + idempotent retries even after goal progress.
    {
        let session = "goal-backed-atomic";
        let create = f
            .signed_goal_backed_request(session, "goal-backed-first")
            .await;
        let input = gents_protocol::operations::goal::GoalStartInput {
            session_id: session.to_string(),
            objective: "Ship atomically".to_string(),
            token_budget: Some(5_000),
        };
        let first = crate::operations::goal::start(access, f.did(), &input, &create)
            .await
            .expect("atomic goal submission");
        assert_eq!(first, crate::goal::GoalBackedRequestDisposition::Created);
        let escaped = escape_graphql_string(session);
        let binding = access
            .execute(&format!(
                r#"{{
                    AgentSession(filter: {{ session_id: {{ _eq: "{escaped}" }} }}) {{ _docID agent_did requester_did }}
                    Goal(filter: {{ session_id: {{ _eq: "{escaped}" }} }}) {{ _docID session_doc_id agent_did creator_did parent_linked assignment_root_request_doc_id }}
                    GoalCreationClaim(filter: {{ session_id: {{ _eq: "{escaped}" }} }}) {{ _docID session_doc_id agent_did creator_did parent_linked }}
                    AgentRequest(filter: {{ session_id: {{ _eq: "{escaped}" }} }}) {{ _docID request_id lifecycle_state }}
                }}"#
            ))
            .await
            .unwrap();
        let sess_row = &binding["data"]["AgentSession"][0];
        let req_row = &binding["data"]["AgentRequest"][0];
        assert_eq!(sess_row["agent_did"], f.agent_did());
        assert_eq!(sess_row["requester_did"], f.agent_did());
        for case in gents_lean_vocab::families::goal_links_goal_backed_submission::cases() {
            let collection = case.input.collection.as_str();
            let row = &binding["data"][collection][0];
            assert_eq!(row["session_doc_id"], sess_row["_docID"]);
            assert_eq!(row["agent_did"], f.agent_did());
            assert_eq!(row["creator_did"], f.agent_did());
            case.check(row["parent_linked"].as_bool().unwrap());
            let doc = row["_docID"].as_str().unwrap().to_owned();
            let owners = access
                .transact_readonly("test.goal_backed_native_owner", |txn| {
                    let doc = &doc;
                    Box::pin(async move {
                        txn.document_owners(collection, std::slice::from_ref(doc))
                            .await
                    })
                })
                .await
                .unwrap();
            assert_eq!(
                owners[0].owner.as_ref().map(|owner| owner.as_str()),
                Some(f.agent_did())
            );
        }
        assert_eq!(
            binding["data"]["Goal"][0]["assignment_root_request_doc_id"],
            req_row["_docID"]
        );
        assert_eq!(binding["data"]["Goal"].as_array().unwrap().len(), 1);
        assert_eq!(binding["data"]["AgentRequest"].as_array().unwrap().len(), 1);
        assert_eq!(req_row["lifecycle_state"], "pending");

        let retry = crate::operations::goal::start(access, f.did(), &input, &create)
            .await
            .expect("idempotent atomic retry");
        assert_eq!(retry, crate::goal::GoalBackedRequestDisposition::Idempotent);

        let committed_goal = f.load_goal(access, session).await.expect("goal exists");
        access
            .write(
                "test.goal_backed_progress",
                &format!(
                    r#"mutation {{ update_Goal(docID: "{}", input: {{ status: "complete" }}) {{ _docID }} }}"#,
                    escape_graphql_string(&committed_goal.doc_id),
                ),
            )
            .await
            .expect("simulate progress after an ambiguous commit acknowledgement");
        let late_retry = crate::operations::goal::start(access, f.did(), &input, &create)
            .await
            .expect("exact retry remains idempotent after goal progress");
        assert_eq!(
            late_retry,
            crate::goal::GoalBackedRequestDisposition::Idempotent
        );
    }

    // Retry rejects changed logical request fields.
    {
        let session = "goal-backed-fingerprint";
        let create = f
            .signed_goal_backed_request(session, "goal-backed-fingerprint")
            .await;
        let input = gents_protocol::operations::goal::GoalStartInput {
            session_id: session.to_string(),
            objective: "Ship one exact request".to_string(),
            token_budget: None,
        };
        crate::operations::goal::start(access, f.did(), &input, &create)
            .await
            .expect("initial goal submission");
        let mut changed = create.clone();
        changed.content = "Begin different work".to_string();
        gents_session::request::admission::sign_agent_request_create(
            f.home.agent_identity.as_ref(),
            &mut changed,
        )
        .await
        .expect("re-sign changed request");
        let error = crate::operations::goal::start(access, f.did(), &input, &changed)
            .await
            .expect_err("changed retry semantics must conflict");
        assert!(error
            .to_string()
            .contains("different immutable request fields"));
    }

    // Matching terminal goal is rejected without publishing request or claim.
    {
        let session = "goal-backed-terminal";
        let create = f
            .signed_goal_backed_request(session, "goal-backed-terminal")
            .await;
        seed_goal_from_access(
            access,
            f.did(),
            session,
            Some("Already terminal"),
            Some(GoalStatus::Complete),
            None,
            None,
        )
        .await
        .expect("set terminal goal");
        let error = crate::operations::goal::start(
            access,
            f.did(),
            &gents_protocol::operations::goal::GoalStartInput {
                session_id: session.to_string(),
                objective: "Already terminal".to_string(),
                token_budget: None,
            },
            &create,
        )
        .await
        .expect_err("terminal goal must reject a new first request");
        assert!(error.to_string().contains("non-active goal"));
        let escaped = escape_graphql_string(session);
        let response = access
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{escaped}" }} }}) {{ request_id }} GoalCreationClaim(filter: {{ session_id: {{ _eq: "{escaped}" }} }}) {{ creation_key }} }}"#
            ))
            .await
            .unwrap();
        assert!(response["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(response["data"]["GoalCreationClaim"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    // Request staging failure rolls back Goal and claim while preserving reserved session.
    {
        let session = "goal-backed-rollback";
        let mut invalid = f
            .signed_goal_backed_request(session, "goal-backed-invalid")
            .await;
        invalid.initial_lifecycle_state =
            gents_protocol::client_protocol::RequestLifecycleState::Processing;
        gents_session::request::admission::sign_agent_request_create(
            f.home.agent_identity.as_ref(),
            &mut invalid,
        )
        .await
        .expect("re-sign invalid request");
        let escaped = escape_graphql_string(session);
        let reserved = access
            .execute(&format!(
                r#"{{ AgentSession(filter: {{ session_id: {{ _eq: "{escaped}" }} }}) {{ _docID }} }}"#
            ))
            .await
            .expect("load reserved session")["data"]["AgentSession"]
            .clone();
        let error = crate::operations::goal::start(
            access,
            f.did(),
            &gents_protocol::operations::goal::GoalStartInput {
                session_id: session.to_string(),
                objective: "Never publish partially".to_string(),
                token_budget: None,
            },
            &invalid,
        )
        .await
        .expect_err("invalid request must abort transaction");
        assert!(error.to_string().contains("pending state"));
        let response = access
            .execute(&format!(
                r#"{{
                    AgentSession(filter: {{ session_id: {{ _eq: "{escaped}" }} }}) {{ _docID }}
                    Goal(filter: {{ session_id: {{ _eq: "{escaped}" }} }}) {{ _docID }}
                    GoalCreationClaim(filter: {{ session_id: {{ _eq: "{escaped}" }} }}) {{ creation_key }}
                    AgentRequest(filter: {{ session_id: {{ _eq: "{escaped}" }} }}) {{ request_id }}
                }}"#
            ))
            .await
            .unwrap();
        assert_eq!(
            response["data"]["AgentSession"], reserved,
            "the aborted transaction left the reserved session unchanged"
        );
        assert_eq!(reserved.as_array().map(Vec::len), Some(1));
        for collection in ["Goal", "GoalCreationClaim", "AgentRequest"] {
            assert!(
                response["data"][collection].as_array().unwrap().is_empty(),
                "{collection} escaped the aborted transaction"
            );
        }
    }
}

async fn scenario_stale_controller_cas(f: &GoalFixture, session: &str, status: GoalStatus) {
    f.reserve_session(session).await;
    f.seed_request(session, &format!("newly-claimed-{session}"), "completed")
        .await;
    let stale = seed_goal_from_access(
        f.agent_access(),
        f.did(),
        session,
        Some("Preserve the newer continuation from stale controller decisions"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .expect("set active goal");
    assert_eq!(stale.continuation_sequence(), 0);
    let parent_id = format!("newly-claimed-{session}");
    assert!(
        crate::goal::claim_continuation_from_access(f.agent_access(), &stale, &parent_id)
            .await
            .expect("claim newer continuation")
    );
    let claimed = f.load_goal(f.agent_access(), session).await.unwrap();
    assert_eq!(claimed.parsed_status(), Some(GoalStatus::Active));
    assert_eq!(claimed.continuation_sequence(), 1);
    assert_eq!(
        claimed.last_continued_from_request_id.as_deref(),
        Some(parent_id.as_str())
    );
    let fields = format!(
        r#"status: "{}", last_failure: "stale controller observation""#,
        status.as_str()
    );
    assert!(
        !crate::goal::update_goal_fields_if_status_from_access(
            f.agent_access(),
            &stale,
            GoalStatus::Active,
            &fields,
        )
        .await
        .expect("stale controller mutation returns cleanly"),
        "same status must not authorize an older continuation snapshot"
    );
    let current = f.load_goal(f.agent_access(), session).await.unwrap();
    assert_eq!(current.parsed_status(), Some(GoalStatus::Active));
    assert_eq!(
        current.continuation_sequence(),
        claimed.continuation_sequence()
    );
    assert_eq!(
        current.last_continued_from_request_id,
        claimed.last_continued_from_request_id
    );
    assert_eq!(current.last_failure, claimed.last_failure);
    assert!(crate::goal::update_goal_fields_if_status_from_access(
        f.agent_access(),
        &current,
        GoalStatus::Active,
        &fields,
    )
    .await
    .expect("current controller mutation"));
    let updated = f.load_goal(f.agent_access(), session).await.unwrap();
    assert_eq!(updated.parsed_status(), Some(status));
    assert_eq!(updated.continuation_sequence(), 1);
}

async fn scenario_cas_and_clear(f: &GoalFixture) {
    // continuation_claim_rejects_a_stale_active_status_snapshot
    let s1 = "cas-stale-active";
    f.reserve_session(s1).await;
    f.seed_request(s1, "parent-stale", "completed").await;
    let stale = seed_goal_from_access(
        f.agent_access(),
        f.did(),
        s1,
        Some("Do not race an operator pause"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .unwrap();
    seed_goal_from_access(
        f.client_access(),
        f.did(),
        s1,
        None,
        Some(GoalStatus::Paused),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(
        !crate::goal::claim_continuation_from_access(f.agent_access(), &stale, "parent-stale")
            .await
            .unwrap()
    );

    // controller_status_cas_never_overwrites_a_newer_completion
    let s2 = "cas-completion-wins";
    f.reserve_session(s2).await;
    let stale = seed_goal_from_access(
        f.agent_access(),
        f.did(),
        s2,
        Some("Completion wins controller races"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .unwrap();
    seed_goal_from_access(
        f.client_access(),
        f.did(),
        s2,
        None,
        Some(GoalStatus::Complete),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(!crate::goal::update_goal_fields_if_status_from_access(
        f.agent_access(),
        &stale,
        GoalStatus::Active,
        r#"status: "paused", last_failure: "stale controller""#,
    )
    .await
    .unwrap());
    assert_eq!(
        f.load_goal(f.agent_access(), s2)
            .await
            .unwrap()
            .parsed_status(),
        Some(GoalStatus::Complete)
    );

    scenario_stale_controller_cas(f, "cas-older-pause", GoalStatus::Paused).await;
    scenario_stale_controller_cas(f, "cas-older-block", GoalStatus::Blocked).await;

    // clearing_a_goal_also_clears_its_creation_claim
    let s3 = "cas-clear-claim";
    f.reserve_session(s3).await;
    crate::goal::create_goal_for_session_from_access(
        f.agent_access(),
        f.did(),
        s3,
        "First objective",
        None,
    )
    .await
    .unwrap();
    let cleared = f
        .agent_access()
        .transact_write("test.clear_agent_goal", |txn| {
            Box::pin(async move {
                let goal = txn
                    .execute(r#"mutation { delete_Goal(filter: {session_id: {_eq: "cas-clear-claim"}}) {_docID} }"#)
                    .await?;
                let claim = txn
                    .execute(r#"mutation { delete_GoalCreationClaim(filter: {session_id: {_eq: "cas-clear-claim"}}) {_docID} }"#)
                    .await?;
                let goals = goal["data"]["delete_Goal"].as_array().map_or(0, Vec::len);
                let claims = claim["data"]["delete_GoalCreationClaim"]
                    .as_array()
                    .map_or(0, Vec::len);
                Ok::<_, anyhow::Error>((goals, claims))
            })
        })
        .await
        .unwrap();
    assert_eq!(cleared, (1, 1));
    let replacement = crate::goal::create_goal_for_session_from_access(
        f.agent_access(),
        f.did(),
        s3,
        "Replacement objective",
        None,
    )
    .await
    .unwrap();
    assert!(matches!(
        replacement,
        CreateGoalForSessionOutcome::Created(_)
    ));
}

async fn scenario_baselines_and_usage(f: &GoalFixture) {
    // fresh_goal_baseline_snapshots_prior_session_usage_and_charges_only_new_spend
    let s1 = "baseline-prior";
    f.reserve_session(s1).await;
    let doc = f.seed_request(s1, "parent-baseline", "completed").await;
    f.seed_usage(s1, "parent-baseline", &doc, "baseline-call", (1000, 50, 0))
        .await;
    let CreateGoalForSessionOutcome::Created(goal) =
        crate::goal::create_goal_for_session_from_access(
            f.client_access(),
            f.did(),
            s1,
            "Baseline prior usage",
            Some(10_000),
        )
        .await
        .unwrap()
    else {
        panic!("expected fresh create")
    };
    assert_eq!(
        (goal.token_usage_baseline, goal.tokens_used),
        (Some(1050), Some(0))
    );
    f.seed_usage(s1, "parent-baseline", &doc, "post-goal-call", (400, 50, 0))
        .await;
    let refreshed = f.load_goal(f.client_access(), s1).await.unwrap();
    assert_eq!(
        (refreshed.token_usage_baseline, refreshed.tokens_used),
        (Some(1050), Some(0))
    );
    let tokens = crate::goal::refresh_goal_usage_from_access(f.agent_access(), &refreshed)
        .await
        .unwrap();
    assert_eq!(tokens, 450);
    let charged = f.load_goal(f.client_access(), s1).await.unwrap();
    assert_eq!(
        (charged.token_usage_baseline, charged.tokens_used),
        (Some(1050), Some(450))
    );

    // idempotent_goal_recreate_preserves_the_existing_baseline
    let s2 = "baseline-idempotent";
    f.reserve_session(s2).await;
    let doc = f.seed_request(s2, "parent-idempotent", "completed").await;
    f.seed_usage(s2, "parent-idempotent", &doc, "first-call", (1000, 50, 0))
        .await;
    let CreateGoalForSessionOutcome::Created(goal) =
        crate::goal::create_goal_for_session_from_access(
            f.client_access(),
            f.did(),
            s2,
            "Baseline prior usage",
            Some(10_000),
        )
        .await
        .unwrap()
    else {
        panic!("expected fresh create")
    };
    assert_eq!(goal.token_usage_baseline, Some(1050));
    f.seed_usage(s2, "parent-idempotent", &doc, "second-call", (400, 50, 0))
        .await;
    let CreateGoalForSessionOutcome::Idempotent(goal) =
        crate::goal::create_goal_for_session_from_access(
            f.client_access(),
            f.did(),
            s2,
            " Baseline prior usage ",
            Some(10_000),
        )
        .await
        .unwrap()
    else {
        panic!("expected idempotent replay")
    };
    assert_eq!(
        (goal.token_usage_baseline, goal.tokens_used),
        (Some(1050), Some(0))
    );
    let replayed = seed_goal_from_access(
        f.agent_access(),
        f.did(),
        s2,
        Some("Baseline prior usage"),
        Some(GoalStatus::Active),
        Some(Some(10_000)),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        (replayed.token_usage_baseline, replayed.tokens_used),
        (Some(1050), Some(0))
    );
    assert_eq!(
        f.client_access()
            .execute(
                r#"{ Goal(filter: { session_id: { _eq: "baseline-idempotent" } }) { _docID } }"#
            )
            .await
            .unwrap()["data"]["Goal"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // fresh_goal_with_zero_prior_usage_starts_charging_from_zero
    let s3 = "baseline-zero";
    f.reserve_session(s3).await;
    let doc = f.seed_request(s3, "parent-zero", "completed").await;
    let CreateGoalForSessionOutcome::Created(goal) =
        crate::goal::create_goal_for_session_from_access(
            f.client_access(),
            f.did(),
            s3,
            "Zero prior usage",
            Some(10_000),
        )
        .await
        .unwrap()
    else {
        panic!("expected fresh create")
    };
    assert_eq!(
        (goal.token_usage_baseline, goal.tokens_used),
        (Some(0), Some(0))
    );
    f.seed_usage(s3, "parent-zero", &doc, "only-call", (400, 50, 0))
        .await;
    let refreshed = f.load_goal(f.client_access(), s3).await.unwrap();
    assert_eq!(
        crate::goal::refresh_goal_usage_from_access(f.agent_access(), &refreshed)
            .await
            .unwrap(),
        450
    );
    let charged = f.load_goal(f.client_access(), s3).await.unwrap();
    assert_eq!(
        (charged.token_usage_baseline, charged.tokens_used),
        (Some(0), Some(450))
    );

    // session_token_usage_charges_cached_input_as_part_of_the_total
    let s4 = "cached-input";
    f.reserve_session(s4).await;
    let doc = f.seed_request(s4, "parent-cached", "completed").await;
    f.seed_usage(
        s4,
        "parent-cached",
        &doc,
        "goal-cached-call",
        (1000, 50, 800),
    )
    .await;
    assert_eq!(
        session_token_usage(&f.home.node, &f.agent_did().parse().unwrap(), f.did(), s4)
            .await
            .unwrap(),
        1050
    );
}

async fn scenario_concurrent_create_exhaustion_and_time_accounting(f: &GoalFixture) {
    // concurrent_model_goal_create_converges_to_one_physical_goal
    let s1 = "concurrent-create";
    f.reserve_session(s1).await;
    let left_client = f.client_access().clone();
    let right_client = f.client_access().clone();
    let left_did = f.did().to_owned();
    let right_did = f.did().to_owned();
    let left = tokio::spawn(async move {
        crate::goal::create_goal_for_session_from_access(
            &left_client,
            &left_did,
            s1,
            "Concurrent objective",
            None,
        )
        .await
    });
    let right = tokio::spawn(async move {
        crate::goal::create_goal_for_session_from_access(
            &right_client,
            &right_did,
            s1,
            "Concurrent objective",
            None,
        )
        .await
    });
    left.await.unwrap().unwrap();
    right.await.unwrap().unwrap();
    assert_eq!(
        f.client_access()
            .execute(r#"{ Goal(filter: { session_id: { _eq: "concurrent-create" } }) { _docID } }"#)
            .await
            .unwrap()["data"]["Goal"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // continuation_sequence_exhaustion_rejects_claims_without_retry_charge
    for retry in [false, true] {
        let session = if retry {
            "seq-exhaust-retry"
        } else {
            "seq-exhaust-normal"
        };
        f.reserve_session(session).await;
        let last_parent = format!("last-parent-{retry}");
        let blocked_parent = format!("must-not-be-claimed-{retry}");
        f.seed_request(session, &last_parent, "completed").await;
        f.seed_request(session, &blocked_parent, "completed").await;
        let initial = seed_goal_from_access(
            f.agent_access(),
            f.did(),
            session,
            Some("Never reuse an exhausted continuation sequence"),
            Some(GoalStatus::Active),
            None,
            None,
        )
        .await
        .unwrap();
        seed_goal_fields_from_access(
            f.agent_access(),
            &initial,
            &format!(
                "continuation_sequence: {}, infrastructure_retry_count: 0, last_failure: null, last_continued_from_request_id: null",
                i64::MAX - 1
            ),
        )
        .await;
        let before = f.load_goal(f.agent_access(), session).await.unwrap();
        let claimed = if retry {
            crate::goal::claim_retry_continuation_from_access(
                f.agent_access(),
                &before,
                &last_parent,
                1,
                "failed",
            )
            .await
        } else {
            crate::goal::claim_continuation_from_access(f.agent_access(), &before, &last_parent)
                .await
        };
        assert!(claimed.expect("MAX-1 must permit one final sequence advance"));
        let exhausted = f.load_goal(f.agent_access(), session).await.unwrap();
        assert_eq!(exhausted.continuation_sequence(), i64::MAX);
        assert_eq!(
            exhausted.last_continued_from_request_id.as_deref(),
            Some(last_parent.as_str())
        );
        assert_eq!(exhausted.infrastructure_retry_count, Some(i64::from(retry)));
        let rejected = if retry {
            crate::goal::claim_retry_continuation_from_access(
                f.agent_access(),
                &exhausted,
                &blocked_parent,
                2,
                "must-not-be-charged",
            )
            .await
        } else {
            crate::goal::claim_continuation_from_access(
                f.agent_access(),
                &exhausted,
                &blocked_parent,
            )
            .await
        };
        assert!(
            rejected.is_err(),
            "exhaustion must fail explicitly: {rejected:?}"
        );
        let after = f.load_goal(f.agent_access(), session).await.unwrap();
        assert_eq!(
            serde_json::to_value(&after).unwrap(),
            serde_json::to_value(&exhausted).unwrap(),
            "rejected claim must not change watermark, retry charge, failure, or timestamp"
        );
    }

    // stale_usage_refresh_preserves_completed_goal_time_accounting
    let s2 = "stale-usage-time";
    f.reserve_session(s2).await;
    let initial = seed_goal_from_access(
        f.agent_access(),
        f.did(),
        s2,
        Some("A stale observer cannot restart completed goal accounting"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .unwrap();
    seed_goal_fields_from_access(
        f.agent_access(),
        &initial,
        "active_time_seconds: 7, active_started_at: null",
    )
    .await;
    let stale = f.load_goal(f.agent_access(), s2).await.unwrap();
    seed_goal_from_access(
        f.client_access(),
        f.did(),
        s2,
        None,
        Some(GoalStatus::Complete),
        None,
        None,
    )
    .await
    .unwrap();
    seed_goal_fields_from_access(
        f.agent_access(),
        &stale,
        "active_time_seconds: 73, active_started_at: null",
    )
    .await;
    let completed = f.load_goal(f.agent_access(), s2).await.unwrap();
    assert_eq!(completed.status, "complete");
    assert_eq!(completed.active_time_seconds, Some(73));
    assert!(completed.active_started_at.is_none());
    crate::goal::refresh_goal_usage_from_access(f.agent_access(), &stale)
        .await
        .expect("stale usage observation must safely skip obsolete accounting writes");
    let after = f.load_goal(f.agent_access(), s2).await.unwrap();
    assert_eq!(after.status, "complete");
    assert_eq!(after.active_time_seconds, completed.active_time_seconds);
    assert_eq!(after.active_started_at, completed.active_started_at);
    assert_eq!(
        after.continuation_sequence(),
        completed.continuation_sequence()
    );
}

async fn scenario_operator_resume_variants(f: &GoalFixture) {
    // concurrent_operator_resumes_publish_one_child
    let s1 = "resume-concurrent";
    f.reserve_session(s1).await;
    f.seed_request_at(
        s1,
        "concurrent-parent",
        "interrupted",
        "2026-07-15T00:00:00Z",
    )
    .await;
    seed_goal_from_access(
        f.client_access(),
        f.did(),
        s1,
        Some("One operator continuation"),
        Some(GoalStatus::Paused),
        None,
        None,
    )
    .await
    .unwrap();
    let (left, right) = tokio::join!(
        crate::goal::resume_goal_request(
            f.client_access(),
            &f.home.user,
            f.did(),
            s1,
            "concurrent-parent"
        ),
        crate::goal::resume_goal_request(
            f.client_access(),
            &f.home.user,
            f.did(),
            s1,
            "concurrent-parent"
        ),
    );
    let (left, right) = (left.unwrap(), right.unwrap());
    assert_ne!(left.created, right.created);
    assert_eq!(
        (left.request_id, left.doc_id),
        (right.request_id, right.doc_id)
    );
    assert_eq!(
        goal_children_in_session(f.agent_access(), s1).await.len(),
        1
    );
    let goal = f.load_goal(f.client_access(), s1).await.unwrap();
    assert_eq!(goal.parsed_status(), Some(GoalStatus::Active));
    assert_eq!(goal.continuation_sequence(), 1);

    // operator_resume_preserves_pending_wrapup_in_child_policy
    let s2 = "resume-retained-wrapup";
    f.reserve_session(s2).await;
    f.seed_request_at(s2, "wrapup-parent", "interrupted", "2026-07-15T00:00:00Z")
        .await;
    let paused = seed_goal_from_access(
        f.client_access(),
        f.did(),
        s2,
        Some("Finish the already requested wrap-up"),
        Some(GoalStatus::Paused),
        None,
        None,
    )
    .await
    .unwrap();
    seed_goal_fields_from_access(
        f.agent_access(),
        &paused,
        "wrapup_requested: true, wrapup_completed: false",
    )
    .await;
    let receipt = crate::goal::resume_goal_request(
        f.client_access(),
        &f.home.user,
        f.did(),
        s2,
        "wrapup-parent",
    )
    .await
    .unwrap();
    let goal = f.load_goal(f.client_access(), s2).await.unwrap();
    assert_eq!(goal.parsed_status(), Some(GoalStatus::BudgetLimited));
    assert_eq!(goal.wrapup_requested, Some(true));
    assert_eq!(goal.wrapup_completed, Some(false));
    let child = operator_resume_child_row_from_access(f.client_access(), &receipt.doc_id).await;
    assert_eq!(
        child["input"]
            .pointer("/goal_continuation/wrapup")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    let content = child["content"].as_str().unwrap();
    assert!(content.contains("one wrap-up turn"), "{content}");
    assert!(content.contains("do not start new work"), "{content}");

    // operator_resume_accepts_same_second_child_that_sorts_before_root
    for (idx, parent_id) in ["graph-parent", "task-goal-request:parent"]
        .into_iter()
        .enumerate()
    {
        let session = format!("resume-same-second-{idx}");
        let (child_id, child_doc) =
            seed_same_second_canonical_goal_child(f, &session, parent_id, true).await;
        let receipt = crate::goal::resume_goal_request(
            f.client_access(),
            &f.home.user,
            f.did(),
            &session,
            &child_id,
        )
        .await
        .expect("canonical child is latest despite its lexical position");
        let next = operator_resume_child_row_from_access(f.client_access(), &receipt.doc_id).await;
        assert_eq!(next["caused_by_parent_request_id"], child_id);
        assert_eq!(next["caused_by_parent_request_doc_id"], child_doc);
        let goal = f.load_goal(f.client_access(), &session).await.unwrap();
        assert_eq!(goal.continuation_sequence(), 2);
        assert_eq!(goal.parsed_status(), Some(GoalStatus::Active));
    }
}

#[tokio::test]
async fn goal_cas_idempotency_baseline_and_operator_resume_scenarios() {
    let f = GoalFixture::start("goal-cas-baseline-resume").await;
    scenario_cas_and_clear(&f).await;
    scenario_baselines_and_usage(&f).await;
    scenario_concurrent_create_exhaustion_and_time_accounting(&f).await;
    scenario_operator_resume_variants(&f).await;
}

async fn scenario_nested_workspace_lineage(f: &GoalFixture) {
    let session = "lineage-workspace";
    let (parent_doc_id, _) = seed_lineage_parent(
        f,
        session,
        "parent-nested-workspace",
        "completed",
        "workspace-goal",
        "seal-hash",
    )
    .await;
    publish_owned_terminal_activity(
        f.agent_access(),
        f.home.agent_identity.as_ref(),
        &parent_doc_id,
        "durable progress",
    )
    .await;
    seed_goal_from_access(
        f.agent_access(),
        f.did(),
        session,
        Some("Preserve nested workspace scope"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .unwrap();
    let (mut source, _tx) = f.ready_source().await;
    next_intent(&mut source, "lineage continuation").await;
    let children = goal_children_in_session(f.agent_access(), session).await;
    assert_eq!(children.len(), 1);
    let child = &children[0];
    let visible = goal_children_in_session(f.client_access(), session).await;
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].doc_id, child.doc_id);
    assert_eq!(child.request_hop, Some(2));
    assert_eq!(child.workspace_id.as_deref(), Some("workspace-goal"));
    assert_eq!(child.workspace_authority.as_deref(), Some("readOnly"));
    assert_eq!(child.workspace_owner_node_did.as_deref(), Some(f.did()));
    assert_eq!(child.workspace_seal_hash.as_deref(), Some("seal-hash"));
    let child_identity = (
        child.doc_id.clone(),
        child.request_id.clone(),
        child.retry_key.clone(),
    );
    let continuation_sequence = f
        .load_goal(f.agent_access(), session)
        .await
        .unwrap()
        .continuation_sequence;
    drop(source);
    for _ in 0..2 {
        let recovered =
            gents_session::request::lifecycle::RequestLifecycle::recover_all_with_identities(
                &f.home.node,
                f.did(),
                std::slice::from_ref(&f.home.agent_identity),
            )
            .await
            .unwrap();
        assert_eq!(recovered.requests_recovered, 0);
        let (mut restarted, _restart_tx) = f.ready_source().await;
        assert_settles_without_fire(
            &mut restarted,
            "repeated maintenance and source reconciliation must not emit another continuation",
        )
        .await;
        let children = goal_children_in_session(f.agent_access(), session).await;
        assert_eq!(children.len(), 1);
        let child = &children[0];
        assert_eq!(
            (
                child.doc_id.clone(),
                child.request_id.clone(),
                child.retry_key.clone()
            ),
            child_identity
        );
        assert_eq!(child.request_hop, Some(2));
        assert_eq!(child.workspace_id.as_deref(), Some("workspace-goal"));
        assert_eq!(child.workspace_authority.as_deref(), Some("readOnly"));
        assert_eq!(child.workspace_owner_node_did.as_deref(), Some(f.did()));
        assert_eq!(child.workspace_seal_hash.as_deref(), Some("seal-hash"));
        assert_eq!(
            f.load_goal(f.agent_access(), session)
                .await
                .unwrap()
                .continuation_sequence,
            continuation_sequence
        );
    }
    f.clear_goal(session).await;
}

async fn scenario_delimiter_and_single_child(f: &GoalFixture) {
    // delimiter_ambiguous_goal_parent_pairs_get_distinct_retry_keys
    for (session, parent) in [("b:c", "d"), ("b", "c:d")] {
        f.reserve_session(session).await;
        let doc_id = f
            .seed_request_at(session, parent, "completed", "2026-11-04T00:00:00Z")
            .await;
        publish_owned_terminal_activity(
            f.agent_access(),
            f.home.agent_identity.as_ref(),
            &doc_id,
            "progress",
        )
        .await;
        seed_goal_from_access(
            f.agent_access(),
            f.did(),
            session,
            Some(&format!("goal for {session}")),
            Some(GoalStatus::Active),
            None,
            None,
        )
        .await
        .unwrap();
    }
    let (mut source, _tx) = f.ready_source().await;
    for _ in 0..2 {
        next_intent(&mut source, "delimiter continuation").await;
    }
    let mut keys = RapidHashSet::new();
    for session in ["b:c", "b"] {
        let children = goal_children_in_session(f.agent_access(), session).await;
        assert_eq!(children.len(), 1);
        keys.insert(children[0].retry_key.clone().expect("retry key"));
        f.clear_goal(session).await;
    }
    assert_eq!(keys.len(), 2);

    // completed_request_materializes_exactly_one_same_session_goal_child
    let session = "goal-exactly-once";
    f.seed_recovery_parent(session, "parent-complete", "completed")
        .await;
    let goal = seed_goal_from_access(
        f.agent_access(),
        f.did(),
        session,
        Some("Finish the durable objective"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .unwrap();
    let (mut source, _tx) = f.ready_source().await;
    let intent = next_intent(&mut source, "goal continuation").await;
    assert!(intent.pre_materialized_request_id.is_some());
    let children = goal_children_in_session(f.agent_access(), session).await;
    assert_eq!(children.len(), 1);
    let child = &children[0];
    let visible = goal_children_in_session(f.client_access(), session).await;
    assert_eq!(
        visible.len(),
        1,
        "the session owner must see the continuation"
    );
    assert_eq!(visible[0].doc_id, child.doc_id);
    assert_eq!(child.session_id, session);
    assert_eq!(child.agent_did.as_deref(), Some(f.agent_did()));
    assert_eq!(
        child.caused_by_parent_request_id.as_deref(),
        Some("parent-complete")
    );
    assert_eq!(
        child.caused_by_trigger_id.as_deref(),
        Some(goal.goal_id.as_str())
    );
    assert_eq!(child.caused_by_trigger_kind.as_deref(), Some("goal"));
    assert_settles_without_fire(
        &mut source,
        "a pending child must suppress duplicate continuation",
    )
    .await;
    assert_eq!(
        goal_children_in_session(f.agent_access(), session)
            .await
            .len(),
        1
    );
    f.clear_goal(session).await;
}

async fn scenario_foreign_collisions_and_restart_recovery(f: &GoalFixture) {
    use sha2::{Digest, Sha256};

    // foreign_request_id_collision_cannot_preempt_owned_goal_continuation
    let s1 = "foreign-req-collision";
    f.seed_recovery_parent(s1, "parent-owned-collision", "completed")
        .await;
    let goal = seed_goal_from_access(
        f.agent_access(),
        f.did(),
        s1,
        Some("Ignore foreign request IDs"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .unwrap();
    let digest = Sha256::digest(format!("{}\0parent-owned-collision", goal.goal_id).as_bytes());
    let collision_id = format!(
        "goal-cont-{:020}-{}",
        1,
        digest[..16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    create_visible_foreign_collision(f, &collision_id, None).await;
    let (mut source, _tx) = f.ready_source().await;
    next_intent(&mut source, "owned continuation despite foreign request_id").await;
    let children = goal_children_in_session(f.agent_access(), s1).await;
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].request_id, collision_id);
    assert_eq!(children[0].session_id, s1);
    f.clear_goal(s1).await;

    // foreign_retry_key_collision_is_rejected_instead_of_reused
    let s2 = "foreign-retry-collision";
    f.seed_recovery_parent(s2, "parent-owned-retry-collision", "completed")
        .await;
    let goal = seed_goal_from_access(
        f.agent_access(),
        f.did(),
        s2,
        Some("Reject foreign retry keys"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .unwrap();
    let digest =
        Sha256::digest(format!("{}\0parent-owned-retry-collision", goal.goal_id).as_bytes());
    let retry_key = format!(
        "goal-continuation:{}",
        digest[..16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    create_visible_foreign_collision(f, "foreign-retry-collision", Some(&retry_key)).await;
    let (mut source, _tx) = f.ready_source().await;
    assert_settles_without_fire(
        &mut source,
        "a conflicting retry key must not be reported as an owned continuation",
    )
    .await;
    assert!(goal_children_in_session(f.agent_access(), s2)
        .await
        .is_empty());
    f.clear_goal(s2).await;

    // restart_reconcile_does_not_duplicate_a_goal_continuation
    let s3 = "restart-no-dup";
    f.seed_recovery_parent(s3, "parent-restart", "completed")
        .await;
    seed_goal_from_access(
        f.agent_access(),
        f.did(),
        s3,
        Some("Continue exactly once across restart"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .unwrap();
    let (mut first, _tx) = f.ready_source().await;
    next_intent(&mut first, "first source").await;
    drop(first);
    let (mut restarted, _tx) = f.ready_source().await;
    assert_settles_without_fire(&mut restarted, "restart must not emit a second child").await;
    assert_eq!(
        goal_children_in_session(f.agent_access(), s3).await.len(),
        1
    );
    f.clear_goal(s3).await;

    // restart_materializes_a_claimed_continuation_after_crash
    let s4 = "restart-claimed";
    f.seed_recovery_parent(s4, "parent-claimed", "completed")
        .await;
    let goal = seed_goal_from_access(
        f.agent_access(),
        f.did(),
        s4,
        Some("Recover the claimed continuation"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(
        crate::goal::claim_continuation_from_access(f.agent_access(), &goal, "parent-claimed")
            .await
            .unwrap()
    );
    assert!(goal_children_in_session(f.agent_access(), s4)
        .await
        .is_empty());
    let (mut restarted, _tx) = f.ready_source().await;
    next_intent(&mut restarted, "recovery source").await;
    assert_eq!(
        goal_children_in_session(f.agent_access(), s4).await.len(),
        1
    );
    f.clear_goal(s4).await;

    // restart_materializes_claimed_wrapup_retry_without_charging_it_twice
    let s5 = "restart-claimed-wrapup-retry";
    f.seed_recovery_parent(s5, "parent-claimed-wrapup-retry", "failed")
        .await;
    let goal = seed_goal_from_access(
        f.agent_access(),
        f.did(),
        s5,
        Some("Recover one charged wrap-up retry"),
        Some(GoalStatus::BudgetLimited),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(crate::goal::claim_retry_continuation_from_access(
        f.agent_access(),
        &goal,
        "parent-claimed-wrapup-retry",
        1,
        "failed",
    )
    .await
    .unwrap());
    let (mut restarted, _tx) = f.ready_source().await;
    next_intent(&mut restarted, "wrap-up retry recovery").await;
    let recovered = f.load_goal(f.agent_access(), s5).await.unwrap();
    assert_eq!(recovered.infrastructure_retry_count, Some(1));
    let children = goal_children_in_session(f.agent_access(), s5).await;
    assert_eq!(children.len(), 1);
    assert_eq!(
        children[0].input.as_ref().unwrap()["goal_continuation"]["wrapup"],
        true
    );
    f.clear_goal(s5).await;
}

async fn scenario_idleness_retired_inputs_interrupted_and_same_second(f: &GoalFixture) {
    // any_newer_active_request_blocks_goal_continuation_for_the_whole_session
    let s1 = "session-idle-block";
    f.seed_recovery_parent(s1, "older-complete", "completed")
        .await;
    f.seed_request_at(s1, "newer-manual", "pending", "2026-11-04T00:01:00Z")
        .await;
    seed_goal_from_access(
        f.agent_access(),
        f.did(),
        s1,
        Some("Wait for session idleness"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .unwrap();
    let (mut source, _tx) = f.ready_source().await;
    assert_settles_without_fire(&mut source, "the goal owner must not continue").await;
    assert!(goal_children_in_session(f.agent_access(), s1)
        .await
        .is_empty());
    f.clear_goal(s1).await;

    // retired_queued_inputs_do_not_pause_completed_goal_execution
    for (name, state, reason) in [
        (
            "folded",
            "superseded",
            gents_session::request::lifecycle::FOLDED_REASON,
        ),
        (
            "replaced",
            "superseded",
            gents_session::request::lifecycle::REPLACED_REASON,
        ),
        (
            "removed",
            "interrupted",
            gents_session::request::lifecycle::REMOVED_REASON,
        ),
    ] {
        let session = format!("retired-input-{name}");
        let parent_id = format!("execution-parent-{name}");
        let parent = f
            .seed_recovery_parent(&session, &parent_id, "completed")
            .await;
        let input = f
            .seed_request(&session, &format!("zz-retired-input-{name}"), state)
            .await;
        f.agent_access()
            .write(
                "test.goal_retired_input",
                &format!(
                    r#"mutation {{ update_AgentRequest(docID: "{}", input: {{
                        failure_reason: "{}", superseded_by_request: "{}",
                        superseded_by_request_doc_id: "{}"
                    }}) {{ _docID }} }}"#,
                    escape_graphql_string(&input),
                    escape_graphql_string(reason),
                    escape_graphql_string(&parent_id),
                    escape_graphql_string(&parent),
                ),
            )
            .await
            .unwrap();
        seed_goal_from_access(
            f.agent_access(),
            f.did(),
            &session,
            Some("Continue the existing objective"),
            Some(GoalStatus::Active),
            None,
            None,
        )
        .await
        .unwrap();
        let (mut source, _tx) = f.ready_source().await;
        next_intent(
            &mut source,
            &format!("{name}: retired input prevented continuation"),
        )
        .await;
        let goal = f.load_goal(f.agent_access(), &session).await.unwrap();
        assert_eq!(goal.parsed_status(), Some(GoalStatus::Active), "{name}");
        assert_eq!(
            goal_children_in_session(f.agent_access(), &session)
                .await
                .len(),
            1,
            "{name}"
        );
        f.clear_goal(&session).await;
    }

    // interrupted_terminal_pauses_instead_of_self_continuing
    let s2 = "interrupted-pauses";
    f.seed_recovery_parent(s2, "parent-interrupted", "interrupted")
        .await;
    seed_goal_from_access(
        f.agent_access(),
        f.did(),
        s2,
        Some("Respect human interruption"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .unwrap();
    let (mut source, _tx) = f.ready_source().await;
    f.wait_for_goal(s2, &mut source, "interrupted goal to pause", |goal| {
        goal.parsed_status() == Some(GoalStatus::Paused)
    })
    .await;
    assert!(goal_children_in_session(f.agent_access(), s2)
        .await
        .is_empty());
    f.clear_goal(s2).await;

    // goal_source_continues_same_second_child_that_sorts_before_root
    for (idx, parent_id) in ["graph-parent", "task-goal-request:parent"]
        .into_iter()
        .enumerate()
    {
        let session = format!("source-same-second-{idx}");
        let (child_id, _) =
            seed_same_second_canonical_goal_child(f, &session, parent_id, false).await;
        let (mut controller, _tx) = f.ready_source().await;
        let intent = next_intent(
            &mut controller,
            "causal child must not be hidden by lexical root ordering",
        )
        .await;
        let next_id = intent
            .pre_materialized_request_id
            .expect("materialized child");
        let children = goal_children_in_session(f.agent_access(), &session).await;
        let next = children
            .iter()
            .find(|row| row.request_id == next_id)
            .unwrap();
        assert_eq!(
            next.caused_by_parent_request_id.as_deref(),
            Some(child_id.as_str())
        );
        let goal = f.load_goal(f.agent_access(), &session).await.unwrap();
        assert_eq!(goal.continuation_sequence(), 2);
        assert_eq!(
            goal.last_continued_from_request_id.as_deref(),
            Some(child_id.as_str())
        );
        f.clear_goal(&session).await;
    }
}

#[tokio::test]
async fn goal_source_continuation_and_collision_scenarios() {
    let f = GoalFixture::start("goal-source-continuation").await;
    scenario_nested_workspace_lineage(&f).await;
    scenario_delimiter_and_single_child(&f).await;
    scenario_foreign_collisions_and_restart_recovery(&f).await;
    scenario_idleness_retired_inputs_interrupted_and_same_second(&f).await;
}

async fn scenario_token_budget_wrapup_and_failed_wrapup_retries(f: &GoalFixture) {
    // token_budget_materializes_one_wrapup_and_never_repeats_it
    let s1 = "budget-wrapup";
    f.seed_recovery_parent(s1, "parent-budget", "completed")
        .await;
    let (parent_doc_id, session_doc_id) = f.request_doc_and_session("parent-budget").await;
    seed_goal_from_access(
        f.agent_access(),
        f.did(),
        s1,
        Some("Stop at the durable budget"),
        Some(GoalStatus::Active),
        Some(Some(10)),
        None,
    )
    .await
    .unwrap();
    f.seed_usage(
        s1,
        "parent-budget",
        &parent_doc_id,
        "goal-budget-call",
        (100, 5, 90),
    )
    .await;
    let foreign_dir = tempfile::tempdir().unwrap();
    let foreign_node = gents_store::identity::KeyIdentity::load_or_create(
        foreign_dir.path().join("foreign-node.key"),
        None,
    )
    .unwrap();
    let foreign_did = foreign_node.did().to_owned();
    let agent_did = f.agent_did().to_owned();
    let doc = f.agent_access().transact_write("test.goal_foreign_usage", |txn| {
        let foreign_did = &foreign_did;
        let agent_did = &agent_did;
        let parent_doc_id = &parent_doc_id;
        let session_doc_id = &session_doc_id;
        Box::pin(async move {
            let input = serde_json::json!({
                "call_id": "goal-budget-call-foreign", "node_did": foreign_did,
                "agent_did": agent_did, "session_id": s1, "request_id": "parent-budget",
                "request_doc_id": parent_doc_id, "call_seq": 1, "call_kind": "inference",
                "attempt": 1, "call_state": "completed", "prompt_tokens": 10000,
                "completion_tokens": 10000, "cached_input_tokens": 0
            });
            let result = txn.execute_with_variables(
                "mutation($input: InferenceCallMutationInputArg!) {add_InferenceCall(input:$input) {_docID}}",
                &serde_json::json!({"input": input}),
            ).await?;
            let doc = gents_store::graphql::created_doc_id(&result, "InferenceCall")?;
            txn.add_resource_relationship("InferenceCall", &doc, "parent", "sessions", session_doc_id).await?;
            Ok::<_, anyhow::Error>(doc)
        })
    }).await.unwrap();
    let observed = f
        .agent_access()
        .execute(&format!(
            r#"{{InferenceCall(docID: "{}") {{_docID node_did request_doc_id}}}}"#,
            escape_graphql_string(&doc)
        ))
        .await
        .unwrap();
    assert_eq!(
        observed["data"]["InferenceCall"][0]["node_did"],
        foreign_node.did()
    );
    assert_eq!(
        observed["data"]["InferenceCall"][0]["request_doc_id"],
        parent_doc_id
    );

    let replayed = seed_goal_from_access(
        f.agent_access(),
        f.did(),
        s1,
        Some("Stop at the durable budget"),
        Some(GoalStatus::Active),
        Some(Some(10)),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        (replayed.token_usage_baseline, replayed.tokens_used),
        (Some(0), Some(0))
    );

    let (mut source, _tx) = f.ready_source().await;
    next_intent(&mut source, "goal continuation").await;
    let children = goal_children_in_session(f.agent_access(), s1).await;
    assert_eq!(children.len(), 1);
    let user_children = goal_children_in_session(f.client_access(), s1).await;
    assert_eq!(user_children.len(), 1);
    assert_eq!(user_children[0].doc_id, children[0].doc_id);
    assert_eq!(
        children[0]
            .input
            .as_ref()
            .unwrap()
            .pointer("/goal_continuation/wrapup"),
        Some(&serde_json::json!(true))
    );
    set_request_state_as(f.agent_access(), &children[0].doc_id, "completed").await;
    publish_owned_terminal_activity(
        f.agent_access(),
        f.home.agent_identity.as_ref(),
        &children[0].doc_id,
        "final durable wrapup",
    )
    .await;
    let goal = f
        .wait_for_goal(s1, &mut source, "completed wrapup", |goal| {
            goal.parsed_status() == Some(GoalStatus::BudgetLimited)
                && goal.wrapup_completed == Some(true)
        })
        .await;
    assert_settles_without_fire(&mut source, "a completed wrapup must not continue").await;
    assert_eq!(goal.tokens_used, Some(105));
    assert_eq!(goal.wrapup_requested, Some(true));
    assert_eq!(
        goal_children_in_session(f.agent_access(), s1).await.len(),
        1
    );
    let child_identity = (
        children[0].doc_id.clone(),
        children[0].request_id.clone(),
        children[0].retry_key.clone(),
    );
    let continuation_sequence = goal.continuation_sequence;
    assert_eq!(
        session_token_usage(&f.home.node, &f.agent_did().parse().unwrap(), f.did(), s1)
            .await
            .unwrap(),
        105
    );
    drop(source);
    for _ in 0..2 {
        let recovered =
            gents_session::request::lifecycle::RequestLifecycle::recover_all_with_identities(
                &f.home.node,
                f.did(),
                std::slice::from_ref(&f.home.agent_identity),
            )
            .await
            .unwrap();
        assert_eq!(
            (recovered.requests_recovered, recovered.responses_recovered),
            (0, 0)
        );
        let (mut restarted, _tx) = f.ready_source().await;
        assert_settles_without_fire(
            &mut restarted,
            "recovery and goal reconciliation must not emit a second wrapup",
        )
        .await;
        let current = f.load_goal(f.agent_access(), s1).await.unwrap();
        assert_eq!(current.parsed_status(), Some(GoalStatus::BudgetLimited));
        assert_eq!(current.tokens_used, Some(105));
        assert_eq!(current.continuation_sequence, continuation_sequence);
        assert_eq!(current.wrapup_completed, Some(true));
        let after = goal_children_in_session(f.agent_access(), s1).await;
        assert_eq!(after.len(), 1);
        assert_eq!(
            (
                after[0].doc_id.clone(),
                after[0].request_id.clone(),
                after[0].retry_key.clone()
            ),
            child_identity
        );
    }
    f.clear_goal(s1).await;

    // failed_wrapup_retries_twice_then_is_durably_abandoned
    let s2 = "wrapup-retry-bound";
    let parent_doc = f
        .seed_recovery_parent(s2, "parent-wrapup-retry", "completed")
        .await;
    seed_goal_from_access(
        f.agent_access(),
        f.did(),
        s2,
        Some("Bound failed wrap-up retries"),
        Some(GoalStatus::Active),
        Some(Some(1)),
        None,
    )
    .await
    .unwrap();
    f.seed_usage(
        s2,
        "parent-wrapup-retry",
        &parent_doc,
        "wrapup-budget-call",
        (2, 0, 0),
    )
    .await;
    let (mut source, _tx) = f.ready_source().await;
    for expected_children in 1..=3 {
        next_intent(&mut source, &format!("wrapup child {expected_children}")).await;
        let children = goal_children_in_session(f.agent_access(), s2).await;
        assert_eq!(children.len(), expected_children);
        let child = children
            .iter()
            .find(|child| child.lifecycle_state.as_deref() == Some("pending"))
            .expect("new pending wrap-up child");
        set_request_state_as(f.agent_access(), &child.doc_id, "failed").await;
    }
    let goal = f
        .wait_for_goal(s2, &mut source, "failed wrap-up abandonment", |goal| {
            goal.parsed_status() == Some(GoalStatus::BudgetLimited)
                && goal.wrapup_completed == Some(true)
        })
        .await;
    assert_eq!(goal.infrastructure_retry_count, Some(2));
    assert!(goal
        .last_failure
        .as_deref()
        .is_some_and(|reason| reason.contains("after 2 retries")));
    assert_settles_without_fire(
        &mut source,
        "bounded failed wrap-up must not spawn a fourth child",
    )
    .await;
    assert_eq!(
        goal_children_in_session(f.agent_access(), s2).await.len(),
        3
    );
    f.clear_goal(s2).await;
}

async fn scenario_usage_limits_and_passed_resets(f: &GoalFixture) {
    let (openrouter_reason, _, _) = crate::goal::blocked_turn::permit_recorded(
        r#"HttpError: Invalid status code 402 Payment Required with message: {"error":{"code":402,"message":"Insufficient credits. Add more using https://openrouter.ai/credits"}}"#,
        chrono::Utc::now(),
    );
    let limit_cases: &[(&str, &str, Vec<FailedInferenceCall<'_>>, &str)] = &[
        (
            "usage-limit-quota",
            "req-limit-quota",
            vec![FailedInferenceCall {
                call_id: "usage-limited-call",
                call_kind: "inference",
                attempt: 1,
                ended_at: "2026-07-15T00:00:01Z",
                failure_reason: "provider insufficient_quota: credit balance exhausted",
            }],
            "insufficient_quota",
        ),
        (
            "usage-limit-compaction",
            "req-limit-compaction",
            vec![FailedInferenceCall {
                call_id: "compaction-call",
                call_kind: "compaction",
                attempt: 1,
                ended_at: "2026-07-15T00:00:10Z",
                failure_reason: RENDERED_LIMIT,
            }],
            "resets at 2026-07-15T05:00:00Z",
        ),
        (
            "usage-limit-later-wins",
            "req-limit-later",
            vec![
                FailedInferenceCall {
                    call_id: "earlier-retry",
                    call_kind: "inference",
                    attempt: 2,
                    ended_at: "2026-07-15T00:00:05Z",
                    failure_reason: "HttpError: Invalid status code 500",
                },
                FailedInferenceCall {
                    call_id: "later-compaction",
                    call_kind: "compaction",
                    attempt: 1,
                    ended_at: "2026-07-15T00:00:10Z",
                    failure_reason: RENDERED_LIMIT,
                },
            ],
            "resets at 2026-07-15T05:00:00Z",
        ),
        (
            "usage-limit-openrouter",
            "req-limit-openrouter",
            vec![FailedInferenceCall {
                call_id: "openrouter-credits-call",
                call_kind: "inference",
                attempt: 1,
                ended_at: "2026-07-15T00:00:01Z",
                failure_reason: &openrouter_reason,
            }],
            "Insufficient credits",
        ),
    ];
    for (session, req_id, calls, expected_fragment) in limit_cases {
        f.seed_recovery_parent(session, req_id, "failed").await;
        let (request_doc, session_doc) = f.request_doc_and_session(req_id).await;
        for call in calls {
            f.seed_failed_call(
                session,
                req_id,
                &request_doc,
                &session_doc,
                FailedInferenceCall {
                    call_id: call.call_id,
                    call_kind: call.call_kind,
                    attempt: call.attempt,
                    ended_at: call.ended_at,
                    failure_reason: call.failure_reason,
                },
            )
            .await;
        }
        seed_goal_from_access(
            f.agent_access(),
            f.did(),
            session,
            Some("Stop on provider limit"),
            Some(GoalStatus::Active),
            None,
            None,
        )
        .await
        .unwrap();
        let (mut source, _tx) = f.ready_source().await;
        let goal = f
            .wait_for_goal(session, &mut source, session, |goal| {
                goal.parsed_status() == Some(GoalStatus::UsageLimited)
            })
            .await;
        assert_settles_without_fire(&mut source, "a usage-limited goal must not continue").await;
        assert!(
            goal.last_failure
                .as_deref()
                .is_some_and(|reason| reason.contains(expected_fragment)),
            "{session}: {:?}",
            goal.last_failure
        );
        f.clear_goal(session).await;
    }

    // a_database_update_leaves_a_passed_reset_to_the_rescan_tick
    {
        let session = "reset-update-tick";
        f.seed_recovery_parent(session, "req-reset-tick", "failed")
            .await;
        let (req_doc, sess_doc) = f.request_doc_and_session("req-reset-tick").await;
        f.seed_failed_call(
            session,
            "req-reset-tick",
            &req_doc,
            &sess_doc,
            FailedInferenceCall {
                call_id: "limited-call-tick",
                call_kind: "inference",
                attempt: 1,
                ended_at: "2026-07-15T00:00:10Z",
                failure_reason: RENDERED_LIMIT,
            },
        )
        .await;
        seed_goal_from_access(
            f.agent_access(),
            f.did(),
            session,
            Some("Resume at the reported reset"),
            Some(GoalStatus::Active),
            None,
            None,
        )
        .await
        .unwrap();
        seed_goal_from_access(
            f.client_access(),
            f.did(),
            session,
            None,
            None,
            None,
            Some(true),
        )
        .await
        .unwrap();
        f.publish_readiness(gents_protocol::row::NodeReadinessProcessState::Ready, None)
            .await;
        let updates = MockUpdateSubscriptionSource::new();
        let (_tx, rx) = watch::channel(f.agent_snapshot());
        let mut source = GoalSource::with_subscription_source(
            Arc::new(updates.clone()),
            rx,
            f.home.node.clone(),
            CancellationToken::new(),
        )
        .with_rescan_interval(Duration::from_secs(3600));
        f.wait_for_goal(session, &mut source, "usage limit before tick", |goal| {
            goal.parsed_status() == Some(GoalStatus::UsageLimited)
        })
        .await;
        let goal_collection_id =
            gents_store::subscription_source::collection_ids(&f.home.node, ["Goal"])
                .unwrap()
                .remove(0);
        updates.publish_update(goal_collection_id, "goal-doc");
        assert!(
            source.rescan_to_settled(false).await.is_none(),
            "an update-triggered rescan must leave the passed reset to the tick"
        );
        assert_eq!(
            f.load_goal(f.agent_access(), session)
                .await
                .unwrap()
                .parsed_status(),
            Some(GoalStatus::UsageLimited)
        );
        assert!(goal_children_in_session(f.agent_access(), session)
            .await
            .is_empty());
        f.clear_goal(session).await;
    }

    // opted_in_goal_resumes_at_its_reported_reset (both opted_in = true and false)
    for opted_in in [true, false] {
        let session = if opted_in {
            "reset-opted-in"
        } else {
            "reset-not-opted-in"
        };
        let req_id = format!("req-{session}");
        f.seed_recovery_parent(session, &req_id, "failed").await;
        let (req_doc, sess_doc) = f.request_doc_and_session(&req_id).await;
        f.seed_failed_call(
            session,
            &req_id,
            &req_doc,
            &sess_doc,
            FailedInferenceCall {
                call_id: &format!("call-{session}"),
                call_kind: "inference",
                attempt: 1,
                ended_at: "2026-07-15T00:00:10Z",
                failure_reason: RENDERED_LIMIT,
            },
        )
        .await;
        seed_goal_from_access(
            f.agent_access(),
            f.did(),
            session,
            Some("Resume at the reported reset"),
            Some(GoalStatus::Active),
            None,
            None,
        )
        .await
        .unwrap();
        if opted_in {
            seed_goal_from_access(
                f.client_access(),
                f.did(),
                session,
                None,
                None,
                None,
                Some(true),
            )
            .await
            .unwrap();
        }
        let (mut source, _tx) = f.ready_source().await;
        f.wait_for_goal(session, &mut source, "provider reset decision", |goal| {
            if opted_in {
                goal.parsed_status() == Some(GoalStatus::Active)
                    && goal.continuation_sequence == Some(1)
            } else {
                goal.parsed_status() == Some(GoalStatus::UsageLimited)
            }
        })
        .await;
        if opted_in {
            assert_settles_without_fire(
                &mut source,
                "reset must not publish a second continuation",
            )
            .await;
        }
        let goal = f.load_goal(f.agent_access(), session).await.unwrap();
        let child_count = goal_children_in_session(f.agent_access(), session)
            .await
            .len();
        if opted_in {
            assert_eq!(
                (goal.parsed_status(), child_count),
                (Some(GoalStatus::Active), 1)
            );
        } else {
            assert_eq!(
                (goal.parsed_status(), child_count),
                (Some(GoalStatus::UsageLimited), 0)
            );
        }
        f.clear_goal(session).await;
    }
}

#[tokio::test]
async fn goal_source_budget_and_usage_limit_scenarios() {
    let f = GoalFixture::start("goal-source-budget-limits").await;
    scenario_token_budget_wrapup_and_failed_wrapup_retries(&f).await;
    scenario_usage_limits_and_passed_resets(&f).await;
}

async fn scenario_readiness_defer_and_race(f: &GoalFixture) {
    use gents_protocol::row::{
        AgentReadinessUnavailableReason as Reason, NodeReadinessProcessState as Process,
    };

    // unready_behavior_defers_goal_continuation_without_writes_or_retry_charge
    {
        let session = "readiness-wait";
        f.seed_recovery_parent(session, "parent-waits-for-readiness", "completed")
            .await;
        seed_goal_from_access(
            f.agent_access(),
            f.did(),
            session,
            Some("Wait for the behavior instead of spending retries"),
            Some(GoalStatus::Active),
            None,
            None,
        )
        .await
        .unwrap();
        f.publish_readiness(Process::Recovering, None).await;
        let before =
            serde_json::to_value(f.load_goal(f.agent_access(), session).await.unwrap()).unwrap();
        let (mut source, _tx) = f.unready_source();
        let mut previous = before["last_failure"].as_str().map(str::to_owned);
        for (process, reason) in [
            (Process::Recovering, None),
            (Process::Ready, Some(Reason::RuntimeConfigurationInvalid)),
            (Process::Ready, Some(Reason::BackendTemporarilyUnavailable)),
        ] {
            f.publish_readiness(process, reason).await;
            let waiting = serde_json::to_value(
                f.wait_for_goal(session, &mut source, "recorded readiness wait", |goal| {
                    goal.last_failure.as_deref().is_some_and(|r| {
                        r.starts_with(crate::goal::GOAL_READINESS_WAIT_PREFIX)
                            && Some(r) != previous.as_deref()
                    })
                })
                .await,
            )
            .unwrap();
            previous = waiting["last_failure"].as_str().map(str::to_owned);
            for field in [
                "status",
                "continuation_sequence",
                "infrastructure_retry_count",
                "tokens_used",
            ] {
                assert_eq!(
                    waiting[field], before[field],
                    "{process:?}/{reason:?}: {field}"
                );
            }
            assert_settles_without_fire(&mut source, "the goal owner must not continue").await;
            assert_eq!(
                serde_json::to_value(f.load_goal(f.agent_access(), session).await.unwrap())
                    .unwrap(),
                waiting,
                "{process:?}/{reason:?}: an unchanged wait must not rewrite the Goal"
            );
            assert!(goal_children_in_session(f.agent_access(), session)
                .await
                .is_empty());
        }
        f.publish_readiness(Process::Ready, None).await;
        next_intent(&mut source, "goal continuation after readiness recovered").await;
        let children = goal_children_in_session(f.agent_access(), session).await;
        assert_eq!(children.len(), 1);
        assert_eq!(
            children[0].caused_by_parent_request_id.as_deref(),
            Some("parent-waits-for-readiness")
        );
        let goal = f.load_goal(f.agent_access(), session).await.unwrap();
        assert_eq!(goal.parsed_status(), Some(GoalStatus::Active));
        assert_eq!(goal.continuation_sequence(), 1);
        assert_eq!(goal.infrastructure_retry_count.unwrap_or_default(), 0);
        assert_eq!(goal.last_failure, None);
        drop(source);
        let (mut restarted, _tx) = f.ready_source().await;
        assert_settles_without_fire(&mut restarted, "restart must not duplicate recovered child")
            .await;
        assert_eq!(
            goal_children_in_session(f.agent_access(), session)
                .await
                .len(),
            1
        );
        f.clear_goal(session).await;
    }

    // readiness_rejected_goal_children_never_spend_the_retry_budget
    {
        let session = "readiness-race";
        f.seed_recovery_parent(session, "parent-before-race", "completed")
            .await;
        seed_goal_from_access(
            f.agent_access(),
            f.did(),
            session,
            Some("Survive readiness races"),
            Some(GoalStatus::Active),
            None,
            None,
        )
        .await
        .unwrap();
        let (mut source, _tx) = f.ready_source().await;
        for round in 1..=(crate::goal::MAX_INFRASTRUCTURE_RETRIES as usize + 1) {
            next_intent(&mut source, &format!("round {round}")).await;
            let children = goal_children_in_session(f.agent_access(), session).await;
            assert_eq!(children.len(), round);
            let pending = children
                .iter()
                .find(|child| child.lifecycle_state.as_deref() == Some("pending"))
                .expect("pending goal child");
            assert_eq!(
                pending.input.as_ref().unwrap()["goal_continuation"]["wrapup"],
                false
            );
            f.publish_readiness(Process::Ready, Some(Reason::RuntimeConfigurationInvalid))
                .await;
            reject_before_claim(
                f.agent_access(),
                &pending.doc_id,
                Reason::RuntimeConfigurationInvalid.public_message(),
            )
            .await;
            assert_settles_without_fire(&mut source, "rejected child must wait for readiness")
                .await;
            let goal = f.load_goal(f.agent_access(), session).await.unwrap();
            assert_eq!(
                goal.parsed_status(),
                Some(GoalStatus::Active),
                "round {round}"
            );
            assert_eq!(
                goal.infrastructure_retry_count.unwrap_or_default(),
                0,
                "round {round}"
            );
            f.publish_readiness(Process::Ready, None).await;
        }
        next_intent(&mut source, "goal continuation after the final race").await;
        let children = goal_children_in_session(f.agent_access(), session).await;
        let pending = children
            .iter()
            .find(|child| child.lifecycle_state.as_deref() == Some("pending"))
            .expect("pending goal child");
        reject_before_claim(
            f.agent_access(),
            &pending.doc_id,
            Reason::RuntimeConfigurationInvalid.public_message(),
        )
        .await;
        let waiting = f
            .wait_for_goal(
                session,
                &mut source,
                "readiness wait after rejection",
                |goal| {
                    goal.last_failure
                        .as_deref()
                        .is_some_and(|r| r.contains("not been republished since its rejection"))
                },
            )
            .await;
        assert_eq!(
            goal_children_in_session(f.agent_access(), session)
                .await
                .len(),
            children.len()
        );
        assert_eq!(waiting.parsed_status(), Some(GoalStatus::Active));
        assert_eq!(waiting.infrastructure_retry_count.unwrap_or_default(), 0);
        f.publish_readiness(Process::Ready, None).await;
        next_intent(
            &mut source,
            "goal continuation after readiness was republished",
        )
        .await;
        assert_eq!(
            goal_children_in_session(f.agent_access(), session)
                .await
                .len(),
            children.len() + 1
        );
        let goal = f.load_goal(f.agent_access(), session).await.unwrap();
        assert_eq!(goal.parsed_status(), Some(GoalStatus::Active));
        assert_eq!(goal.infrastructure_retry_count.unwrap_or_default(), 0);
        assert_eq!(goal.last_failure, None);
        f.clear_goal(session).await;
    }

    // claimed_continuation_waits_for_readiness_before_materializing
    {
        let session = "claimed-readiness";
        f.seed_recovery_parent(session, "parent-claimed-waits", "completed")
            .await;
        let goal = seed_goal_from_access(
            f.agent_access(),
            f.did(),
            session,
            Some("Hold a durable claim until the behavior is ready"),
            Some(GoalStatus::Active),
            None,
            None,
        )
        .await
        .unwrap();
        assert!(crate::goal::claim_continuation_from_access(
            f.agent_access(),
            &goal,
            "parent-claimed-waits"
        )
        .await
        .unwrap());
        f.publish_readiness(Process::Recovering, None).await;
        let claimed =
            serde_json::to_value(f.load_goal(f.agent_access(), session).await.unwrap()).unwrap();
        let (mut source, _tx) = f.unready_source();
        assert_settles_without_fire(
            &mut source,
            "a claimed child must not be published to an unready behavior",
        )
        .await;
        assert!(goal_children_in_session(f.agent_access(), session)
            .await
            .is_empty());
        assert_eq!(
            serde_json::to_value(f.load_goal(f.agent_access(), session).await.unwrap()).unwrap(),
            claimed,
            "waiting must leave the durable claim untouched"
        );
        f.publish_readiness(Process::Ready, None).await;
        next_intent(&mut source, "goal continuation after readiness recovered").await;
        let children = goal_children_in_session(f.agent_access(), session).await;
        assert_eq!(children.len(), 1);
        assert_eq!(
            children[0].caused_by_parent_request_id.as_deref(),
            Some("parent-claimed-waits")
        );
        f.clear_goal(session).await;
    }

    // closed_goal_notifications_keep_durable_continuation_and_cancellation_live
    {
        let session = "closed-notifications";
        f.publish_readiness(Process::Ready, None).await;
        let updates = MockUpdateSubscriptionSource::new();
        updates.close();
        let (_tx, rx) = watch::channel(f.agent_snapshot());
        let cancel = CancellationToken::new();
        let mut source = GoalSource::with_subscription_source(
            Arc::new(updates),
            rx,
            f.home.node.clone(),
            cancel.clone(),
        )
        .with_rescan_interval(RESCAN);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), source.next_fire())
                .await
                .is_err(),
            "closing notifications must not retire the durable Goal source"
        );
        f.seed_recovery_parent(session, "completed-before-continuation", "completed")
            .await;
        seed_goal_from_access(
            f.agent_access(),
            f.did(),
            session,
            Some("Continue the existing objective"),
            Some(GoalStatus::Active),
            None,
            None,
        )
        .await
        .unwrap();
        let _fire =
            tokio::time::timeout(gents_store::test_support::STALL_CEILING, source.next_fire())
                .await
                .expect("durable rescan must find the active Goal")
                .expect("closed notifications must not terminate the source");
        assert_eq!(
            goal_children_in_session(f.agent_access(), session)
                .await
                .len(),
            1
        );
        assert_eq!(
            f.load_goal(f.agent_access(), session)
                .await
                .unwrap()
                .parsed_status(),
            Some(GoalStatus::Active)
        );
        cancel.cancel();
        assert!(
            tokio::time::timeout(gents_store::test_support::STALL_CEILING, source.next_fire())
                .await
                .expect("cancellation must stop the source")
                .is_none()
        );
    }
}

#[tokio::test]
async fn goal_source_readiness_and_closed_notification_scenarios() {
    let f = GoalFixture::start("goal-source-readiness").await;
    scenario_readiness_defer_and_race(&f).await;
}
