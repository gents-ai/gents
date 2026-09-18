//! Read-only native-JSON graph proposal preview.
//!
//! Authoritative `StageCapability` values currently come only from installed
//! graph packs. This adapter therefore compiles explicitly proposed capability
//! shapes for syntax and topology feedback, but never represents them as
//! configured authority or sends the resulting plan to publication.

use serde::{Deserialize, Serialize};

use crate::graph_pipeline::{
    compile_graph, graph_plan_creation_set, CompilerPolicy, Diagnostic, GraphIntent, GraphPlan,
    PlannedGraphDocument, StageCapability,
};
use crate::llm::tool::{Tool, ToolDefinition};

use super::{SelfConfigCore, SelfConfigError, PREVIEW_GRAPH_TOOL_NAME};

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewGraphParams {
    pub intent: GraphIntent,
    /// Proposal-only capability shapes. These are not loaded from the
    /// authoritative pack catalog and confer no execution authority.
    pub proposed_capabilities: Vec<StageCapability>,
}

#[derive(Clone, Debug, Serialize)]
pub struct GraphPreviewAuthority {
    pub caller_did: String,
    pub preview_tool_granted: bool,
    pub capability_source: &'static str,
    pub acp_verified: bool,
    pub tasks_verified: bool,
    pub tools_and_outputs_verified: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct PreviewGraphResponse {
    pub committed: bool,
    pub syntax_and_topology_valid: bool,
    pub publishable: bool,
    pub authority: GraphPreviewAuthority,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<GraphPlan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub creation_set: Vec<PlannedGraphDocument>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<Diagnostic>,
    pub limitation: &'static str,
}

pub struct PreviewGraphTool {
    pub(super) core: SelfConfigCore,
}

impl Tool for PreviewGraphTool {
    const NAME: &'static str = PREVIEW_GRAPH_TOOL_NAME;
    type Error = SelfConfigError;
    type Args = PreviewGraphParams;
    type Output = PreviewGraphResponse;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_owned(),
            description: "Validate a native-JSON graph intent against proposal-only capability shapes and return a stable compiler digest and prospective publication document identities. This read-only preview never reads or writes configuration, publishes or activates a revision, starts execution, or verifies authoritative pack capabilities, Tasks, ACP, tools, or output authority.".to_owned(),
            parameters: schemars::schema_for!(PreviewGraphParams).to_value(),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let caller_did = self.core.agent_did();
        let authority = GraphPreviewAuthority {
            caller_did: caller_did.to_owned(),
            preview_tool_granted: true,
            capability_source: "proposal_only",
            acp_verified: false,
            tasks_verified: false,
            tools_and_outputs_verified: false,
        };
        let limitation = "Syntax/topology and proposed caller admission only. Authoritative StageCapability values remain pack-owned; publication must separately resolve principal-owned Tasks, ACP, tools, outputs, and the matching digest through the transaction owner.";
        if args.intent.agent_did != caller_did {
            return Ok(PreviewGraphResponse {
                committed: false,
                syntax_and_topology_valid: false,
                publishable: false,
                authority,
                digest: None,
                plan: None,
                creation_set: Vec::new(),
                diagnostics: vec![Diagnostic {
                    code: crate::graph_pipeline::DiagnosticCode::UnauthorizedCapability,
                    path: "/agent_did".to_owned(),
                    message: format!(
                        "graph proposal owner {:?} does not match invoking principal {:?}",
                        args.intent.agent_did, caller_did
                    ),
                }],
                limitation,
            });
        }
        match compile_graph(
            &args.intent,
            &args.proposed_capabilities,
            caller_did,
            &CompilerPolicy::default(),
        ) {
            Ok(plan) => {
                let creation_set = graph_plan_creation_set(caller_did, &plan)?;
                let digest = plan.digest.clone();
                Ok(PreviewGraphResponse {
                    committed: false,
                    syntax_and_topology_valid: true,
                    publishable: false,
                    authority,
                    digest: Some(digest),
                    plan: Some(plan),
                    creation_set,
                    diagnostics: Vec::new(),
                    limitation,
                })
            }
            Err(error) => Ok(PreviewGraphResponse {
                committed: false,
                syntax_and_topology_valid: false,
                publishable: false,
                authority,
                digest: None,
                plan: None,
                creation_set: Vec::new(),
                diagnostics: error.diagnostics,
                limitation,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::{json, Value};

    use super::*;
    use crate::graph_pipeline::{
        EntryBinding, GraphLimits, GraphNode, PortCardinality, PortRef, PortSpec,
        ResultCardinality, ResultContract,
    };
    use crate::llm::tool::{ToolDyn, ToolError, UnparseableArgsKind};
    use crate::tool_surface::SelfConfigToolConfig;

    const OWNER: &str = "did:key:preview-owner";

    fn capability_for(owner: &str) -> StageCapability {
        StageCapability {
            agent_did: owner.to_owned(),
            capability_id: "score".to_owned(),
            revision: "v1".to_owned(),
            task_id: "existing-score-task".to_owned(),
            input_ports: vec![PortSpec {
                name: "input".to_owned(),
                collection: "SessionInput".to_owned(),
                schema: "SessionInput/v1".to_owned(),
                correlation_field: "run_id".to_owned(),
                cardinality: PortCardinality::One,
                required: true,
            }],
            output_ports: vec![PortSpec {
                name: "score".to_owned(),
                collection: "SessionScore".to_owned(),
                schema: "SessionScore/v1".to_owned(),
                correlation_field: "run_id".to_owned(),
                cardinality: PortCardinality::One,
                required: false,
            }],
            allowed_callers: vec![owner.to_owned()],
            workspace_authority: None,
            tags: vec![],
        }
    }

    fn capability() -> StageCapability {
        capability_for(OWNER)
    }

    fn intent_for(owner: &str, capability_id: &str) -> GraphIntent {
        GraphIntent {
            agent_did: owner.to_owned(),
            graph_id: "session-evaluation".to_owned(),
            nodes: vec![GraphNode {
                node_id: "score".to_owned(),
                capability_id: capability_id.to_owned(),
                capability_revision: "v1".to_owned(),
            }],
            edges: vec![],
            entries: vec![EntryBinding {
                name: "session".to_owned(),
                collection: "SessionInput".to_owned(),
                schema: "SessionInput/v1".to_owned(),
                input_contract: None,
                to: PortRef {
                    node_id: "score".to_owned(),
                    port: "input".to_owned(),
                },
            }],
            results: vec![ResultContract {
                name: "score".to_owned(),
                from: PortRef {
                    node_id: "score".to_owned(),
                    port: "score".to_owned(),
                },
                cardinality: ResultCardinality::Exactly { count: 1 },
                terminal: true,
            }],
            limits: GraphLimits {
                max_nodes: 2,
                max_edges: 2,
                max_depth: 2,
                max_fan_out: 2,
                max_total_invocations: 2,
                max_runtime_secs: 60,
            },
            tags: vec![],
        }
    }

    fn intent(capability_id: &str) -> GraphIntent {
        intent_for(OWNER, capability_id)
    }

    async fn node() -> Arc<defra_node::EmbeddedNode> {
        let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        node
    }

    fn tool_for(node: Arc<defra_node::EmbeddedNode>, owner: &str) -> PreviewGraphTool {
        PreviewGraphTool {
            core: SelfConfigCore::new(node, owner.to_owned(), "working".to_owned()).unwrap(),
        }
    }

    fn tool(node: Arc<defra_node::EmbeddedNode>) -> PreviewGraphTool {
        tool_for(node, OWNER)
    }

    async fn persisted_graph_documents(node: &defra_node::EmbeddedNode) -> Value {
        node.execute(
            "{ GraphDefinition {_docID} GraphRevision {_docID} EventSource {_docID} Trigger {_docID} }",
        )
        .await
        .data
        .unwrap()
    }

    #[tokio::test]
    async fn valid_preview_is_stable_and_writes_nothing() {
        let node = node().await;
        let tool = tool(node.clone());
        let before = persisted_graph_documents(&node).await;
        let args = PreviewGraphParams {
            intent: intent("score"),
            proposed_capabilities: vec![capability()],
        };
        let first = Tool::call(&tool, args.clone()).await.unwrap();
        let second = Tool::call(&tool, args).await.unwrap();
        assert_eq!(
            serde_json::to_value(&first).unwrap(),
            serde_json::to_value(&second).unwrap()
        );
        assert!(first.syntax_and_topology_valid);
        assert!(!first.committed);
        assert!(!first.publishable);
        assert_eq!(first.authority.capability_source, "proposal_only");
        assert!(!first.authority.acp_verified);
        assert!(!first.authority.tasks_verified);
        assert!(!first.authority.tools_and_outputs_verified);
        assert!(first
            .digest
            .as_deref()
            .is_some_and(|digest| digest.starts_with("sha256:")));
        assert_eq!(first.creation_set.len(), 4);
        assert!(first
            .creation_set
            .iter()
            .all(|document| document.owner_did == OWNER));
        assert_eq!(
            first
                .creation_set
                .iter()
                .map(|document| document.collection.as_str())
                .collect::<Vec<_>>(),
            ["EventSource", "GraphDefinition", "GraphRevision", "Trigger"]
        );
        assert_eq!(before, persisted_graph_documents(&node).await);
        node.shutdown().await;
    }

    #[tokio::test]
    async fn same_logical_graph_is_bound_to_each_preview_principal() {
        let node = node().await;
        let other = "did:key:preview-other";
        let before = persisted_graph_documents(&node).await;
        let first = Tool::call(
            &tool_for(node.clone(), OWNER),
            PreviewGraphParams {
                intent: intent_for(OWNER, "score"),
                proposed_capabilities: vec![capability_for(OWNER)],
            },
        )
        .await
        .unwrap();
        let second = Tool::call(
            &tool_for(node.clone(), other),
            PreviewGraphParams {
                intent: intent_for(other, "score"),
                proposed_capabilities: vec![capability_for(other)],
            },
        )
        .await
        .unwrap();
        assert_eq!(first.digest, second.digest);
        assert_eq!(
            first
                .creation_set
                .iter()
                .map(|document| (&document.collection, &document.logical_id))
                .collect::<Vec<_>>(),
            second
                .creation_set
                .iter()
                .map(|document| (&document.collection, &document.logical_id))
                .collect::<Vec<_>>()
        );
        assert!(first
            .creation_set
            .iter()
            .all(|document| document.owner_did == OWNER));
        assert!(second
            .creation_set
            .iter()
            .all(|document| document.owner_did == other));
        assert_eq!(before, persisted_graph_documents(&node).await);
        node.shutdown().await;
    }

    fn assert_denied(response: &PreviewGraphResponse) {
        assert!(!response.syntax_and_topology_valid);
        assert!(!response.committed);
        assert!(!response.publishable);
        assert!(response.digest.is_none());
        assert!(response.plan.is_none());
        assert!(response.creation_set.is_empty());
        assert!(response.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == crate::graph_pipeline::DiagnosticCode::UnauthorizedCapability
        }));
    }

    #[tokio::test]
    async fn foreign_intent_owner_is_denied_without_writes() {
        let node = node().await;
        let before = persisted_graph_documents(&node).await;
        let response = Tool::call(
            &tool(node.clone()),
            PreviewGraphParams {
                intent: intent_for("did:key:foreign", "score"),
                proposed_capabilities: vec![capability()],
            },
        )
        .await
        .unwrap();
        assert_denied(&response);
        assert_eq!(before, persisted_graph_documents(&node).await);
        node.shutdown().await;
    }

    #[tokio::test]
    async fn proposed_capability_caller_denial_returns_no_plan_or_writes() {
        let node = node().await;
        let before = persisted_graph_documents(&node).await;
        let mut denied = capability();
        denied.allowed_callers = vec!["did:key:foreign".to_owned()];
        let response = Tool::call(
            &tool(node.clone()),
            PreviewGraphParams {
                intent: intent("score"),
                proposed_capabilities: vec![denied],
            },
        )
        .await
        .unwrap();
        assert_denied(&response);
        assert_eq!(before, persisted_graph_documents(&node).await);
        node.shutdown().await;
    }

    #[tokio::test]
    async fn preview_creation_set_matches_materialization_owned_identities() {
        let node = node().await;
        let before = persisted_graph_documents(&node).await;
        let response = Tool::call(
            &tool(node.clone()),
            PreviewGraphParams {
                intent: intent("score"),
                proposed_capabilities: vec![capability()],
            },
        )
        .await
        .unwrap();
        assert_eq!(before, persisted_graph_documents(&node).await);

        node.add_schema(
            "type SessionInput { run_id: String @index(unique: true) payload: String }",
        )
        .await
        .unwrap();
        node.add_schema("type SessionScore { run_id: String @index score: Int }")
            .await
            .unwrap();
        crate::graph_pipeline::install_graph_test_tasks(
            &node,
            OWNER,
            "working",
            &["existing-score-task"],
        )
        .await;
        let plan = response.plan.as_ref().unwrap();
        let materialized =
            crate::graph_pipeline::materialize_graph_revision(&node, None, OWNER, plan)
                .await
                .unwrap();
        let expected_trigger_ids = response
            .creation_set
            .iter()
            .filter(|document| document.collection == "Trigger")
            .map(|document| document.logical_id.clone())
            .collect::<Vec<_>>();
        assert_eq!(materialized.trigger_ids, expected_trigger_ids);

        let escaped_owner = crate::graphql::escape_graphql_string(OWNER);
        let escaped_digest = crate::graphql::escape_graphql_string(&plan.digest);
        let stored = node
            .execute(&format!(
                r#"{{
                    GraphDefinition(filter: {{agent_did: {{_eq: "{escaped_owner}"}}}}) {{graph_id agent_did}}
                    GraphRevision(filter: {{digest: {{_eq: "{escaped_digest}"}}}}) {{revision_id owner_did}}
                    EventSource(filter: {{agent_did: {{_eq: "{escaped_owner}"}}}}) {{event_source_id agent_did}}
                    Trigger(filter: {{agent_did: {{_eq: "{escaped_owner}"}}}}) {{trigger_id agent_did}}
                }}"#
            ))
            .await;
        assert!(!stored.has_errors(), "{:?}", stored.errors);
        let stored = stored.data.unwrap();
        for document in &response.creation_set {
            let (field, owner_field) = match document.collection.as_str() {
                "GraphDefinition" => ("graph_id", "agent_did"),
                "GraphRevision" => ("revision_id", "owner_did"),
                "EventSource" => ("event_source_id", "agent_did"),
                "Trigger" => ("trigger_id", "agent_did"),
                other => panic!("unexpected preview collection {other}"),
            };
            assert!(stored[&document.collection]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| {
                    row[field] == document.logical_id && row[owner_field] == document.owner_did
                }));
        }
        node.shutdown().await;
    }

    #[tokio::test]
    async fn malformed_and_unknown_native_json_are_rejected_before_preview() {
        let node = node().await;
        let tool = tool(node.clone());
        let malformed = ToolDyn::call(&tool, r#"{"intent":"#.to_owned())
            .await
            .unwrap_err();
        assert!(matches!(
            malformed,
            ToolError::UnparseableArgs {
                kind: UnparseableArgsKind::Truncated,
                ..
            }
        ));

        let unknown = ToolDyn::call(
            &tool,
            json!({
                "intent": intent("score"),
                "proposed_capabilities": [capability()],
                "publish": true
            })
            .to_string(),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            unknown,
            ToolError::UnparseableArgs {
                kind: UnparseableArgsKind::UnknownField,
                ..
            }
        ));
        node.shutdown().await;
    }

    #[tokio::test]
    async fn compiler_diagnostics_are_returned_without_a_digest_or_creation_set() {
        let node = node().await;
        let response = Tool::call(
            &tool(node.clone()),
            PreviewGraphParams {
                intent: intent("unknown"),
                proposed_capabilities: vec![capability()],
            },
        )
        .await
        .unwrap();
        assert!(!response.syntax_and_topology_valid);
        assert!(response.digest.is_none());
        assert!(response.plan.is_none());
        assert!(response.creation_set.is_empty());
        assert!(response.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == crate::graph_pipeline::DiagnosticCode::UnknownCapability
        }));
        node.shutdown().await;
    }

    #[tokio::test]
    async fn preview_tool_is_omitted_when_the_graph_grant_is_off() {
        let node = node().await;
        let mut config = SelfConfigToolConfig {
            behavior_id: "working".to_owned(),
            ..Default::default()
        };
        assert!(!super::super::build_self_config_tools(
            node.clone(),
            OWNER.to_owned(),
            None,
            &config,
        )
        .iter()
        .any(|tool| tool.name() == PREVIEW_GRAPH_TOOL_NAME));

        config.enable_graph_tools = true;
        assert!(super::super::build_self_config_tools(
            node.clone(),
            OWNER.to_owned(),
            None,
            &config,
        )
        .iter()
        .any(|tool| tool.name() == PREVIEW_GRAPH_TOOL_NAME));
        node.shutdown().await;
    }
}
