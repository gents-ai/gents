//! Child of graph_pipeline::run. Real installed plan, signed requests, Workspace
//! owner provisioning, and native publication transaction; no policy simulator.
use super::*;
use crate::identity::{AgentIdentity, KeyIdentity};
use crate::lifecycle::WorkspaceLineage;
use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};

struct Fixture {
    node: Arc<EmbeddedNode>,
    identity: KeyIdentity,
    run: super::super::runtime::GraphRunReceipt,
    plan: GraphPlan,
    workspace: crate::workspace::CreateWorkspaceOutcome,
    _temp: tempfile::TempDir,
}

async fn execute(node: &EmbeddedNode, query: &str) -> Value {
    let response = node.execute(query).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    response.data.unwrap()
}

fn git(repo: &std::path::Path, args: &[&str]) -> String {
    let result = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap().trim().to_owned()
}

impl Fixture {
    async fn new(bound: bool, conflicting_input: bool) -> Self {
        use crate::graph_package::{install_bundled_graph_package, GraphPackageInstallBindings};
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let identity = super::super::runtime::graph_test_identity();
        crate::document_config::ensure_agent_principal(&node, identity.did())
            .await
            .unwrap();
        for mutation in [
            r#"mutation { create_HostDeployment(input: {deployment_id:"graph-test-host",display_name:"Graph test"}) {_docID} }"#,
            r#"mutation { create_InferenceBackend(input: {backend_id:"graph-test-backend",name:"Graph test",provider_kind:"OpenAiCompatible",endpoint:"http://127.0.0.1:1/v1",max_concurrent:4,enabled:true,models:["test-model"]}) {_docID} }"#,
            r#"mutation { create_InferenceProfile(input: {profile_id:"graph-test-profile",display_name:"Graph test",max_turns:8}) {_docID} }"#,
        ] {
            execute(&node, mutation).await;
        }
        let role = super::super::PackageRoleBinding {
            principal_did: identity.did().into(),
            deployment_id: "graph-test-host".into(),
            backend_id: Some("graph-test-backend".into()),
            profile_id: Some("graph-test-profile".into()),
            model_name: Some("test-model".into()),
        };
        let access = ConfigAccess::Local(node.clone());
        let installed = install_bundled_graph_package(
            &access,
            identity.did(),
            "code-review",
            &GraphPackageInstallBindings {
                owner_did: identity.did().into(),
                roles: BTreeMap::from([
                    ("coordinator".into(), role.clone()),
                    ("reviewer".into(), role),
                ]),
            },
        )
        .await
        .unwrap();
        super::super::activate_graph_revision(
            &node,
            None,
            identity.did(),
            &installed.graph_id,
            &installed.revision_digest,
            None,
        )
        .await
        .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.name", "Graph test"]);
        git(&repo, &["config", "user.email", "graph@test.invalid"]);
        std::fs::write(repo.join("README.md"), "immutable review source\n").unwrap();
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-qm", "base"]);
        let head = git(&repo, &["rev-parse", "HEAD"]);
        let workspace = crate::workspace::provision_read_only_workspace(
            &access,
            &repo,
            &head,
            "graph-test-host",
            identity.did(),
        )
        .await
        .unwrap();
        assert_eq!(workspace.workspace.lifecycle_state, "sealed");
        assert!(workspace.workspace.seal_hash.is_some());
        let mut input = json!({"repository_path":".","base_ref":head,"head_ref":head,
            "lens_count":"1","lens_min":"1","lens_max":"1","focus":"lineage"});
        if bound {
            input["workspace_id"] = json!(if conflicting_input {
                "missing-workspace"
            } else {
                &workspace.workspace.workspace_id
            });
            input["workspace_owner_deployment_id"] = json!("graph-test-host");
            input["workspace_authority"] = json!("readOnly");
        }
        let run = super::super::start_graph_run(
            &node,
            None,
            identity.did(),
            &installed.graph_id,
            None,
            "review",
            input,
        )
        .await
        .unwrap();
        let plan = load_plan(node.as_ref(), &installed.revision_digest, identity.did())
            .await
            .unwrap();
        Self {
            node,
            identity,
            run,
            plan,
            workspace,
            _temp: temp,
        }
    }
    fn tuple(&self) -> WorkspaceLineage {
        WorkspaceLineage {
            workspace_id: Some(self.workspace.workspace.workspace_id.clone()),
            workspace_authority: Some("readOnly".into()),
            workspace_owner_deployment_id: Some("graph-test-host".into()),
            workspace_seal_hash: self.workspace.workspace.seal_hash.clone(),
        }
    }
    fn route(&self, node: &str) -> String {
        planned_trigger_nodes(&self.plan)
            .unwrap()
            .into_iter()
            .find(|(_, n)| n == node)
            .unwrap()
            .0
    }
    async fn request(
        &self,
        id: &str,
        node: &str,
        lineage: &WorkspaceLineage,
    ) -> AgentRequestCreate {
        let trigger = self.route(node);
        let response = execute(
            &self.node,
            &format!(
                "{{ EventTrigger(filter:{{trigger_id:{{_eq:\"{}\"}}}}) {{_docID task_id}} }}",
                escape_graphql_string(&trigger)
            ),
        )
        .await;
        let row = &response["EventTrigger"][0];
        let task = execute(
            &self.node,
            &format!(
                "{{ Task(filter:{{task_id:{{_eq:\"{}\"}}}}) {{behavior_id}} }}",
                escape_graphql_string(row["task_id"].as_str().unwrap())
            ),
        )
        .await;
        let mut request = AgentRequestCreate::base(
            id,
            self.identity.did(),
            self.identity.did(),
            task["Task"][0]["behavior_id"].as_str().unwrap(),
            &format!("session-{id}"),
            "review source",
            "scheduled",
            "2026-09-05T00:00:00Z",
            AgentRequestAdmissionRecord::runtime_automated_trigger(self.identity.did(), &trigger),
        );
        request.caused_by_trigger_id = Some(trigger);
        request.caused_by_trigger_kind = Some("event".into());
        request.caused_by_trigger_doc_id = Some(row["_docID"].as_str().unwrap().into());
        request.caused_by_correlation = Some(self.run.correlation.clone());
        // Bootstrap uses the actual seed. Downstream resolver intentionally derives
        // lineage from entry receipt, not the content of this discovery pointer.
        request.caused_by_source_doc_id = Some(self.run.seed_doc_id.clone());
        request.workspace_id = lineage.workspace_id.clone();
        request.workspace_authority = lineage.workspace_authority.clone();
        request.workspace_owner_deployment_id = lineage.workspace_owner_deployment_id.clone();
        request.workspace_seal_hash = lineage.workspace_seal_hash.clone();
        crate::sign_agent_request_create(&self.identity, &mut request)
            .await
            .unwrap();
        request
    }
    async fn observe(
        &self,
        node: &str,
        explicit: &WorkspaceLineage,
    ) -> Result<Option<workspace_lineage::GraphWorkspaceResolution>> {
        workspace_lineage::resolve_graph_workspace(
            self.node.as_ref(),
            &self.route(node),
            Some(&self.run.correlation),
            self.identity.did(),
            Some(&self.run.seed_doc_id),
            explicit,
        )
        .await
    }
}

fn abstract_tuple(fx: &Fixture, lineage: &WorkspaceLineage) -> Value {
    let workspace = lineage.workspace_id.as_ref().map(|id| {
        assert_eq!(id, &fx.workspace.workspace.workspace_id);
        assert_eq!(
            lineage.workspace_owner_deployment_id.as_deref(),
            Some("graph-test-host")
        );
        assert_eq!(
            lineage.workspace_seal_hash,
            fx.workspace.workspace.seal_hash
        );
        json!({"workspace_id":11,"owner":21,"seal_hash":31})
    });
    json!({"workspace":workspace,"authority":lineage.workspace_authority})
}

fn explicit_from_case(fx: &Fixture, explicit: &Value) -> WorkspaceLineage {
    let id = |key: &str, good: u64, actual: String| {
        explicit[key].as_u64().map(|n| {
            if n == good {
                actual
            } else {
                format!("conflict-{n}")
            }
        })
    };
    WorkspaceLineage {
        workspace_id: id(
            "workspace_id",
            11,
            fx.workspace.workspace.workspace_id.clone(),
        ),
        workspace_owner_deployment_id: id("owner", 21, "graph-test-host".into()),
        workspace_seal_hash: id(
            "seal_hash",
            31,
            fx.workspace.workspace.seal_hash.clone().unwrap(),
        ),
        workspace_authority: explicit["authority"].as_str().map(str::to_owned),
    }
}

#[tokio::test]
async fn generated_graph_workspace_cases_drive_installed_plan_and_signed_receipts() {
    let snapshot: Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases = snapshot["graph_workspace_lineage_cases"]
        .as_array()
        .unwrap();
    assert_eq!(cases.len(), 25);
    let mut tested = 0;
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let roots = case["source"]["roots"].as_array();
        let bound = roots
            .and_then(|r| r.first())
            .is_none_or(|r| !r["workspace"].is_null());
        let fx = Fixture::new(bound, name == "bootstrap_controller_input_conflict").await;
        if let Some(roots) = roots {
            for root in roots {
                let tuple = if root["workspace"].is_null() {
                    WorkspaceLineage::default()
                } else {
                    fx.tuple()
                };
                let mut request = fx
                    .request(&format!("entry-{}", root["doc_id"]), "recon", &tuple)
                    .await;
                if root["correlation"] != 1 {
                    request.caused_by_correlation = Some("another-run".into());
                }
                if root["revision"] != 2 {
                    request.caused_by_trigger_id = Some(
                        super::super::runtime::graph_trigger_id(
                            &format!("sha256:{}", "a".repeat(64)),
                            "entry:review:recon:job",
                        )
                        .unwrap(),
                    );
                    request.admission.runtime_source_request_id =
                        request.caused_by_trigger_id.clone();
                }
                crate::sign_agent_request_create(&fx.identity, &mut request)
                    .await
                    .unwrap();
                if root["authenticated_target"] == false {
                    // Tamper after signing: a copied route cannot authenticate.
                    request.content.push_str(" unauthenticated mutation");
                }
                execute(&fx.node, &request.graphql_mutation().unwrap()).await;
            }
        }
        if name == "stale_publication_generation" {
            native_stale_publication_rolls_back_child(&fx).await;
            tested += 1;
            fx.node.shutdown().await;
            continue;
        }
        let destination = if case["source"]["kind"] == "bootstrap" {
            "recon"
        } else if case["context"]["destination_authority"].is_null() {
            "triage"
        } else if name == "scan_to_verifier_partial_matching_context" {
            "verify"
        } else {
            "scan"
        };
        let explicit = explicit_from_case(&fx, &case["explicit"]);
        if case["cancelled"] == true {
            let txn = ConfigApplyTxn::begin_local(&fx.node, None).await.unwrap();
            persist_cancellation_intent(
                txn,
                fx.identity.did(),
                &fx.run.run_id,
                Some("stop review"),
            )
            .await
            .unwrap();
        }
        if !case["primary_cause"].is_null() {
            execute(&fx.node,r#"mutation { update_AgentRequest(filter:{request_id:{_eq:"entry-41"}},input:{lifecycle_state:"failed",failure_reason:"observed provider failure"}) {_docID} }"#).await;
            let view = load_graph_run_view(&fx.node, fx.identity.did(), &fx.run.run_id)
                .await
                .unwrap();
            let txn = ConfigApplyTxn::begin_local(&fx.node, None).await.unwrap();
            capture_failure_txn(txn, &view).await.unwrap();
            assert!(
                query_run(fx.node.as_ref(), &fx.run.run_id).await.unwrap()["error"].is_string()
            );
        }
        let observed = if case["context"]["destination_route_verified"] == false {
            workspace_lineage::resolve_graph_workspace(
                fx.node.as_ref(),
                &super::super::runtime::graph_trigger_id(&fx.run.revision_digest, "unplanned")
                    .unwrap(),
                Some(&fx.run.correlation),
                fx.identity.did(),
                Some(&fx.run.seed_doc_id),
                &explicit,
            )
            .await
        } else {
            fx.observe(destination, &explicit).await
        };
        let stopped = case["cancelled"] == true || !case["primary_cause"].is_null();
        if stopped || case["expected"].is_null() {
            assert!(observed.is_err(), "{name}: unexpected permitted projection");
            assert_eq!(case["published"], false, "{name}");
            let mut candidate = fx.request("candidate", destination, &explicit).await;
            if case["context"]["destination_route_verified"] == false {
                candidate.caused_by_trigger_id = Some(
                    super::super::runtime::graph_trigger_id(&fx.run.revision_digest, "unplanned")
                        .unwrap(),
                );
                candidate.admission.runtime_source_request_id =
                    candidate.caused_by_trigger_id.clone();
                crate::sign_agent_request_create(&fx.identity, &mut candidate)
                    .await
                    .unwrap();
            }
            let before = query_run(fx.node.as_ref(), &fx.run.run_id).await.unwrap();
            let txn = ConfigApplyTxn::begin_local(&fx.node, None).await.unwrap();
            assert!(
                workspace_lineage::fence_root_workspace_in_txn(&txn, &candidate)
                    .await
                    .is_err(),
                "{name}"
            );
            txn.discard().await.unwrap();
            let after = query_run(fx.node.as_ref(), &fx.run.run_id).await.unwrap();
            assert_eq!(
                after["update_generation"], before["update_generation"],
                "{name}"
            );
        } else {
            assert_eq!(case["published"], true, "{name}");
            let resolved = observed.unwrap().unwrap();
            assert_eq!(
                abstract_tuple(&fx, &resolved.lineage),
                case["expected"],
                "{name}"
            );
            let tuple = if resolved.lineage.workspace_id.is_some() {
                resolved.lineage
            } else {
                WorkspaceLineage::default()
            };
            let candidate = fx.request("candidate", destination, &tuple).await;
            let before = query_run(fx.node.as_ref(), &fx.run.run_id).await.unwrap();
            let txn = ConfigApplyTxn::begin_local(&fx.node, None).await.unwrap();
            workspace_lineage::fence_root_workspace_in_txn(&txn, &candidate)
                .await
                .unwrap();
            txn.execute(&candidate.graphql_mutation().unwrap())
                .await
                .unwrap();
            txn.commit().await.unwrap();
            let after = query_run(fx.node.as_ref(), &fx.run.run_id).await.unwrap();
            assert_eq!(
                after["update_generation"].as_i64(),
                before["update_generation"].as_i64().map(|n| n + 1)
            );
        }
        tested += 1;
        fx.node.shutdown().await;
        drop(fx);
    }
    assert_eq!(tested, 25);
}

#[tokio::test]
async fn cleaned_source_attenuates_for_triage_but_bound_destination_cannot_upgrade_entry_seal() {
    let fx = Fixture::new(true, false).await;
    let root = fx.request("entry", "recon", &fx.tuple()).await;
    execute(&fx.node, &root.graphql_mutation().unwrap()).await;
    execute(&fx.node,&format!("mutation {{ update_IsolatedWorkspace(filter:{{workspace_id:{{_eq:\"{}\"}}}},input:{{lifecycle_state:\"cleaned\"}}) {{_docID}} }}",
        escape_graphql_string(&fx.workspace.workspace.workspace_id))).await;
    let triage = fx
        .observe("triage", &WorkspaceLineage::default())
        .await
        .unwrap()
        .unwrap();
    assert!(triage.lineage.workspace_id.is_none());
    assert!(triage.lineage.workspace_authority.is_none());
    assert!(fx
        .observe("scan", &WorkspaceLineage::default())
        .await
        .is_err());
    drop(fx);
    let fx = Fixture::new(true, false).await;
    let mut historical = fx.tuple();
    historical.workspace_seal_hash = None;
    let root = fx
        .request("historical-ready-entry", "recon", &historical)
        .await;
    execute(&fx.node, &root.graphql_mutation().unwrap()).await;
    let error = fx
        .observe("scan", &WorkspaceLineage::default())
        .await
        .err()
        .unwrap();
    assert!(
        error.to_string().contains("immutable entry seal"),
        "{error:#}"
    );
}

async fn native_stale_publication_rolls_back_child(fx: &Fixture) {
    struct Native<'a> {
        node: &'a EmbeddedNode,
        handle: &'a query::TransactionHandle,
    }
    #[async_trait::async_trait]
    impl GraphRunQuery for Native<'_> {
        async fn execute_graph_query(&self, text: &str) -> Result<Value> {
            let response = self
                .node
                .execute_request_in_txn(defra_node::QueryRequest::new(text), self.handle)
                .await;
            anyhow::ensure!(
                !response.has_errors(),
                "native query failed: {:?}",
                response.errors
            );
            Ok(json!({"data":response.data.unwrap_or(Value::Null)}))
        }
    }
    // Supported local ConfigApplyTxn serializes writes; direct native handles
    // deliberately expose storage conflict resolution like existing graph tests.
    let left = fx.node.runner().begin_txn(false).await.unwrap();
    let right = fx.node.runner().begin_txn(false).await.unwrap();
    let one = Native {
        node: &fx.node,
        handle: &left,
    };
    let two = Native {
        node: &fx.node,
        handle: &right,
    };
    let trigger = fx.route("scan");
    let first = workspace_lineage::resolve_graph_workspace(
        &one,
        &trigger,
        Some(&fx.run.correlation),
        fx.identity.did(),
        Some(&fx.run.seed_doc_id),
        &WorkspaceLineage::default(),
    )
    .await
    .unwrap()
    .unwrap();
    let second = workspace_lineage::resolve_graph_workspace(
        &two,
        &trigger,
        Some(&fx.run.correlation),
        fx.identity.did(),
        Some(&fx.run.seed_doc_id),
        &WorkspaceLineage::default(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        first.run["update_generation"],
        second.run["update_generation"]
    );
    let initial = first.run["update_generation"].as_i64().unwrap();
    let winner = fx.request("native-winner", "scan", &first.lineage).await;
    let loser = fx.request("native-loser", "scan", &second.lineage).await;
    for (executor, observed, request) in [(&one, &first, &winner), (&two, &second, &loser)] {
        // This is the SAME validated mutation builder used by the production
        // fence, not a test reimplementation of generation or admission policy.
        let mutation = super::super::runtime::graph_publication_generation_update(
            &observed.run,
            &observed.digest,
        )
        .unwrap();
        let changed = executor.execute_graph_query(&mutation).await.unwrap();
        assert_eq!(rows(&changed, "update_GraphRun").len(), 1);
        let staged = executor
            .execute_graph_query(&request.graphql_mutation().unwrap())
            .await
            .unwrap();
        let created = staged
            .pointer("/data/create_AgentRequest")
            .or_else(|| staged.pointer("/data/add_AgentRequest"))
            .expect("native create result");
        let created = if let Some(rows) = created.as_array() {
            assert_eq!(rows.len(), 1);
            &rows[0]
        } else {
            created
        };
        assert!(created["_docID"].is_string());
    }
    fx.node.runner().commit_txn(&left).await.unwrap();
    let conflict = fx
        .node
        .runner()
        .commit_txn(&right)
        .await
        .expect_err("stale native publication must lose");
    assert!(
        crate::graphql::is_defradb_transaction_conflict_text(&conflict.to_string()),
        "{conflict}"
    );
    let durable = execute(
        &fx.node,
        "{ AgentRequest {request_id} GraphRun {update_generation} }",
    )
    .await;
    assert_eq!(
        durable["GraphRun"][0]["update_generation"].as_i64(),
        Some(initial + 1)
    );
    let requests = durable["AgentRequest"].as_array().unwrap();
    assert!(requests
        .iter()
        .any(|row| row["request_id"] == winner.request_id));
    assert!(
        !requests
            .iter()
            .any(|row| row["request_id"] == loser.request_id),
        "losing transaction must not leave a partially published request"
    );
}

// Append to graph_pipeline::run::workspace_lineage_tests, reusing its Fixture.
#[tokio::test]
async fn installed_review_area_handoff_materializes_bound_goal_scanner() {
    use crate::defra_write::{BoundedWriteParams, BoundedWriteTool};
    use crate::document_config::SurfaceToolDecl;
    use crate::llm::tool::Tool;
    use crate::runtime_snapshot::{
        ConcurrencyMode, EventTriggerFireMode, ResolvedEventTrigger, ResolvedRuntimeSnapshot,
        ResolvedTask,
    };
    use crate::tool_surface::{BehaviorToolConfig, ToolCeiling, ToolSelection};
    use crate::trigger_engine::{MaterializerHandle, TriggerKind, TriggerSource};
    use std::collections::{HashMap, HashSet};
    use tokio::sync::watch;
    use tokio_util::sync::CancellationToken;

    let fx = Fixture::new(true, false).await;
    let package = crate::graph_package::load_bundled_graph_package("code-review").unwrap();
    let mut tasks = HashMap::new();
    let mut routes = HashMap::new();
    let mut behaviors = Vec::new();
    let mut surfaces = HashMap::new();
    let mut write_area = None;
    for stage in ["recon", "scan"] {
        let trigger = fx.route(stage);
        let stored = execute(&fx.node, &format!(
            "{{ EventTrigger(filter:{{trigger_id:{{_eq:\"{}\"}}}}) {{_docID task_id source_collection}} }}",
            escape_graphql_string(&trigger),
        )).await;
        let trigger_row = &stored["EventTrigger"][0];
        let task_rows = execute(&fx.node, &format!(
            "{{ Task(filter:{{task_id:{{_eq:\"{}\"}}}}) {{task_id name behavior_id prompt_template goal_objective_template goal_token_budget output_schema_ref}} }}",
            escape_graphql_string(trigger_row["task_id"].as_str().unwrap()),
        )).await;
        let row = &task_rows["Task"][0];
        let task = ResolvedTask {
            task_id: row["task_id"].as_str().unwrap().into(),
            name: row["name"].as_str().map(str::to_owned),
            behavior_id: row["behavior_id"].as_str().unwrap().into(),
            prompt_template: row["prompt_template"].as_str().unwrap().into(),
            goal_objective_template: row["goal_objective_template"].as_str().map(str::to_owned),
            goal_token_budget: row["goal_token_budget"].as_i64(),
            output_schema_ref: row["output_schema_ref"].as_str().map(str::to_owned),
        };
        let declared: Value = serde_json::from_str(
            package
                .asset_text(&format!(
                    "datastore-tool-surfaces/review-{stage}-writes/object.json"
                ))
                .unwrap(),
        )
        .unwrap();
        let entries: Vec<SurfaceToolDecl> = declared["entries"]
            .as_array()
            .unwrap()
            .iter()
            .cloned()
            .map(serde_json::from_value)
            .collect::<Result<_, _>>()
            .unwrap();
        let mut selection = ToolSelection::default();
        // Include the installed query declarations: their runtime source fills
        // tell EventSource which evidence fields the scanner must inherit.
        for entry in entries {
            match entry {
                SurfaceToolDecl::Create(write) => selection.write_tools.push(write),
                SurfaceToolDecl::Query(query) => selection.query_tools.push(query),
            }
        }
        if stage == "recon" {
            write_area = Some(selection.write_tools[0].clone());
        }
        selection.enable_goal_tools = true;
        selection.enable_goal_creation = false;
        let mut behavior = crate::agent::PendingAgentBehavior::new(&task.behavior_id)
            .build_with_identity_for_test(super::super::runtime::graph_test_identity());
        behavior.backend_id = Some("graph-test-backend".into());
        behavior.tools = BehaviorToolConfig::from_selection(
            &task.behavior_id,
            selection,
            &ToolCeiling::readwrite(&fx.workspace.placement.host_path),
            Vec::new(),
        )
        .unwrap();
        surfaces.insert(
            task.behavior_id.clone(),
            Arc::new(behavior.tools.resolve(&fx.node).await.unwrap()),
        );
        behaviors.push(Arc::new(behavior));
        routes.insert(
            stage,
            (
                trigger,
                trigger_row["_docID"].as_str().unwrap().to_owned(),
                trigger_row["source_collection"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            ),
        );
        tasks.insert(stage, task);
    }
    let scan_route = &routes["scan"];
    assert_eq!(scan_route.2, "CodeReviewArea");
    let trigger = ResolvedEventTrigger {
        trigger_doc_id: scan_route.1.clone(),
        trigger_id: scan_route.0.clone(),
        task_id: tasks["scan"].task_id.clone(),
        task: tasks["scan"].clone(),
        source_collection: scan_route.2.clone(),
        event_kind: "created".into(),
        filter: None,
        enabled: true,
        concurrency: ConcurrencyMode::Parallel,
        fire_mode: EventTriggerFireMode::PerDocument,
        correlation_field: Some("run_id".into()),
        expected_count: None,
        expected_count_field: None,
        group_timeout_secs: None,
        group_min_count: 1,
        workspace_authority: Some("readOnly".into()),
    };
    let principal = behaviors[0].principal.clone();
    let snapshot = Arc::new(
        ResolvedRuntimeSnapshot::from_parts_with_admission_configs(
            tasks["recon"].behavior_id.clone(),
            behaviors,
            surfaces,
            HashMap::new(),
            HashMap::new(),
        )
        .with_event_triggers(
            HashMap::from([(scan_route.0.clone(), trigger)]),
            HashSet::new(),
        )
        .with_principal(principal)
        .activate(1, HashMap::new()),
    );
    let (_snapshot_tx, snapshot_rx) = watch::channel(snapshot.clone());
    let materializer = crate::trigger_engine::production_materializer::ProductionMaterializer::new(
        fx.node.clone(),
        snapshot_rx.clone(),
    )
    .with_local_deployment_id("graph-test-host");
    let mut source = crate::trigger_engine::event_source::EventSource::new(
        snapshot_rx,
        fx.node.clone(),
        CancellationToken::new(),
    );
    source.reconcile_subscriptions(snapshot.as_ref()).await;
    // Only the entry receives the operator's explicit tuple. Child input comes
    // from the actual domain event and contains no copied workspace fields.
    let root_context = serde_json::to_string(&crate::lifecycle::TriggerExecutionContext {
        version: 1,
        source_fields: BTreeMap::from([
            ("repository_path".into(), ".".into()),
            ("evidence_id".into(), "review-handoff-evidence".into()),
            (
                "workspace_id".into(),
                fx.workspace.workspace.workspace_id.clone(),
            ),
            ("workspace_authority".into(), "readOnly".into()),
            (
                "workspace_owner_deployment_id".into(),
                "graph-test-host".into(),
            ),
            (
                "workspace_seal_hash".into(),
                fx.workspace.workspace.seal_hash.clone().unwrap(),
            ),
        ]),
    })
    .unwrap();
    let root_id = materializer
        .materialize(
            &tasks["recon"],
            Some(&routes["recon"].0),
            TriggerKind::Event,
            Some(&routes["recon"].1),
            Some(&fx.run.seed_doc_id),
            Some(&fx.run.correlation),
            Some(&root_context),
            "Publish one review area",
            Some("Close this review area set"),
            &format!("event:{}:doc:{}", routes["recon"].0, fx.run.seed_doc_id),
        )
        .await
        .unwrap();
    let tool = BoundedWriteTool::new(fx.node.clone(), write_area.unwrap());
    crate::tool_call_lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
        None,
        CancellationToken::new(),
        None,
        None,
        None,
        Some(fx.run.correlation.clone()),
        BTreeMap::from([
            ("repository_path".into(), ".".into()),
            ("evidence_id".into(), "review-handoff-evidence".into()),
        ]),
        false,
        async {
            Tool::call(
                &tool,
                BoundedWriteParams(
                    serde_json::json!({
                        "area_id": format!("{}:correctness", fx.run.correlation),
                        "lens": "correctness", "path": "README.md",
                        "instructions": "Check this one file", "baseline": "No baseline errors",
                        "expected_total": "1",
                    })
                    .as_object()
                    .unwrap()
                    .clone(),
                ),
            )
            .await
            .unwrap();
        },
    )
    .await;
    let fire = tokio::time::timeout(std::time::Duration::from_secs(5), source.next_fire())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fire.trigger_id.as_deref(), Some(scan_route.0.as_str()));
    let source_doc = fire.event_vars["source_doc_id"].as_str().unwrap();
    let context =
        crate::lifecycle::TriggerExecutionContext::parse(fire.trigger_context.as_deref()).unwrap();
    assert_eq!(context.source_fields["expected_total"], "1");
    // Runtime-filled fields are durable on the emitted area, while EventSource
    // carries only fields used by the destination prompt/tool surface.
    let area = execute(
        &fx.node,
        &format!(
            "{{ CodeReviewArea(filter: {{ _docID: {{ _eq: \"{}\" }} }} ) {{ repository_path evidence_id }} }}",
            escape_graphql_string(source_doc),
        ),
    )
    .await;
    let area_rows = area["CodeReviewArea"].as_array().unwrap();
    assert_eq!(area_rows.len(), 1);
    assert_eq!(area_rows[0]["repository_path"], ".");
    assert_eq!(area_rows[0]["evidence_id"], "review-handoff-evidence");
    assert!(!context.source_fields.contains_key("repository_path"));
    assert_eq!(
        context.source_fields["evidence_id"],
        "review-handoff-evidence"
    );
    assert!(!context.source_fields.contains_key("workspace_id"));
    let scanner = materializer
        .materialize(
            &fire.task,
            fire.trigger_id.as_deref(),
            fire.trigger_kind,
            Some(&scan_route.1),
            Some(source_doc),
            fire.correlation.as_deref(),
            fire.trigger_context.as_deref(),
            "Scan the emitted area",
            Some("Publish the required scan sentinel"),
            &fire.durable_fire_key,
        )
        .await
        .unwrap();
    let requests = execute(
        &fx.node,
        &format!(
            "{{ AgentRequest {{ {} }} }}",
            crate::request_admission::SIGNED_REQUEST_FIELDS
        ),
    )
    .await;
    let rows: Vec<gents_protocol::row::AgentRequestRow> =
        serde_json::from_value(requests["AgentRequest"].clone()).unwrap();
    assert_eq!(rows.len(), 2);
    for id in [&root_id, &scanner] {
        let row = rows.iter().find(|row| &row.request_id == id).unwrap();
        crate::request_admission::verify_request_receipt_signature(row).unwrap();
        assert_eq!(row.workspace_id, fx.tuple().workspace_id);
        assert_eq!(row.workspace_authority.as_deref(), Some("readOnly"));
        assert_eq!(
            row.workspace_owner_deployment_id.as_deref(),
            Some("graph-test-host")
        );
        assert_eq!(row.workspace_seal_hash, fx.workspace.workspace.seal_hash);
    }
    let scan = rows.iter().find(|row| row.request_id == scanner).unwrap();
    assert_eq!(scan.caused_by_source_doc_id.as_deref(), Some(source_doc));
    assert_ne!(source_doc, fx.run.seed_doc_id);
    assert_eq!(
        scan.caused_by_trigger_doc_id.as_deref(),
        Some(scan_route.1.as_str())
    );
    let goals = execute(&fx.node, "{ Goal { goal_id status } }").await;
    assert_eq!(goals["Goal"].as_array().unwrap().len(), 2);
    drop(source);
    drop(materializer);
    fx.node.shutdown().await;
}
