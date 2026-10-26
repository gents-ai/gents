use super::*;
use crate::identity::{KeyIdentity, NodeIdentity};
use crate::lifecycle::queue::prepare_goal_continuation;
use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};
use rapidhash::{HashMapExt, RapidHashMap};
use serde::Deserialize;

#[derive(Deserialize)]
struct Snapshot {
    goal_request_head_cases: Vec<Case>,
}

#[tokio::test]
async fn generated_goal_wake_ancestry_keeps_assignment_without_widening_invocations() {
    #[derive(Deserialize)]
    struct Contracts {
        goal_wake_ancestry_cases: Vec<serde_json::Value>,
    }
    let contracts: Contracts = gents_lean_contract::load_contract_snapshot().unwrap();
    assert_eq!(contracts.goal_wake_ancestry_cases.len(), 19);
    let temp = tempfile::tempdir().unwrap();
    let identities = [
        KeyIdentity::load_or_create(temp.path().join("owner.key"), None).unwrap(),
        KeyIdentity::load_or_create(temp.path().join("foreign.key"), None).unwrap(),
    ];
    for value in contracts.goal_wake_ancestry_cases {
        let rows = value["rows"].as_array().unwrap();
        let case = Case {
            name: value["name"].as_str().unwrap().into(),
            scope: Scope {
                owner: 1,
                session: 1,
                goal: 1,
            },
            rows: rows
                .iter()
                .map(|row| serde_json::from_value(row["request"].clone()).unwrap())
                .collect(),
            expected: None,
            assignment_root: None,
            expected_receipt_checks: None,
        };
        let mut fixture = Fixture {
            case: &case,
            identities: &identities,
            built: RapidHashMap::new(),
        };
        let mut built = Vec::new();
        for (model, facts) in case.rows.iter().zip(rows) {
            let row = if facts["background_wake"] == true {
                let identity = fixture.identity(model.owner);
                let parent = fixture.built.get(&model.parent_request.unwrap()).cloned();
                let parent_id = parent
                    .as_ref()
                    .map(|parent| parent.request_id.clone())
                    .unwrap_or_else(|| fixture.base_id(model.parent_request.unwrap()));
                let mut create = AgentRequestCreate::base(
                    model.purpose,
                    fixture.base_id(model.request),
                    identity.did(),
                    identity.did(),
                    "head-behavior",
                    format!("session-{}", model.session),
                    "review notifications",
                    "scheduled",
                    "2026-10-21T00:00:00Z",
                    if facts["runtime_wake_receipt"] == true {
                        AgentRequestAdmissionRecord::runtime_local_control(
                            identity.did(),
                            &parent_id,
                        )
                    } else {
                        AgentRequestAdmissionRecord::local_self(identity.did())
                    },
                );
                create.caused_by_parent_request_id = Some(if model.parent_request == Some(999) {
                    fixture.base_id(999)
                } else {
                    parent_id
                });
                create.caused_by_parent_request_doc_id =
                    model.parent_doc.map(|doc| format!("physical-{doc}"));
                create.input.queue = Some(gents_protocol::request_input::RequestQueue {
                    source: gents_protocol::request_input::QueueSource::BackgroundCompletion,
                    policy: gents_protocol::request_input::QueuePolicy::Coalesce,
                    key: Some("background_completion:session-1".into()),
                    delivery: Default::default(),
                    position: None,
                    queued_after_request_id: create.caused_by_parent_request_id.clone(),
                    interrupted_request_id: None,
                    background_completion_wake_version: Some(1),
                });
                if facts["tagged"] == true {
                    create.input.goal_assignment =
                        Some(gents_protocol::request_input::GoalAssignmentInput {
                            goal_id: "goal-1".into(),
                            root_request_doc_id: Some("physical-10".into()),
                        });
                }
                crate::sign_agent_request_create(identity, &mut create)
                    .await
                    .unwrap();
                let mut row = signed_row(&create, model.doc);
                if !model.receipt_valid {
                    row.content = Some("changed after signing".into());
                }
                fixture.built.insert(model.request, row.clone());
                row
            } else {
                let mut row = fixture.build(model.clone()).await;
                if facts["tagged"] == true {
                    let mut create = AgentRequestCreate::base(
                        model.purpose,
                        row.request_id.clone(),
                        fixture.identity(model.owner).did(),
                        fixture.identity(model.owner).did(),
                        "head-behavior",
                        "session-1",
                        "entry",
                        "interactive",
                        "2026-10-21T00:00:00Z",
                        AgentRequestAdmissionRecord::local_self(
                            fixture.identity(model.owner).did(),
                        ),
                    );
                    create.input.goal_assignment =
                        Some(gents_protocol::request_input::GoalAssignmentInput {
                            goal_id: "goal-1".into(),
                            root_request_doc_id: None,
                        });
                    crate::sign_agent_request_create(fixture.identity(model.owner), &mut create)
                        .await
                        .unwrap();
                    row = signed_row(&create, model.doc);
                    fixture.built.insert(model.request, row.clone());
                }
                row
            };
            built.push(row);
        }
        let owner = identities[0].did();
        let by_doc = request_rows_by_doc(owner, "session-1", &built).unwrap();
        let current = format!("physical-{}", value["current"].as_u64().unwrap());
        let expected_goal = value["expected_goal"]
            .as_u64()
            .map(|goal| format!("goal-{goal}"));
        let opening_facts = rows
            .iter()
            .filter(|row| !row["opening_goals"].as_array().unwrap().is_empty())
            .map(|row| {
                (
                    format!("physical-{}", row["request"]["doc"].as_u64().unwrap()),
                    row["opening_goals"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|goal| format!("goal-{}", goal.as_u64().unwrap()))
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();
        let openings = opening_facts
            .iter()
            .map(|(doc, goals)| (doc.as_str(), goals.iter().map(String::as_str).collect()))
            .collect();
        let actual = authenticated_assignment_entry(
            owner,
            "session-1",
            by_doc[current.as_str()],
            &by_doc,
            expected_goal.as_deref(),
            &openings,
        )
        .ok()
        .and_then(|row| row.doc_id.clone());
        let invocation = authenticated_entry(
            owner,
            "session-1",
            by_doc[current.as_str()],
            &by_doc,
            expected_goal.as_deref(),
        )
        .ok()
        .and_then(|row| row.doc_id.clone());
        let expected = |field: &str| value[field].as_u64().map(|doc| format!("physical-{doc}"));
        assert_eq!(
            actual,
            expected("expected_entry"),
            "{}: assignment ancestry",
            case.name
        );
        assert_eq!(
            invocation,
            expected("expected_invocation_entry"),
            "{}: invocation ancestry",
            case.name
        );
    }
}

#[tokio::test]
async fn generated_goal_ancestry_cases_keep_one_signed_goal() {
    #[derive(Deserialize)]
    struct Contracts {
        goal_request_ancestry_cases: Vec<serde_json::Value>,
    }
    let contracts: Contracts = gents_lean_contract::load_contract_snapshot().unwrap();
    assert_eq!(contracts.goal_request_ancestry_cases.len(), 12);
    let temp = tempfile::tempdir().unwrap();
    let identities = [
        KeyIdentity::load_or_create(temp.path().join("owner.key"), None).unwrap(),
        KeyIdentity::load_or_create(temp.path().join("foreign.key"), None).unwrap(),
    ];
    for value in contracts.goal_request_ancestry_cases {
        let case: Case = serde_json::from_value(value.clone()).unwrap();
        let mut fixture = Fixture {
            case: &case,
            identities: &identities,
            built: RapidHashMap::new(),
        };
        let mut rows = Vec::new();
        for row in &case.rows {
            rows.push(fixture.build(row.clone()).await);
        }
        let owner = identities[(case.scope.owner - 1) as usize].did();
        let session = format!("session-{}", case.scope.session);
        let by_doc = request_rows_by_doc(owner, &session, &rows).unwrap();
        let current = format!("physical-{}", value["current"].as_u64().unwrap());
        let goal = value["expected_goal"]
            .as_u64()
            .map(|id| format!("goal-{id}"));
        let actual = authenticated_entry(
            owner,
            &session,
            by_doc[&current.as_str()],
            &by_doc,
            goal.as_deref(),
        )
        .ok()
        .and_then(|row| row.doc_id.clone());
        let expected = value["expected_entry"]
            .as_u64()
            .map(|doc| format!("physical-{doc}"));
        assert_eq!(actual, expected, "{}: signed physical ancestry", case.name);
    }
}
#[derive(Deserialize)]
struct Scope {
    owner: u64,
    session: u64,
    goal: u64,
}
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum InputDisposition {
    #[default]
    Execution,
    Folded,
    Replaced,
    Removed,
}

#[derive(Clone, Debug, Deserialize)]
struct ModelRow {
    purpose: gents_protocol::request_admission::RequestPurpose,
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
    #[serde(default)]
    input_disposition: InputDisposition,
}
#[derive(Deserialize)]
struct Case {
    name: String,
    scope: Scope,
    rows: Vec<ModelRow>,
    expected: Option<ModelRow>,
    #[serde(default)]
    assignment_root: Option<u64>,
    #[serde(default)]
    expected_receipt_checks: Option<usize>,
}

fn signed_row(create: &AgentRequestCreate, doc: u64) -> AgentRequestRow {
    AgentRequestRow {
        purpose: Some(create.purpose),
        doc_id: Some(format!("physical-{doc}")),
        request_id: create.request_id.clone(),
        node_did: Some(create.node_did.clone()),
        requester_did: Some(create.requester_did.clone()),
        agent_id: Some(create.agent_id.clone()),
        session_id: Some(create.session_id.clone()),
        retry_parent_request: create.retry_parent_request.clone(),
        retry_parent_request_doc_id: create.retry_parent_request_doc_id.clone(),
        retry_root_request: create.retry_root_request.clone(),
        retry_key: create.retry_key.clone(),
        content: Some(create.content.clone()),
        input: Some(create.input.clone()),
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
        request_hop: Some(i64::from(create.request_hop)),
        caused_by_parent_request_id: create.caused_by_parent_request_id.clone(),
        caused_by_parent_request_doc_id: create.caused_by_parent_request_doc_id.clone(),
        caused_by_parent_tool_call_id: create.caused_by_parent_tool_call_id.clone(),
        caused_by_parent_tool_call_doc_id: create.caused_by_parent_tool_call_doc_id.clone(),
        workspace_id: create.workspace_id.clone(),
        workspace_authority: create.workspace_authority.clone(),
        workspace_owner_node_did: create.workspace_owner_node_did.clone(),
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
    built: RapidHashMap<u64, AgentRequestRow>,
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
        (chrono::DateTime::parse_from_rfc3339("2026-07-15T00:00:00Z").unwrap()
            + chrono::Duration::seconds(
                i64::try_from(self.case.rows.len().max(60) - 1 - index).unwrap(),
            ))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
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
                        gents_protocol::request_admission::RequestPurpose::Normal,
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
                parent.doc_id = model.parent_doc.map(|doc| format!("physical-{doc}"));
                parent.node_did = Some(self.identity(model.owner).did().into());
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
                    parent.request_hop,
                )
                .unwrap()
            } else {
                let identity = self.identity(model.owner);
                let mut base = AgentRequestCreate::base(
                    gents_protocol::request_admission::RequestPurpose::Normal,
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
                create.caused_by_source_doc_id = if model.goal.is_none() {
                    Some("original-event-source".into())
                } else {
                    None
                };
            }
            create.purpose = model.purpose;
            if !model.deterministic_identity {
                create.request_id.push_str("-wrong-identity");
            }
            crate::sign_agent_request_create(self.identity(model.owner), &mut create)
                .await
                .unwrap();
            let mut row = signed_row(&create, model.doc);
            match model.input_disposition {
                InputDisposition::Execution => {}
                InputDisposition::Folded | InputDisposition::Replaced => {
                    row.lifecycle_state = Some(RequestLifecycleState::Superseded);
                    row.failure_reason = Some(
                        match model.input_disposition {
                            InputDisposition::Folded => crate::lifecycle::FOLDED_REASON,
                            _ => crate::lifecycle::REPLACED_REASON,
                        }
                        .into(),
                    );
                    row.superseded_by_request = Some(self.base_id(100));
                    row.superseded_by_request_doc_id = Some("physical-10".into());
                }
                InputDisposition::Removed => {
                    row.lifecycle_state = Some(RequestLifecycleState::Interrupted);
                    row.failure_reason = Some(crate::lifecycle::REMOVED_REASON.into());
                }
            }
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
    assert_eq!(snapshot.goal_request_head_cases.len(), 27);
    let temp = tempfile::tempdir().unwrap();
    let identities = [
        KeyIdentity::load_or_create(temp.path().join("owner.key"), None).unwrap(),
        KeyIdentity::load_or_create(temp.path().join("foreign.key"), None).unwrap(),
    ];
    for case in snapshot.goal_request_head_cases {
        let goal: GoalDocument = serde_json::from_value(serde_json::json!({
            "_docID": "goal-doc", "goal_id": format!("goal-{}", case.scope.goal),
            "node_did": identities[(case.scope.owner - 1) as usize].did(),
            "session_id": format!("session-{}", case.scope.session), "status": "active",
            "assignment_root_request_doc_id": case.assignment_root.map(|doc| format!("physical-{doc}")),
        }))
        .unwrap();
        let mut fixture = Fixture {
            case: &case,
            identities: &identities,
            built: RapidHashMap::new(),
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
        let (selected, verifications) =
            crate::request_admission::count_request_receipt_verifications(|| {
                latest_goal_request(&goal, &rows).and_then(|row| row.doc_id.as_deref())
            });
        if let Some(expected_checks) = case.expected_receipt_checks {
            assert!(
                verifications <= expected_checks,
                "{}: {verifications} receipt checks exceed the Lean plan bound {expected_checks}",
                case.name,
            );
        }
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

#[tokio::test]
async fn task_assignment_root_fences_older_chain_even_when_its_child_is_newer() {
    use crate::config_client::ConfigAccess;
    let temp = tempfile::tempdir().unwrap();
    let identity = KeyIdentity::load_or_create(temp.path().join("owner.key"), None).unwrap();
    let node = std::sync::Arc::new(
        defra_node::EmbeddedNode::builder()
            .with_node_identity_did(identity.did())
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let access = ConfigAccess::local(node.clone());
    let mut goal = set_goal(
        &node,
        identity.did(),
        "bound-session",
        Some("next assignment"),
        Some(GoalStatus::Active),
        Some(None),
    )
    .await
    .unwrap();
    for (id, created) in [
        ("old-root", "2026-01-01T00:00:00Z"),
        ("new-root", "2026-01-01T00:00:01Z"),
    ] {
        let mut create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            id,
            identity.did(),
            identity.did(),
            "head-behavior",
            "bound-session",
            "root",
            "interactive",
            created,
            AgentRequestAdmissionRecord::local_self(identity.did()),
        );
        crate::sign_agent_request_create(&identity, &mut create)
            .await
            .unwrap();
        access
            .write("test.assignment_root", &create.graphql_mutation().unwrap())
            .await
            .unwrap();
    }
    let query = format!(
        "{{AgentRequest(order: {{created_at: DESC}}) {{{}}}}}",
        crate::request_admission::SIGNED_REQUEST_FIELDS
    );
    let response = access.execute(&query).await.unwrap();
    let rows: Vec<AgentRequestRow> =
        serde_json::from_value(response["data"]["AgentRequest"].clone()).unwrap();
    let old = rows
        .iter()
        .find(|row| row.request_id == "old-root")
        .unwrap();
    let new = rows
        .iter()
        .find(|row| row.request_id == "new-root")
        .unwrap();
    let new_doc = new.doc_id.as_deref().unwrap();
    goal.assignment_root_request_doc_id = Some(new_doc.into());
    access.write("test.assignment_binding", &format!("mutation {{update_Goal(filter: {{_docID: {{_eq: \"{}\"}}}}, input: {{assignment_root_request_doc_id: \"{}\"}}) {{_docID}}}}",
        escape_graphql_string(&goal.doc_id), escape_graphql_string(new_doc))).await.unwrap();
    let mut parent = crate::watcher::AgentRequest::try_from(old.clone()).unwrap();
    for sequence in [1, 2] {
        let mut child = prepare_goal_continuation(
            &parent,
            "head-behavior".into(),
            &goal.goal_id,
            "old continuation",
            sequence,
            false,
            "2026-01-01T00:00:02Z",
            parent.request_hop,
        )
        .unwrap();
        crate::sign_agent_request_create(&identity, &mut child)
            .await
            .unwrap();
        access
            .write("test.old_continuation", &child.graphql_mutation().unwrap())
            .await
            .unwrap();
        let response = access.execute(&query).await.unwrap();
        let rows: Vec<AgentRequestRow> =
            serde_json::from_value(response["data"]["AgentRequest"].clone()).unwrap();
        assert_eq!(
            latest_goal_request(&goal, &rows).unwrap().request_id,
            "new-root"
        );
        let old_child = rows
            .iter()
            .find(|row| row.request_id == child.request_id)
            .unwrap();
        let request = crate::watcher::AgentRequest::try_from(old_child.clone()).unwrap();
        let allowed =
            ConfigAccess::transact_local_write(&node, None, "test.late_goal_claim", |txn| {
                let request = &request;
                Box::pin(async move {
                    fence_goal_continuation_claim_in_txn(txn, request, "2026-01-01T00:00:03Z").await
                })
            })
            .await
            .unwrap();
        assert!(!allowed);
        let response = access.execute(&query).await.unwrap();
        let rows: Vec<AgentRequestRow> =
            serde_json::from_value(response["data"]["AgentRequest"].clone()).unwrap();
        assert_eq!(
            rows.iter()
                .find(|row| row.request_id == child.request_id)
                .unwrap()
                .lifecycle_state,
            Some(RequestLifecycleState::Superseded)
        );
        parent = crate::watcher::AgentRequest::try_from(
            rows.iter()
                .find(|row| row.request_id == child.request_id)
                .unwrap()
                .clone(),
        )
        .unwrap();
    }
}
