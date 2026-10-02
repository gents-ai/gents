use super::*;
use crate::claude_oauth::CLAUDE_OAUTH_PROVIDER;
use crate::config_client::{
    apply_desired_state_plan, write_inference_backend_document, ConfigAccess,
    DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use crate::identity::KeyIdentity;
use crate::oauth_credential::{preset_account_backend, store_sign_in};
use crate::request_admission::verify_runtime_local_control_receipt;
use crate::Collection;
use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};
use serde_json::{json, Value};
use std::sync::Arc;

pub(crate) const SESSION: &str = "contract-session";
pub(crate) const PARENT: &str = "contract-parent";
pub(crate) const PROFILE: &str = "contract-behavior:inference";

/// Claude accounts A (`label-a`) and B (`label-b`) of the fixture's DID.
pub(crate) struct ClaudeAccounts {
    pub access: ConfigAccess,
    pub a: String,
    pub b: String,
    pub a_credential: String,
}

pub(crate) struct Fixture {
    pub node: Arc<EmbeddedNode>,
    pub identity: Arc<KeyIdentity>,
    pub goal: GoalDocument,
    pub parent: crate::watcher::AgentRequest,
    _temp: tempfile::TempDir,
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
        let temp = tempfile::tempdir().unwrap();
        let identity =
            Arc::new(KeyIdentity::load_or_create(temp.path().join("target.key"), None).unwrap());
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        crate::schema::ensure_runtime_schemas(&node).await.unwrap();
        crate::test_support::install_test_behavior(
            node.as_ref(),
            identity.did(),
            "contract-behavior",
        )
        .await;
        let goal = set_goal(
            &node,
            identity.did(),
            SESSION,
            Some("Finish the original work"),
            Some(GoalStatus::parse(before["status"].as_str().unwrap()).unwrap()),
            Some(Some(1000)),
        )
        .await
        .unwrap();
        if let Some(request_id) = earlier {
            let mut request = AgentRequestCreate::base(
                gents_protocol::request_admission::RequestPurpose::Normal,
                request_id,
                identity.did(),
                identity.did(),
                "contract-behavior",
                SESSION,
                "Earlier work",
                "interactive",
                "2010-01-01T00:00:00Z",
                AgentRequestAdmissionRecord::local_self(identity.did()),
            );
            crate::sign_agent_request_create(identity.as_ref(), &mut request)
                .await
                .unwrap();
            execute(&node, &request.graphql_mutation().unwrap()).await;
        }
        let mut create = AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            PARENT,
            identity.did(),
            identity.did(),
            "contract-behavior",
            SESSION,
            "Original context",
            "interactive",
            "2020-01-01T00:00:00Z",
            AgentRequestAdmissionRecord::local_self(identity.did()),
        );
        create.caused_by_correlation = Some("graph-correlation".into());
        create.caused_by_source_doc_id = Some("source".into());
        create.caused_by_trigger_context = Some(r#"{"contract":"context"}"#.into());
        crate::sign_agent_request_create(identity.as_ref(), &mut create)
            .await
            .unwrap();
        execute(&node, &create.graphql_mutation().unwrap()).await;
        if terminal_parent {
            execute(&node, &format!(r#"mutation {{ update_AgentRequest(filter: {{ request_id: {{ _eq: "{PARENT}" }} }}, input: {{ lifecycle_state: "completed" }}) {{ _docID }} }}"#)).await;
        }
        let rows = request_rows(&node).await;
        let parent = crate::watcher::AgentRequest::try_from(
            rows.into_iter().find(|r| r.request_id == PARENT).unwrap(),
        )
        .unwrap();
        let f = Self {
            node,
            identity,
            goal,
            parent,
            _temp: temp,
        };
        f.seed_goal_snapshot(before).await;
        f
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
        execute(&self.node, &format!(r#"mutation {{ update_Goal(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{
            continuation_sequence: {}, last_continued_from_request_id: {last}, consecutive_blocked_audits: {},
            wrapup_requested: {}, wrapup_completed: {}, tokens_used: {}, token_budget: {}
        }}) {{ _docID }} }}"#, escape_graphql_string(&self.goal.doc_id), before["sequence"], before["blocked_audits"],
            before["wrapup_requested"], before["wrapup_completed"], before["tokens_used"], before["token_budget"])).await;
    }
    pub async fn seed_child(&self, conflicting: bool) {
        let mut create = prepare_goal_continuation(
            &self.parent,
            "contract-behavior".into(),
            &self.goal.goal_id,
            "Original signed continuation",
            1,
            false,
            "2021-01-01T00:00:00Z",
            self.parent.subagent_depth,
        )
        .unwrap();
        if conflicting {
            create.input.queue.as_mut().unwrap().key = Some("goal:foreign".into());
        }
        sign_request(&mut create, RequestSigner::Identity(self.identity.as_ref()))
            .await
            .unwrap();
        execute(&self.node, &create.graphql_mutation().unwrap()).await;
        // Historical receipt recovery must not require pending admission.
        execute(&self.node, &format!(r#"mutation {{ update_AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, input: {{ lifecycle_state: "completed", deadline: "2021-01-01T00:00:01Z" }}) {{ _docID }} }}"#, create.request_id)).await;
    }
    pub async fn other_request(&self, id: &str, date: &str, state: &str) {
        let mut create = AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            id,
            self.identity.did(),
            self.identity.did(),
            "contract-behavior",
            SESSION,
            "Other work",
            "interactive",
            date,
            AgentRequestAdmissionRecord::local_self(self.identity.did()),
        );
        crate::sign_agent_request_create(self.identity.as_ref(), &mut create)
            .await
            .unwrap();
        execute(&self.node, &create.graphql_mutation().unwrap()).await;
        execute(&self.node, &format!(r#"mutation {{ update_AgentRequest(filter: {{ request_id: {{ _eq: "{id}" }} }}, input: {{ lifecycle_state: "{state}" }}) {{ _docID }} }}"#)).await;
    }
    /// Sign Claude accounts A and B in, each with its backend, and put the
    /// behavior's profile on A.
    pub async fn claude_accounts(&self) -> ClaudeAccounts {
        let did = self.identity.did().to_owned();
        let access = ConfigAccess::Local(self.node.clone());
        let preset =
            preset_account_backend(&did, CLAUDE_OAUTH_PROVIDER, None, "Claude".into()).unwrap();
        write_inference_backend_document(&access, &preset)
            .await
            .unwrap();
        let mut accounts = Vec::new();
        for who in ["a", "b"] {
            let credential = crate::claude_oauth::credential_from_login_tokens(
                &did,
                CLAUDE_OAUTH_PROVIDER,
                &crate::claude_oauth::ClaudeLoginTokens {
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
        let [(a, a_credential), (b, _)] = <[_; 2]>::try_from(accounts).unwrap();
        let profile = json!({"agent_did": did, "profile_id": PROFILE, "backend_id": a, "model_name": "test-model"});
        let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
            collection: Collection::InferenceProfile,
            add: profile.clone(),
            update: profile,
        }])
        .unwrap();
        access
            .transact("test.claude_accounts.profile", |txn| {
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
        }
    }

    /// Fail `request_id` with one failed inference call on `backend` that
    /// queued, started and ended at `at`.
    pub async fn fail_with_call(&self, request_id: &str, backend: &str, failure: &str, at: &str) {
        let did = self.identity.did();
        let failure = escape_graphql_string(failure);
        execute(
            &self.node,
            &format!(
                r#"mutation {{
                    update_AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, input: {{
                        lifecycle_state: "failed" failure_reason: "{failure}"
                    }}) {{ _docID }}
                    create_InferenceCall(input: {{
                        call_id: "call-{request_id}" request_id: "{request_id}" call_seq: 1
                        backend_id: "{backend}" behavior_id: "contract-behavior" agent_did: "{did}"
                        call_kind: "inference" attempt: 1 call_state: "failed"
                        failure_reason: "{failure}"
                        queued_at: "{at}" started_at: "{at}" ended_at: "{at}"
                    }}) {{ _docID }}
                }}"#
            ),
        )
        .await;
    }

    pub async fn observe(&self) -> Value {
        let g = load_canonical_goal(&self.node, self.identity.did(), SESSION)
            .await
            .unwrap()
            .unwrap();
        let rows = request_rows(&self.node).await;
        let mut children = Vec::new();
        for child in rows.iter().filter(|r| {
            r.retry_key
                .as_deref()
                .is_some_and(|k| k.starts_with("goal-continuation:"))
        }) {
            verify_runtime_local_control_receipt(
                child,
                self.identity.did(),
                PARENT,
                self.identity.did(),
            )
            .unwrap();
            let input = child.input.as_ref().unwrap();
            let queue = input.queue.as_ref().unwrap();
            let continuation = input.goal_continuation.as_ref().unwrap();
            // Assert concrete lineage independently of prepare_goal_continuation:
            // sharing its implementation must not hide a producer deletion.
            assert_eq!(child.agent_did.as_deref(), Some(self.identity.did()));
            assert_eq!(child.requester_did.as_deref(), Some(self.identity.did()));
            assert_eq!(child.session_id.as_deref(), Some(SESSION));
            assert_eq!(child.behavior_id.as_deref(), Some("contract-behavior"));
            assert_eq!(child.execution_origin.as_deref(), Some("scheduled"));
            assert_eq!(child.caused_by_parent_request_id.as_deref(), Some(PARENT));
            assert_eq!(
                child.caused_by_parent_request_doc_id.as_deref(),
                Some(self.parent.doc_id.as_str())
            );
            assert_eq!(child.caused_by_parent_tool_call_id, None);
            assert_eq!(child.caused_by_parent_tool_call_doc_id, None);
            assert_eq!(child.subagent_depth, Some(0));
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

            let expected = prepare_goal_continuation(
                &self.parent,
                "contract-behavior".into(),
                &g.goal_id,
                child.content.as_deref().unwrap(),
                seq,
                wrapup,
                child.created_at.as_deref().unwrap(),
                self.parent.subagent_depth,
            )
            .unwrap();
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
pub(crate) async fn execute(node: &EmbeddedNode, query: &str) {
    let response = node.execute(query).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
}
pub(crate) async fn request_rows(node: &EmbeddedNode) -> Vec<AgentRequestRow> {
    let response=node.execute(&format!("{{ AgentRequest(order: [{{ created_at: DESC }}, {{ request_id: DESC }}]) {{ {SIGNED_REQUEST_FIELDS} }} }}")).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    crate::graphql::rows(&response, "AgentRequest").unwrap()
}
