use super::shared::{MetaToolError, StructuredToolError};

fn make_call_result(texts: &[&str]) -> rmcp::model::CallToolResult {
    use rmcp::model::CallToolResult;

    let content = texts
        .iter()
        .map(|t| rmcp::model::Content::text(*t))
        .collect();

    CallToolResult::success(content)
}

#[test]
fn structured_meta_tool_error_becomes_a_typed_dispatch_failure() {
    let error = StructuredToolError::invalid_tool_arguments(
        "x-data",
        "query",
        "/arguments/limit",
        "limit must be an integer",
    );
    let dispatch_error = MetaToolError::structured(error).into_dispatch_error();
    let outcome =
        crate::tools::call::lifecycle::ToolOutcome::from_dispatch("call_tool", Err(dispatch_error));

    match outcome {
        crate::tools::call::lifecycle::ToolOutcome::Failed { class, text, .. } => {
            assert_eq!(
                class,
                crate::tools::call::lifecycle::FailureClass::ArgumentInvalid
            );
            let value: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(value["ok"], false);
            assert_eq!(value["failure_class"], "invalid_tool_arguments");
        }
        other => panic!("structured meta-tool error must be typed failed, got {other:?}"),
    }
}

pub(super) fn remote_selection(services: &[&str], names: &[&str]) -> crate::documents::RemoteTools {
    crate::documents::RemoteTools {
        services: services
            .iter()
            .map(|service| crate::documents::RemoteServiceTools {
                mcp_service_id: service.to_string(),
                tool_names: names.iter().map(|name| name.to_string()).collect(),
                ..Default::default()
            })
            .collect(),
    }
}

use super::{CallToolArgs, CallToolTool, MetaToolContext};
use crate::documents::RemoteToolStyle;
use crate::inference::llm::tool::Tool;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

pub(crate) async fn test_node() -> (Arc<defra_node::Node>, crate::KeyIdentity) {
    use crate::store::identity::NodeIdentity;
    let directory = tempfile::tempdir().unwrap();
    let runtime =
        crate::KeyIdentity::load_or_create(directory.path().join("runtime"), None).unwrap();
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(runtime.did())
            .with_node_acp_owner(runtime.did().to_owned())
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let access =
        crate::ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did()).unwrap());
    let did = crate::store::graphql::escape_graphql_string(runtime.did());
    access
        .write(
            "test.mcp.node",
            &format!(r#"mutation {{ create_Node(input: {{node_did: "{did}"}}) {{_docID}} }}"#),
        )
        .await
        .unwrap();
    (node, runtime)
}

pub(crate) async fn seed_registry_service(
    node: &Arc<defra_node::Node>,
    service_id: &str,
    hostname: &str,
    port: u16,
) {
    use crate::store::config_client::{
        apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    let did = node.node_identity_did().unwrap();
    let access = crate::ConfigAccess::local_as(node.clone(), identity::Did::new(did).unwrap());
    let value = serde_json::json!({"node_did":did,"service_id":service_id,"display_name":service_id,"description":format!("test service {service_id}"),"hostname":hostname,"mcp_port":port,"mcp_path":"/mcp","enabled":true});
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: crate::Collection::ToolServiceRegistry,
        add: value.clone(),
        update: value,
    }])
    .unwrap();
    access
        .transact_write("test.mcp.service", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
        })
        .await
        .unwrap();
}

async fn remote_node() -> (Arc<defra_node::Node>, crate::KeyIdentity) {
    let (node, runtime) = test_node().await;
    seed_registry_service(&node, "shared", "remote", 8080).await;
    (node, runtime)
}

fn catalog() -> rmcp::model::ListToolsResult {
    rmcp::model::ListToolsResult::with_all_items(["selected", "hidden"].into_iter().map(|name| {
        rmcp::model::Tool::new(name, name, Arc::new(serde_json::json!({"type":"object", "properties":{"value":{"type":"integer"}}, "required":["value"]}).as_object().unwrap().clone()))
    }).collect())
}

fn remote_context(
    node: Arc<defra_node::Node>,
    pool: crate::tools::mcp::pool::McpPool,
) -> MetaToolContext {
    let node_did = node.node_identity_did().unwrap().to_owned();
    MetaToolContext {
        configuration: crate::inference::resolved::ResolvedConfiguration::Static,
        actor: identity::Did::new(&node_did).unwrap(),
        node,
        mcp_pool: pool.for_agent(&node_did),
        health: crate::tools::mcp::health::ServiceHealthMap::new(),
        local_hostname: "local".into(),
        local_subnet: None,
        node_did,
        allowed_mcp_service_ids: vec!["shared".into()],
        remote_tools: remote_selection(&["shared"], &["selected"]),
    }
}

#[tokio::test]
async fn service_enablement_at_use_matches_model() {
    let cases = tokio::task::spawn_blocking(|| {
        crate::lean_vocab_test::lean_identity_household_cases()
            .iter()
            .find(|case| case.name == "configuration-explicit-grants")
            .expect("MCP use model")
            .mcp_service_use_cases
            .clone()
    })
    .await
    .unwrap();
    let (node, _runtime) = remote_node().await;
    let access = crate::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(node.node_identity_did().unwrap()).unwrap(),
    );
    let rows = access
        .execute(r#"{ ToolServiceRegistry(filter: { service_id: { _eq: "shared" } }) { _docID } }"#)
        .await
        .unwrap();
    let doc_id = rows["data"]["ToolServiceRegistry"][0]["_docID"]
        .as_str()
        .unwrap();
    let doc_id = crate::store::graphql::escape_graphql_string(doc_id);
    let ctx = remote_context(node.clone(), crate::tools::mcp::pool::McpPool::new());
    for case in cases.iter().filter(|case| {
        [
            "same_actor",
            "same_chain",
            "same_selection",
            "selected",
            "service_visible",
        ]
        .iter()
        .all(|key| case[*key] == true)
    }) {
        let enabled = case["enabled"].as_bool().unwrap();
        access.write("test.mcp.enablement", &format!(r#"mutation {{ update_ToolServiceRegistry(docID: "{doc_id}", input: {{ enabled: {enabled} }}) {{ _docID }} }}"#)).await.unwrap();
        let actual = super::shared::lookup_service(&ctx, "shared").await.is_ok();
        assert_eq!(actual, case["allowed"].as_bool().unwrap(), "{case}");
    }
    let selected_case = cases.iter().find(|case| case["selected"] == false).unwrap();
    let mut unselected = ctx.clone();
    unselected.allowed_mcp_service_ids.clear();
    assert_eq!(
        super::shared::lookup_service(&unselected, "shared")
            .await
            .is_ok(),
        selected_case["allowed"].as_bool().unwrap()
    );
    let actor_case = cases
        .iter()
        .find(|case| case["same_actor"] == false)
        .unwrap();
    let mut wrong_actor = ctx.clone();
    wrong_actor.configuration = crate::inference::resolved::ResolvedConfiguration::Documents(
        crate::inference::resolved::SelectedConfiguration::new(
            node.clone(),
            identity::Did::new(crate::triggers::callback::test_agent_did()).unwrap(),
            vec![],
        ),
    );
    assert_eq!(
        super::shared::lookup_service(&wrong_actor, "shared")
            .await
            .is_ok(),
        actor_case["allowed"].as_bool().unwrap()
    );
    node.shutdown().await.unwrap();
}

#[tokio::test]
async fn endpoint_rebind_during_preflight_does_not_dispatch_to_either_route() {
    let (node, _runtime) = remote_node().await;
    let access = crate::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(node.node_identity_did().unwrap()).unwrap(),
    );
    let rows = access
        .execute(r#"{ ToolServiceRegistry(filter: { service_id: { _eq: "shared" } }) { _docID } }"#)
        .await
        .unwrap();
    let doc_id = rows["data"]["ToolServiceRegistry"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let pool = crate::tools::mcp::pool::McpPool::new_with_tool_handlers(
        move |_, _| {
            let access = access.clone();
            let doc_id = doc_id.clone();
            async move {
                let doc_id = crate::store::graphql::escape_graphql_string(&doc_id);
                access.write("test.mcp.rebind", &format!(r#"mutation {{ update_ToolServiceRegistry(docID: "{doc_id}", input: {{ mcp_port: 8081 }}) {{ _docID }} }}"#)).await?;
                Ok(catalog())
            }
        },
        move |_| {
            let count = count.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(make_call_result(&["unexpected"]))
            }
        },
    );
    let tool = CallToolTool::new(remote_context(node.clone(), pool));
    let error = tool
        .call(CallToolArgs {
            service_id: "shared".into(),
            tool_name: "selected".into(),
            arguments: serde_json::json!({"value": 1}),
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("route changed"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    node.shutdown().await.unwrap();
}

#[tokio::test]
async fn scoped_discovery_and_flat_dispatch_have_identical_permissions() {
    let (node, _runtime) = remote_node().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let pool = crate::tools::mcp::pool::McpPool::new_with_tool_handlers(
        |_, endpoint| async move {
            assert_eq!(endpoint, "http://remote:8080/mcp");
            Ok(catalog())
        },
        move |params| {
            let count = count.clone();
            async move {
                assert_eq!(params.name, "selected");
                count.fetch_add(1, Ordering::SeqCst);
                Ok(make_call_result(&["executed"]))
            }
        },
    );
    let ctx = remote_context(node.clone(), pool.clone());
    let discovered = super::DiscoverToolsTool::new(ctx.clone())
        .call(super::discover::DiscoverToolsArgs { query: None })
        .await
        .unwrap();
    assert!(discovered.contains("selected"));
    assert!(!discovered.contains("hidden"));
    for style in [RemoteToolStyle::Discovery, RemoteToolStyle::Flat] {
        let mut context = ctx.clone();
        context.remote_tools.services[0].style = style;
        let dispatcher = CallToolTool::new(context.clone());
        let denial = dispatcher
            .call(CallToolArgs {
                service_id: "shared".into(),
                tool_name: "hidden".into(),
                arguments: serde_json::json!({"value":1}),
            })
            .await
            .unwrap_err();
        assert!(denial.to_string().contains("tool_not_allowed"));
        let admission = Tool::admit(
            &dispatcher,
            &CallToolArgs {
                service_id: "shared".into(),
                tool_name: "hidden".into(),
                arguments: serde_json::json!({"value":1}),
            },
        )
        .unwrap_err();
        assert!(admission.to_string().contains("tool_not_allowed"));
        let flat = super::flat::FlatRemoteTool {
            definition: crate::inference::llm::tool::ToolDefinition {
                name: "test".into(),
                description: String::new(),
                parameters: serde_json::json!({}),
            },
            service_id: "shared".into(),
            tool_name: "hidden".into(),
            context: context.clone(),
        };
        let denial = crate::inference::llm::tool::ToolDyn::call(&flat, "{}".into())
            .await
            .unwrap_err();
        assert!(matches!(
            denial,
            crate::inference::llm::tool::ToolError::ReportedFailure {
                class: crate::tools::call::lifecycle::FailureClass::PolicyDenied,
                ..
            }
        ));
        let admission = crate::inference::llm::tool::ToolDyn::admit(&flat, "{}").unwrap_err();
        assert!(matches!(
            admission,
            crate::inference::llm::tool::ToolError::ReportedFailure {
                class: crate::tools::call::lifecycle::FailureClass::PolicyDenied,
                ..
            }
        ));
        let built = super::build_meta_tools(
            node.clone(),
            pool.clone(),
            context.health,
            "local".into(),
            None,
            context.node_did,
            context.actor,
            context.allowed_mcp_service_ids,
            context.remote_tools,
            crate::inference::resolved::ResolvedConfiguration::Static,
        )
        .await
        .unwrap();
        match style {
            RemoteToolStyle::Discovery => {
                assert_eq!(built.len(), 3);
                assert_eq!(
                    dispatcher
                        .call(CallToolArgs {
                            service_id: "shared".into(),
                            tool_name: "selected".into(),
                            arguments: serde_json::json!({"value":1})
                        })
                        .await
                        .unwrap(),
                    "executed"
                );
            }
            RemoteToolStyle::Flat => {
                assert_eq!(built.len(), 1);
                assert_eq!(built[0].name(), super::flat_tool_name("shared", "selected"));
                assert_eq!(
                    built[0].definition(String::new()).await.parameters["required"],
                    serde_json::json!(["value"])
                );
                assert_eq!(
                    built[0].call("{\"value\":1}".into()).await.unwrap(),
                    "executed"
                );
            }
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn disabled_and_duplicate_registry_identity_fail_before_dispatch() {
    let (node, _runtime) = remote_node().await;
    let context = remote_context(node.clone(), crate::tools::mcp::pool::McpPool::new());
    let access = crate::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(node.node_identity_did().unwrap()).unwrap(),
    );
    access.write("test.mcp.disable", r#"mutation { update_ToolServiceRegistry(filter: { service_id: {_eq: "shared"}}, input: {enabled: false}) {_docID} }"#).await.unwrap();
    assert!(super::shared::lookup_service(&context, "shared")
        .await
        .is_err());
    let did = crate::store::graphql::escape_graphql_string(node.node_identity_did().unwrap());
    let result = access.write("test.mcp.duplicate", &format!(r#"mutation {{ create_ToolServiceRegistry(input: {{node_did: "{did}", service_id: "shared", hostname: "other", mcp_port: 9000}}) {{_docID}} }}"#)).await;
    assert!(
        result.is_err(),
        "the canonical unique owner/service identity must reject duplicate rows"
    );
}

#[tokio::test(start_paused = true)]
async fn configured_discovery_timeout_cannot_fall_through_to_mutating_dispatch() {
    let (node, _runtime) = remote_node().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let pool = crate::tools::mcp::pool::McpPool::new_with_tool_handlers(
        |_, _| async {
            std::future::pending::<()>().await;
            Ok(catalog())
        },
        move |_| {
            let count = count.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(make_call_result(&["unexpected"]))
            }
        },
    );
    let mut context = remote_context(node, pool);
    context.remote_tools.services[0].discovery_timeout_secs = Some(2);
    let started = tokio::time::Instant::now();
    let error = CallToolTool::new(context)
        .call(CallToolArgs {
            service_id: "shared".into(),
            tool_name: "selected".into(),
            arguments: serde_json::json!({"value":1}),
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("service_unavailable"));
    assert!(started.elapsed() <= std::time::Duration::from_secs(2));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn stale_limit_intersects_normal_call_timeout() {
    let (node, _runtime) = remote_node().await;
    for (normal, stale, expected) in [(3, 10, 3), (10, 2, 2)] {
        let pool = crate::tools::mcp::pool::McpPool::new_with_tool_handlers(
            |_, _| async { Ok(catalog()) },
            |_| async {
                std::future::pending::<()>().await;
                Ok(make_call_result(&["unexpected"]))
            },
        );
        let mut context = remote_context(node.clone(), pool);
        context.remote_tools.services[0].timeout_secs = Some(normal);
        context.remote_tools.services[0].stale_timeout_secs = Some(stale);
        context
            .health
            .set_for_test(
                "shared",
                crate::tools::mcp::health::ServiceHealth {
                    status: crate::tools::mcp::health::HealthStatus::Stale,
                    last_seen: chrono::Utc::now(),
                    last_error: None,
                },
            )
            .await;
        let start = tokio::time::Instant::now();
        let result = CallToolTool::new(context)
            .call(CallToolArgs {
                service_id: "shared".into(),
                tool_name: "selected".into(),
                arguments: serde_json::json!({"value":1}),
            })
            .await;
        assert!(result.unwrap_err().to_string().contains("timed out"));
        assert_eq!(start.elapsed(), std::time::Duration::from_secs(expected));
    }
}
