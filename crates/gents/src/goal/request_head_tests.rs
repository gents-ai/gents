//! Real signed-row selector consumer; synthetic physical IDs deliberately do
//! not claim database storage uniqueness, transaction, or replication coverage.
use super::*;
use crate::identity::{AgentIdentity, KeyIdentity};
use crate::lifecycle::queue::prepare_goal_continuation;
use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Deserialize)]
struct Snapshot {
    goal_request_head_cases: Vec<Case>,
}
#[derive(Deserialize)]
struct Scope {
    owner: u64,
    session: u64,
    goal: u64,
}
#[derive(Clone, Debug, Deserialize)]
struct ModelRow {
    doc: u64,
    request: u64,
    owner: u64,
    session: u64,
    goal: Option<u64>,
    parent_doc: Option<u64>,
    parent_request: Option<u64>,
    sequence: u64,
    receipt_valid: bool,
    deterministic_identity: bool,
}
#[derive(Deserialize)]
struct Case {
    name: String,
    scope: Scope,
    rows: Vec<ModelRow>,
    expected: Option<ModelRow>,
}

fn signed_row(create: &AgentRequestCreate, doc: u64) -> AgentRequestRow {
    AgentRequestRow {
        doc_id: Some(format!("physical-{doc}")),
        request_id: create.request_id.clone(),
        agent_did: Some(create.agent_did.clone()),
        requester_did: Some(create.requester_did.clone()),
        behavior_id: create.behavior_id.clone(),
        session_id: Some(create.session_id.clone()),
        retry_parent_request: create.retry_parent_request.clone(),
        retry_parent_request_doc_id: create.retry_parent_request_doc_id.clone(),
        retry_root_request: create.retry_root_request.clone(),
        retry_key: create.retry_key.clone(),
        content: Some(create.content.clone()),
        temperature: create.temperature,
        top_p: create.top_p,
        top_k: create.top_k,
        seed: create.seed,
        max_tokens: create.max_tokens,
        max_total_tokens: create.max_total_tokens,
        metadata: create.metadata.clone(),
        backend_id: create.backend_id.clone(),
        execution_origin: Some(create.execution_origin.clone()),
        caused_by_trigger_id: create.caused_by_trigger_id.clone(),
        caused_by_trigger_doc_id: create.caused_by_trigger_doc_id.clone(),
        caused_by_trigger_kind: create.caused_by_trigger_kind.clone(),
        caused_by_correlation: create.caused_by_correlation.clone(),
        caused_by_trigger_context: create.caused_by_trigger_context.clone(),
        caused_by_source_doc_id: create.caused_by_source_doc_id.clone(),
        created_at: Some(create.created_at.clone()),
        retry_count: Some(create.retry_count),
        max_retries: Some(create.max_retries),
        valid_until: create.valid_until.clone(),
        subagent_depth: Some(i64::from(create.subagent_depth)),
        caused_by_parent_request_id: create.caused_by_parent_request_id.clone(),
        caused_by_parent_request_doc_id: create.caused_by_parent_request_doc_id.clone(),
        caused_by_parent_tool_call_id: create.caused_by_parent_tool_call_id.clone(),
        caused_by_parent_tool_call_doc_id: create.caused_by_parent_tool_call_doc_id.clone(),
        workspace_id: create.workspace_id.clone(),
        workspace_authority: create.workspace_authority.clone(),
        workspace_owner_deployment_id: create.workspace_owner_deployment_id.clone(),
        workspace_seal_hash: create.workspace_seal_hash.clone(),
        lifecycle_state: Some(RequestLifecycleState::Completed),
        admission_kind: Some(create.admission.kind.as_str().into()),
        admission_signer_did: Some(create.admission.signer_did.clone()),
        admission_signature: Some(bs58::encode(&create.admission.signature).into_string()),
        runtime_issuer_did: create.admission.runtime_issuer_did.clone(),
        runtime_source_request_id: create.admission.runtime_source_request_id.clone(),
        runtime_source_kind: create
            .admission
            .runtime_source_kind
            .map(|kind| kind.as_str().into()),
        ..Default::default()
    }
}

struct Fixture<'a> {
    case: &'a Case,
    identities: &'a [KeyIdentity],
    built: HashMap<u64, AgentRequestRow>,
}
impl Fixture<'_> {
    fn identity(&self, owner: u64) -> &KeyIdentity {
        &self.identities[(owner - 1) as usize]
    }
    fn base_id(&self, request: u64) -> String {
        match request {
            100 if self.case.name == "same_second_task_parent_chain" => {
                "task-goal-request:original".into()
            }
            100 => "graph-parent".into(),
            400 => "unrelated-latest".into(),
            _ => format!("ordinary-{request}"),
        }
    }
    fn timestamp(&self, request: u64) -> String {
        if self.case.name.starts_with("same_second_") {
            return "2026-07-15T00:00:00Z".into();
        }
        let index = self
            .case
            .rows
            .iter()
            .position(|row| row.request == request)
            .unwrap_or(0);
        format!("2026-07-15T00:00:{:02}Z", 59 - index)
    }
    fn build<'a>(
        &'a mut self,
        model: ModelRow,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = AgentRequestRow> + 'a>> {
        Box::pin(async move {
            if let Some(row) = self.built.get(&model.request) {
                return row.clone();
            }
            let now = self.timestamp(model.request);
            let mut create = if let Some(goal) = model.goal {
                let parent_request = model.parent_request.unwrap();
                let parent_model = self
                    .case
                    .rows
                    .iter()
                    .find(|row| row.request == parent_request)
                    .cloned();
                let mut parent = if let Some(parent_model) = parent_model {
                    self.build(parent_model).await
                } else {
                    let identity = self.identity(model.owner);
                    let base = AgentRequestCreate::base(
                        self.base_id(parent_request),
                        identity.did(),
                        identity.did(),
                        "head-behavior",
                        format!("session-{}", model.session),
                        "parent",
                        "interactive",
                        &now,
                        AgentRequestAdmissionRecord::local_self(identity.did()),
                    );
                    signed_row(&base, model.parent_doc.unwrap())
                };
                // Model fields name the claimed physical pair, independently of
                // whether it matches any actual parent in the input snapshot.
                parent.doc_id = model.parent_doc.map(|doc| format!("physical-{doc}"));
                parent.agent_did = Some(self.identity(model.owner).did().into());
                parent.session_id = Some(format!("session-{}", model.session));
                let parent = crate::watcher::AgentRequest::try_from(parent).unwrap();
                prepare_goal_continuation(
                    &parent,
                    "head-behavior".into(),
                    &format!("goal-{goal}"),
                    "signed child",
                    i64::try_from(model.sequence).unwrap(),
                    false,
                    &now,
                )
                .unwrap()
            } else {
                let identity = self.identity(model.owner);
                let mut base = AgentRequestCreate::base(
                    self.base_id(model.request),
                    identity.did(),
                    identity.did(),
                    "head-behavior",
                    format!("session-{}", model.session),
                    "signed root",
                    "interactive",
                    &now,
                    AgentRequestAdmissionRecord::local_self(identity.did()),
                );
                base.caused_by_parent_request_id =
                    model.parent_request.map(|request| self.base_id(request));
                base.caused_by_parent_request_doc_id =
                    model.parent_doc.map(|doc| format!("physical-{doc}"));
                base
            };
            if self.case.name == "legacy_source_omission_keeps_physical_edge" {
                // Old automatic continuations were validly signed without this
                // inherited field; change it before signing the fixture DTO.
                create.caused_by_source_doc_id = if model.goal.is_none() {
                    Some("original-event-source".into())
                } else {
                    None
                };
            }
            if !model.deterministic_identity {
                create.request_id.push_str("-wrong-identity");
            }
            crate::sign_agent_request_create(self.identity(model.owner), &mut create)
                .await
                .unwrap();
            let mut row = signed_row(&create, model.doc);
            if !model.receipt_valid {
                row.content = Some("altered after signing".into());
            }
            assert_eq!(
                verify_request_receipt_signature(&row).is_ok(),
                model.receipt_valid,
                "{}: actual signature",
                self.case.name
            );
            self.built.insert(model.request, row.clone());
            row
        })
    }
}

#[tokio::test]
async fn generated_goal_request_head_cases_drive_signed_row_selector() {
    let snapshot: Snapshot = gents_lean_contract::load_contract_snapshot().unwrap();
    assert_eq!(snapshot.goal_request_head_cases.len(), 15);
    let temp = tempfile::tempdir().unwrap();
    let identities = [
        KeyIdentity::load_or_create(temp.path().join("owner.key"), None).unwrap(),
        KeyIdentity::load_or_create(temp.path().join("foreign.key"), None).unwrap(),
    ];
    for case in snapshot.goal_request_head_cases {
        let goal: GoalDocument = serde_json::from_value(serde_json::json!({
            "_docID": "goal-doc", "goal_id": format!("goal-{}", case.scope.goal),
            "agent_did": identities[(case.scope.owner - 1) as usize].did(),
            "session_id": format!("session-{}", case.scope.session), "status": "active",
        }))
        .unwrap();
        let mut fixture = Fixture {
            case: &case,
            identities: &identities,
            built: HashMap::new(),
        };
        let mut rows = Vec::new();
        for model in &case.rows {
            rows.push(fixture.build(model.clone()).await);
        }
        assert_eq!(
            rows.iter()
                .map(|row| row.doc_id.as_deref().unwrap())
                .collect::<Vec<_>>(),
            case.rows
                .iter()
                .map(|row| format!("physical-{}", row.doc))
                .collect::<Vec<_>>(),
            "{}: preserve Lean input mapping",
            case.name
        );
        assert!(
            rows.windows(2)
                .all(|pair| (pair[0].created_at.as_ref(), &pair[0].request_id)
                    >= (pair[1].created_at.as_ref(), &pair[1].request_id)),
            "{}: real canonical input order",
            case.name
        );
        if case.name == "legacy_source_omission_keeps_physical_edge" {
            assert_eq!(
                rows[0].caused_by_source_doc_id.as_deref(),
                Some("original-event-source")
            );
            assert_eq!(rows[1].caused_by_source_doc_id, None);
            verify_request_receipt_signature(&rows[0]).unwrap();
            verify_request_receipt_signature(&rows[1]).unwrap();
        }
        let selected = latest_goal_request(&goal, &rows).and_then(|row| row.doc_id.as_deref());
        let expected = case
            .expected
            .as_ref()
            .map(|row| format!("physical-{}", row.doc));
        assert_eq!(
            selected,
            expected.as_deref(),
            "{}: production selector must honor authenticated causal head",
            case.name
        );
    }
}
