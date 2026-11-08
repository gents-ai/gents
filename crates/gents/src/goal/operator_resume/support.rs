use super::*;
use crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER;
use crate::inference::auth::credential::{preset_account_backend, store_sign_in};
use crate::documents::{apply_desired_state_plan, write_inference_backend_document};
use crate::store::config_client::{ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan};
use crate::store::identity::request_receipt::verify_request_receipt_signature;
use crate::store::identity::KeyIdentity;
use crate::Collection;
use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};
use serde_json::{json, Value};
use std::sync::Arc;

pub(crate) const SESSION: &str = "contract-session";
pub(crate) const PARENT: &str = "contract-parent";
pub(crate) const PROFILE: &str = "contract-behavior:inference";

pub(crate) struct ClaudeAccounts {
    pub access: ConfigAccess,
    pub a: String,
    pub b: String,
    pub a_credential: String,
    pub b_credential: String,
}

pub(crate) struct Fixture {
    pub node: Arc<EmbeddedNode>,
    pub identity: Arc<dyn NodeIdentity>,
    pub user: Arc<KeyIdentity>,
    pub agent: Arc<dyn NodeIdentity>,
    pub session_doc_id: String,
    pub agent_doc_id: String,
    pub goal: GoalDocument,
    pub parent: gents_protocol::row::AgentRequest,
    user_access: ConfigAccess,
    _db: crate::support::TestDb,
}
impl Fixture {
    pub async fn new(before: &Value) -> Self {
        Self::new_with_parent_state(before, true).await
    }

    pub async fn new_with_parent_state(before: &Value, terminal_parent: bool) -> Self {
        Self::new_with_earlier_request(before, terminal_parent, None).await
    }

    pub async fn new_with_earlier_request(
        before: &Value,
        terminal_parent: bool,
        earlier: Option<&str>,
    ) -> Self {
        Self::new_with_goal_author(before, terminal_parent, earlier, false).await
    }

    pub async fn new_with_goal_author(
        before: &Value,
        terminal_parent: bool,
        earlier: Option<&str>,
        user_owned_goal: bool,
    ) -> Self {
        let (db, user, user_access, binding) =
            crate::test_support::signed_test_node("goal-operator").await;
        let node = db.node.clone();
        let identity = db.node_identity.clone();
        let user = Arc::new(user);
        let agent =
            crate::store::identity::resolve_agent_identity(identity.as_ref(), &binding).unwrap();
        let admin =
            ConfigAccess::local_as(node.clone(), ::identity::Did::new(identity.did()).unwrap());
        for permission in [
            crate::store::config_client::NodePermission::DacRelationDelete,
            crate::store::config_client::NodePermission::DocumentDelete,
        ] {
            admin
                .grant_node_permission(&::identity::Did::new(user.did()).unwrap(), permission)
                .await
                .unwrap();
        }
        let actor = ::identity::Did::new(agent.did()).unwrap();
        for permission in [
            crate::store::config_client::NodePermission::CollectionGet,
            crate::store::config_client::NodePermission::DocumentRead,
            crate::store::config_client::NodePermission::DocumentUpdate,
            crate::store::config_client::NodePermission::DocumentDelete,
            crate::store::config_client::NodePermission::DacRelationAdd,
            crate::store::config_client::NodePermission::DacRelationDelete,
        ] {
            admin
                .grant_node_permission(&actor, permission)
                .await
                .unwrap();
        }
        let context = format!("{}:context", binding.agent_did);
        let tools = format!("{}:tools", binding.agent_did);
        let profile = format!("{}:inference", binding.agent_did);
        let backend = format!("{}:backend", binding.agent_did);
        let documents = [
            (Collection::Agent, json!({"node_did":identity.did(),"agent_did":binding.agent_did,"creation_nonce":binding.creation_nonce,"display_name":"contract-behavior","context_id":context,"inference_profile_id":profile})),
            (Collection::AgentContext, json!({"node_did":identity.did(),"context_id":context,"tools_id":tools})),
            (Collection::Tools, json!({"node_did":identity.did(),"tools_id":tools})),
            (Collection::InferenceProfile, json!({"node_did":identity.did(),"profile_id":profile,"backend_id":backend,"model_name":"test-model"})),
            (Collection::InferenceBackend, json!({"node_did":identity.did(),"backend_id":backend,"name":"Test inference","provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:1/v1","auth":{"kind":"unauthenticated"}})),
        ].into_iter().map(|(collection, value)| DesiredStateApplyDocument {
            collection, add: value.clone(), update: value,
        }).collect();
        let plan = DesiredStateApplyPlan::new(documents).unwrap();
        user_access
            .transact_write("test.goal_fixture.agent", |txn| {
                let plan = &plan;
                Box::pin(async move { apply_desired_state_plan(txn, plan).await })
            })
            .await
            .unwrap();
        crate::inference::backends::registry::set_backend_probe_status_as(
            &admin,
            identity.did(),
            &backend,
            "healthy",
        )
        .await
        .unwrap();
        user_access
            .transact_write("test.goal_fixture.session", |txn| {
                let binding = &binding;
                let identity = &identity;
                let user = &user;
                Box::pin(async move {
                    crate::session::ensure_session_in_txn(
                        txn,
                        SESSION,
                        identity.did(),
                        &binding.agent_did,
                        Some(user.did()),
                        None,
                        None,
                        "2026-01-01T00:00:00Z",
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await
            .unwrap();
        let session_doc_id = user_access
            .transact_readonly("test.goal_fixture.session_document", |txn| {
                Box::pin(async {
                    crate::store::config_client::agent_session::load_agent_session_owner_in_txn(
                        txn,
                        identity.did(),
                        SESSION,
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .unwrap()
            .doc_id;
        let agent_doc_id = crate::goal::client_control::agent_document_for_test(
            &user_access,
            identity.did(),
            &binding.agent_did,
        )
        .await;
        crate::session::grant_created_session_relationships(
            &user_access,
            &session_doc_id,
            &agent_doc_id,
            &binding.agent_did,
        )
        .await
        .unwrap();
        let agent_access = ConfigAccess::local_managed_agent(
            node.clone(),
            ::identity::Did::new(&binding.agent_did).unwrap(),
            agent.as_ref(),
        )
        .unwrap();
        let goal = set_goal_from_access(
            if user_owned_goal {
                &user_access
            } else {
                &agent_access
            },
            identity.did(),
            SESSION,
            Some("Finish the original work"),
            Some(GoalStatus::parse(before["status"].as_str().unwrap()).unwrap()),
            Some(Some(1000)),
            None,
        )
        .await
        .unwrap();
        if let Some(request_id) = earlier {
            let mut request = AgentRequestCreate::base(
                gents_protocol::request_admission::RequestPurpose::Normal,
                request_id,
                identity.did(),
                user.did(),
                &binding.agent_did,
                SESSION,
                "Earlier work",
                "interactive",
                "2010-01-01T00:00:00Z",
                AgentRequestAdmissionRecord::client(user.did()),
            );
            request.session_doc_id = Some(session_doc_id.clone());
            request.agent_doc_id = Some(agent_doc_id.clone());
            crate::sign_agent_request_create(user.as_ref(), &mut request)
                .await
                .unwrap();
            crate::request::lifecycle::submit_client_request(&user_access, &request)
                .await
                .unwrap();
        }
        let mut create = AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            PARENT,
            identity.did(),
            user.did(),
            &binding.agent_did,
            SESSION,
            "Original context",
            "interactive",
            "2020-01-01T00:00:00Z",
            AgentRequestAdmissionRecord::client(user.did()),
        );
        create.session_doc_id = Some(session_doc_id.clone());
        create.agent_doc_id = Some(agent_doc_id.clone());
        create.caused_by_correlation = Some("graph-correlation".into());
        create.caused_by_source_doc_id = Some("source".into());
        create.caused_by_trigger_context = Some(r#"{"contract":"context"}"#.into());
        crate::sign_agent_request_create(user.as_ref(), &mut create)
            .await
            .unwrap();
        crate::request::lifecycle::submit_client_request(&user_access, &create)
            .await
            .unwrap();
        if terminal_parent {
            agent_access.write("test.goal_fixture.terminal", &format!(r#"mutation {{ update_AgentRequest(filter: {{ request_id: {{ _eq: "{PARENT}" }} }}, input: {{ lifecycle_state: "completed" }}) {{ _docID }} }}"#)).await.unwrap();
        }
        let rows = request_rows(&user_access).await;
        let parent = gents_protocol::row::AgentRequest::try_from(
            rows.into_iter().find(|r| r.request_id == PARENT).unwrap(),
        )
        .unwrap();
        let f = Self {
            node,
            identity,
            user,
            agent,
            session_doc_id,
            agent_doc_id,
            goal,
            parent,
            user_access,
            _db: db,
        };
        f.seed_goal_snapshot(before).await;
        f
    }
    pub fn agent_access(&self) -> ConfigAccess {
        ConfigAccess::local_managed_agent(
            self.node.clone(),
            ::identity::Did::new(self.agent.did()).unwrap(),
            self.agent.as_ref(),
        )
        .unwrap()
    }

    pub fn user_access(&self) -> ConfigAccess {
        self.user_access.clone()
    }

    pub async fn goal_commit(&self, doc_id: &str) -> crate::store::graphql::CompositeCommit {
        self.agent_access()
            .transact_readonly("test.goal_revision", |txn| {
                Box::pin(async move {
                    crate::store::graphql::newest_document_composite_commit_in_txn(
                        txn,
                        doc_id,
                        "test.goal_revision",
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .unwrap()
    }

    pub async fn seed_goal_snapshot(&self, before: &Value) {
        let last = if before["last_continued_from"].is_null() {
            "null".to_owned()
        } else {
            let request = match before["last_continued_from"].as_u64().unwrap() {
                10 => PARENT,
                30 => "watermark-parent",
                value => panic!("unmapped predecessor watermark {value}"),
            };
            format!("\"{request}\"")
        };
        execute(&self.agent_access(), &format!(r#"mutation {{ update_Goal(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{
            continuation_sequence: {}, last_continued_from_request_id: {last}, consecutive_blocked_audits: {},
            wrapup_requested: {}, wrapup_completed: {}, tokens_used: {}, token_budget: {}
        }}) {{ _docID }} }}"#, escape_graphql_string(&self.goal.doc_id), before["sequence"], before["blocked_audits"],
            before["wrapup_requested"], before["wrapup_completed"], before["tokens_used"], before["token_budget"])).await;
    }
    pub async fn seed_child(&self, conflicting: bool) {
        self.seed_child_with_targets(
            conflicting,
            Some(self.session_doc_id.clone()),
            Some(self.agent_doc_id.clone()),
        )
        .await;
    }

    pub async fn seed_child_with_targets(
        &self,
        conflicting: bool,
        session_doc: Option<String>,
        agent_doc: Option<String>,
    ) {
        let mut create = prepare_goal_continuation(
            &self.parent,
            self.parent.agent_did.clone(),
            &self.goal.goal_id,
            "Original signed continuation",
            1,
            false,
            "2021-01-01T00:00:00Z",
            self.parent.request_hop,
        )
        .unwrap();
        create.input.goal_assignment =
            Some(continuation_assignment(&self.goal, &self.parent, None).unwrap());
        create.session_doc_id = session_doc;
        create.agent_doc_id = agent_doc;
        if conflicting {
            create.input.queue.as_mut().unwrap().key = Some("goal:foreign".into());
        }
        sign_request(&mut create, RequestSigner::Identity(self.agent.as_ref()))
            .await
            .unwrap();
        let agent_access = self.agent_access();
        agent_access
            .write("test.goal.seed_child", &create.graphql_mutation().unwrap())
            .await
            .unwrap();
        let child = request_rows(&agent_access)
            .await
            .into_iter()
            .find(|row| row.request_id == create.request_id)
            .unwrap();
        let child_doc = child.doc_id.as_deref().unwrap();
        agent_access
            .add_resource_relationship(
                "AgentRequest",
                child_doc,
                "parent",
                "agents",
                &self.agent_doc_id,
            )
            .await
            .unwrap();
        agent_access
            .add_resource_relationship(
                "AgentRequest",
                child_doc,
                "session",
                "sessions",
                &self.session_doc_id,
            )
            .await
            .unwrap();
        agent_access
            .add_actor_relationship(
                "AgentRequest",
                child_doc,
                "working_agent",
                &::identity::Did::new(self.agent.did()).unwrap(),
            )
            .await
            .unwrap();
        agent_access.write("test.goal.seed_child_grants", &format!(r#"mutation {{ update_AgentRequest(docID: "{}", input: {{ relationships_pending: false }}) {{ _docID }} }}"#, escape_graphql_string(child_doc))).await.unwrap();
        agent_access
            .transact_write("test.goal.seed_child_binding", |txn| {
                let create = &create;
                let child = &child;
                Box::pin(async move {
                    bind_goal_first_request_in_txn(
                        txn,
                        &self.goal,
                        child.doc_id.as_deref().unwrap(),
                        create.input.goal_assignment.as_ref(),
                    )
                    .await
                })
            })
            .await
            .unwrap();
        agent_access.write("test.goal.seed_child_terminal", &format!(r#"mutation {{ update_AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, input: {{ lifecycle_state: "completed", deadline: "2021-01-01T00:00:01Z" }}) {{ _docID }} }}"#, escape_graphql_string(&create.request_id))).await.unwrap();
    }
    pub async fn other_request(&self, id: &str, date: &str, state: &str) {
        let mut create = AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            id,
            self.identity.did(),
            self.user.did(),
            &self.parent.agent_did,
            SESSION,
            "Other work",
            "interactive",
            date,
            AgentRequestAdmissionRecord::client(self.user.did()),
        );
        create.session_doc_id = Some(self.session_doc_id.clone());
        create.agent_doc_id = Some(self.agent_doc_id.clone());
        crate::sign_agent_request_create(self.user.as_ref(), &mut create)
            .await
            .unwrap();
        let access = self.user_access();
        crate::request::lifecycle::submit_client_request(&access, &create)
            .await
            .unwrap();
        self.agent_access()
            .write("test.goal.other_request_state", &format!(r#"mutation {{ update_AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, input: {{ lifecycle_state: "{}" }}) {{ _docID }} }}"#, escape_graphql_string(id), escape_graphql_string(state))).await.unwrap();
    }
    pub async fn claude_accounts(&self) -> ClaudeAccounts {
        let did = self.identity.did().to_owned();
        let access = self.user_access();
        let preset =
            preset_account_backend(&did, CLAUDE_OAUTH_PROVIDER, None, "Claude".into()).unwrap();
        write_inference_backend_document(&access, &preset)
            .await
            .unwrap();
        let mut accounts = Vec::new();
        for who in ["a", "b"] {
            let credential = crate::inference::auth::claude::credential_from_login_tokens(
                &did,
                CLAUDE_OAUTH_PROVIDER,
                &crate::inference::auth::claude::ClaudeLoginTokens {
                    access_token: format!("access-SECRET-{who}"),
                    refresh_token: format!("refresh-SECRET-{who}"),
                    expires_in: Some(3600),
                    scope: None,
                    account_id: Some("IDENTITY".into()),
                    organization_uuid: Some("org-1".into()),
                    account_uuid: Some(format!("account-{who}")),
                },
                Utc::now() - chrono::Duration::hours(1),
            );
            let label = format!("label-{who}");
            let stored = store_sign_in(&access, credential, Some(&label))
                .await
                .unwrap()
                .credential;
            let backend = match stored.account_ref.as_deref() {
                Some(account_ref) => format!("{CLAUDE_OAUTH_PROVIDER}-{account_ref}"),
                None => CLAUDE_OAUTH_PROVIDER.to_owned(),
            };
            accounts.push((backend, stored.credential_id));
        }
        let [(a, a_credential), (b, b_credential)] = <[_; 2]>::try_from(accounts).unwrap();
        let profile = json!({"node_did": did, "profile_id": PROFILE, "backend_id": a, "model_name": "test-model"});
        let (_, mut agent) = access
            .transact_readonly("test.claude_accounts.agent", |txn| {
                Box::pin(async {
                    crate::store::config_client::read_desired_state_record_in_txn(
                        txn,
                        Collection::Agent,
                        &did,
                        &self.parent.agent_did,
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .unwrap();
        agent["inference_profile_id"] = PROFILE.into();
        let plan = DesiredStateApplyPlan::new(vec![
            DesiredStateApplyDocument {
                collection: Collection::InferenceProfile,
                add: profile.clone(),
                update: profile,
            },
            DesiredStateApplyDocument {
                collection: Collection::Agent,
                add: agent.clone(),
                update: agent,
            },
        ])
        .unwrap();
        access
            .transact_write("test.claude_accounts.profile", |txn| {
                let plan = &plan;
                Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
            })
            .await
            .unwrap();
        ClaudeAccounts {
            access,
            a,
            b,
            a_credential,
            b_credential,
        }
    }

    pub async fn compacts_on(&self, access: &ConfigAccess, backend: &str) {
        let did = self.identity.did();
        let documents = [
            (
                Collection::InferenceProfile,
                json!({"node_did": did, "profile_id": "summ", "backend_id": backend, "model_name": "model-s"}),
            ),
            (
                Collection::Compaction,
                json!({"node_did": did, "compaction_id": "compaction-c", "inference_profile_id": "summ"}),
            ),
            (
                Collection::AgentContext,
                json!({"node_did": did, "context_id": format!("{}:context", self.parent.agent_did), "tools_id": format!("{}:tools", self.parent.agent_did), "compaction_id": "compaction-c"}),
            ),
        ];
        let plan = DesiredStateApplyPlan::new(
            documents
                .into_iter()
                .map(|(collection, value)| DesiredStateApplyDocument {
                    collection,
                    add: value.clone(),
                    update: value,
                })
                .collect(),
        )
        .unwrap();
        access
            .transact_write("test.compacts_on", |txn| {
                let plan = &plan;
                Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
            })
            .await
            .unwrap();
    }

    pub async fn current_goal(&self) -> GoalDocument {
        self.agent_access()
            .transact_readonly("test.goal.current", |txn| {
                Box::pin(load_canonical_goal_in_txn(
                    txn,
                    self.identity.did(),
                    SESSION,
                ))
            })
            .await
            .unwrap()
            .unwrap()
    }

    pub async fn goal_status(&self) -> String {
        self.current_goal().await.status
    }

    pub async fn children(&self) -> Vec<String> {
        request_rows(&self.agent_access())
            .await
            .into_iter()
            .filter(|row| {
                row.retry_key
                    .as_deref()
                    .is_some_and(|key| key.starts_with("goal-continuation:"))
            })
            .map(|row| row.request_id)
            .collect()
    }

    pub async fn fail_with_call(
        &self,
        request_id: &str,
        kind: &str,
        backend: &str,
        failure: &str,
        at: &str,
    ) {
        let access = self.agent_access();
        let rows = request_rows(&access).await;
        let matches = rows
            .iter()
            .filter(|row| row.request_id == request_id)
            .collect::<Vec<_>>();
        let [request] = matches.as_slice() else {
            panic!("failed call must have exactly one physical request");
        };
        let request_doc = request.doc_id.as_deref().expect("failed request document");
        let update = format!(
            r#"mutation {{ update_AgentRequest(docID: "{}", input: {{ lifecycle_state: "failed", failure_reason: "{}" }}) {{ _docID }} }}"#,
            escape_graphql_string(request_doc),
            escape_graphql_string(failure),
        );
        let input = json!({
            "call_id": format!("call-{request_id}"), "request_id": request_id,
            "request_doc_id": request_doc, "session_id": SESSION,
            "call_seq": 1, "backend_id": backend, "agent_did": self.agent.did(),
            "node_did": self.identity.did(), "call_kind": kind, "attempt": 1,
            "call_state": "failed", "failure_reason": failure,
            "queued_at": at, "started_at": at, "ended_at": at,
        });
        access.transact_write("test.goal.failed_call", |txn| {
            let update = &update;
            let input = &input;
            let session_doc = &self.session_doc_id;
            Box::pin(async move {
                let changed = txn.execute(update).await?;
                let updates = crate::store::graphql::mutation_documents(&changed, "update_AgentRequest")?
                    .context("failed request update omitted result")?;
                anyhow::ensure!(updates.len() == 1, "failed request update lost its physical identity");
                let response = txn.execute_with_variables(
                    "mutation($input: InferenceCallMutationInputArg!) { create_InferenceCall(input: $input) { _docID } }",
                    &json!({"input": input}),
                ).await?;
                let call_doc = crate::store::graphql::created_doc_id(&response, "InferenceCall")?;
                txn.add_resource_relationship("InferenceCall", &call_doc, "parent", "sessions", session_doc).await?;
                Ok::<_, anyhow::Error>(())
            })
        }).await.unwrap();
        let call_id = escape_graphql_string(&format!("call-{request_id}"));
        let observed = self.user_access.execute(&format!(
            r#"{{ InferenceCall(filter: {{call_id: {{_eq: "{call_id}"}}}}, limit: 2) {{request_doc_id failure_reason}} }}"#,
        )).await.unwrap();
        let [call] = observed["data"]["InferenceCall"]
            .as_array()
            .unwrap()
            .as_slice()
        else {
            panic!("session User must observe the failed inference call");
        };
        assert_eq!(call["request_doc_id"], request_doc);
        assert_eq!(call["failure_reason"], failure);
    }

    pub async fn observe(&self) -> Value {
        let g = self
            .agent_access()
            .transact_readonly("test.goal.observe", |txn| {
                Box::pin(load_canonical_goal_in_txn(
                    txn,
                    self.identity.did(),
                    SESSION,
                ))
            })
            .await
            .unwrap()
            .unwrap();
        let rows = request_rows(&self.agent_access()).await;
        let mut children = Vec::new();
        for child in rows.iter().filter(|r| {
            r.retry_key
                .as_deref()
                .is_some_and(|k| k.starts_with("goal-continuation:"))
        }) {
            verify_request_receipt_signature(child).unwrap();
            match child.admission_kind.as_deref() {
                Some("runtime-internal") => assert_eq!(
                    child.admission_signer_did.as_deref(),
                    Some(self.agent.did())
                ),
                Some("client") => {
                    assert_eq!(child.admission_signer_did.as_deref(), Some(self.user.did()))
                }
                other => panic!("unexpected continuation admission kind {other:?}"),
            }
            let input = child.input.as_ref().unwrap();
            let queue = input.queue.as_ref().unwrap();
            let continuation = input.goal_continuation.as_ref().unwrap();
            assert_eq!(child.node_did.as_deref(), Some(self.identity.did()));
            assert_eq!(child.requester_did.as_deref(), Some(self.user.did()));
            assert_eq!(child.session_id.as_deref(), Some(SESSION));
            assert_eq!(
                child.agent_did.as_deref(),
                Some(self.parent.agent_did.as_str())
            );
            assert_eq!(child.execution_origin.as_deref(), Some("scheduled"));
            assert_eq!(child.caused_by_parent_request_id.as_deref(), Some(PARENT));
            assert_eq!(
                child.caused_by_parent_request_doc_id.as_deref(),
                Some(self.parent.doc_id.as_str())
            );
            assert_eq!(child.caused_by_parent_tool_call_id, None);
            assert_eq!(child.caused_by_parent_tool_call_doc_id, None);
            assert_eq!(child.request_hop, Some(0));
            assert_eq!(
                child.caused_by_trigger_id.as_deref(),
                Some(self.goal.goal_id.as_str())
            );
            assert_eq!(child.caused_by_trigger_kind.as_deref(), Some("goal"));
            assert_eq!(child.caused_by_trigger_doc_id, None);
            assert_eq!(
                child.caused_by_correlation.as_deref(),
                Some("graph-correlation")
            );
            assert_eq!(child.caused_by_source_doc_id.as_deref(), Some("source"));
            assert_eq!(
                child.caused_by_trigger_context.as_deref(),
                Some(r#"{"contract":"context"}"#)
            );
            let wrapup = self.goal.parsed_status() == Some(GoalStatus::BudgetLimited);
            assert_eq!(continuation.wrapup, wrapup);
            assert_eq!(
                queue.source,
                gents_protocol::request_input::QueueSource::Goal
            );
            assert_eq!(
                queue.policy,
                gents_protocol::request_input::QueuePolicy::Coalesce
            );
            assert_eq!(queue.queued_after_request_id.as_deref(), Some(PARENT));
            let seq = continuation.sequence;
            assert_eq!(seq, 1);
            let identity = goal_continuation_identity(&self.goal.goal_id, PARENT, 1).unwrap();
            assert_eq!(child.request_id, identity.request_id);
            assert_eq!(
                child.retry_key.as_deref(),
                Some(identity.retry_key.as_str())
            );
            if queue.key.as_deref() != Some("goal:foreign") {
                assert_eq!(queue.key.as_deref(), Some(identity.queue_key.as_str()));
            }

            let mut expected = prepare_goal_continuation(
                &self.parent,
                self.parent.agent_did.clone(),
                &g.goal_id,
                child.content.as_deref().unwrap(),
                seq,
                wrapup,
                child.created_at.as_deref().unwrap(),
                self.parent.request_hop,
            )
            .unwrap();
            expected.input.goal_assignment =
                Some(continuation_assignment(&g, &self.parent, child.doc_id.as_deref()).unwrap());
            assert_eq!(
                child.session_doc_id.as_deref(),
                Some(self.session_doc_id.as_str())
            );
            assert_eq!(
                child.agent_doc_id.as_deref(),
                Some(self.agent_doc_id.as_str())
            );
            expected.session_doc_id = Some(self.session_doc_id.clone());
            expected.agent_doc_id = Some(self.agent_doc_id.clone());
            if child.admission_kind.as_deref() == Some("client") {
                expected.admission = AgentRequestAdmissionRecord::client(self.user.did());
            }
            let actual: GoalBackedRequestFingerprint =
                serde_json::from_value(serde_json::to_value(child).unwrap()).unwrap();
            let mut comparison = expected.clone();
            let foreign = queue.key.as_deref() == Some("goal:foreign");
            if foreign {
                comparison.input.queue.as_mut().unwrap().key = Some("goal:foreign".into());
            }
            assert_eq!(
                actual,
                GoalBackedRequestFingerprint::from_create(&comparison).unwrap(),
                "all concrete signed child semantics must match before abstraction"
            );
            children.push(json!({"goal":1,"owner":"owner","session":"session","predecessor":10,
                "predecessor_doc":100,"correlation":"graph-correlation","source_document":"source",
                "trigger_context":"context","workspace_fingerprint":"workspace-authority",
                "semantic_fingerprint":if foreign {"foreign"} else {"full-semantic-fingerprint"},"child":20,"sequence":seq}));
        }
        let latest = rows
            .first()
            .map(|r| match r.request_id.as_str() {
                PARENT => 10,
                "later-request" => 30,
                "older-active" | "claimed-later-request" => 40,
                id if id.starts_with("goal-cont-") => 20,
                id => panic!("unknown request {id}"),
            })
            .unwrap();
        let last = g
            .last_continued_from_request_id
            .as_deref()
            .map(|id| match id {
                PARENT => 10,
                "watermark-parent" => 30,
                other => panic!("unmapped predecessor watermark {other}"),
            });
        json!({"status":g.status,"blocked_audits":g.consecutive_blocked_audits.unwrap_or(0),
            "wrapup_requested":g.wrapup_requested.unwrap_or(false),"wrapup_completed":g.wrapup_completed.unwrap_or(false),
            "sequence":g.continuation_sequence(),"last_continued_from":last,"latest_request":latest,
            "children":children,"tokens_used":g.tokens_used.unwrap_or(0),"token_budget":g.token_budget})
    }
}
pub(crate) async fn execute(access: &ConfigAccess, query: &str) {
    access.write("test.goal.fixture", query).await.unwrap();
}
pub(crate) async fn request_rows(access: &ConfigAccess) -> Vec<AgentRequestRow> {
    let response = access.execute(&format!("{{ AgentRequest(order: [{{ created_at: DESC }}, {{ request_id: DESC }}]) {{ {SIGNED_REQUEST_FIELDS} }} }}")).await.unwrap();
    serde_json::from_value(response["data"]["AgentRequest"].clone()).unwrap()
}
