use std::sync::Arc;

use defra_node::EmbeddedNode;
use identity::Did;
use serde::{Deserialize, Serialize};

use crate::llm::tool::{Tool, ToolDefinition};

use super::{
    compile_graph, publish_graph_plan, CompilerPolicy, Diagnostic, GraphIntent, PublishedGraph,
    StageCapability,
};

pub const COMPILE_GRAPH_TOOL_NAME: &str = "compile_graph";
pub const GRAPH_PIPELINE_TOOL_NAMES: [&str; 1] = [COMPILE_GRAPH_TOOL_NAME];

#[derive(Debug)]
pub struct GraphPipelineToolError(anyhow::Error);

impl std::fmt::Display for GraphPipelineToolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:#}", self.0)
    }
}

impl std::error::Error for GraphPipelineToolError {}

impl From<anyhow::Error> for GraphPipelineToolError {
    fn from(error: anyhow::Error) -> Self {
        Self(error)
    }
}

#[derive(Clone)]
pub struct CompileGraphTool {
    node: Arc<EmbeddedNode>,
    identity: Did,
    capabilities: Arc<Vec<StageCapability>>,
    policy: CompilerPolicy,
}

impl CompileGraphTool {
    pub fn new(
        node: Arc<EmbeddedNode>,
        identity: Did,
        capabilities: Vec<StageCapability>,
        policy: CompilerPolicy,
    ) -> Self {
        Self {
            node,
            identity,
            capabilities: Arc::new(capabilities),
            policy,
        }
    }
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompileGraphArgs {
    pub intent: GraphIntent,
}

#[derive(Clone, Debug, Serialize)]
pub struct CompileGraphResponse {
    pub accepted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published: Option<PublishedGraph>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<Diagnostic>,
}

impl Tool for CompileGraphTool {
    const NAME: &'static str = COMPILE_GRAPH_TOOL_NAME;
    type Error = GraphPipelineToolError;
    type Args = CompileGraphArgs;
    type Output = CompileGraphResponse;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_owned(),
            description: "Compile a bounded document DAG from operator-approved existing Task capabilities and publish its EventTriggers atomically. The model cannot author prompts, behaviors, tools, models, executable plans, or arbitrary collections. Invalid graphs return stable repair diagnostics without writes. Execution starts separately through existing bounded document-write tools after normal runtime reconciliation. Configure this tool in approval_required_tools when publication needs human approval.".to_owned(),
            parameters: schemars::schema_for!(CompileGraphArgs).to_value(),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let caller_did = self.identity.to_string();
        let plan = match compile_graph(
            &args.intent,
            self.capabilities.as_slice(),
            &caller_did,
            &self.policy,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                return Ok(CompileGraphResponse {
                    accepted: false,
                    digest: None,
                    published: None,
                    diagnostics: error.diagnostics,
                });
            }
        };
        let digest = plan.digest.clone();
        let published =
            publish_graph_plan(self.node.as_ref(), self.identity.clone(), &plan).await?;
        Ok(CompileGraphResponse {
            accepted: true,
            digest: Some(digest),
            published: Some(published),
            diagnostics: vec![],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_pipeline::{
        EntryBinding, GraphLimits, GraphNode, PortCardinality, PortRef, PortSpec,
    };

    #[derive(Deserialize)]
    struct EvalCase {
        name: String,
        intent: GraphIntent,
        expected_accepted: bool,
        expected_diagnostic_codes: Vec<String>,
    }

    fn capability() -> StageCapability {
        StageCapability {
            capability_id: "worker".to_owned(),
            revision: "v1".to_owned(),
            task_id: "existing-worker-task".to_owned(),
            input_ports: vec![
                PortSpec {
                    name: "input".to_owned(),
                    collection: "PipelineInput".to_owned(),
                    schema: "PipelineInput/v1".to_owned(),
                    correlation_field: "run_id".to_owned(),
                    cardinality: PortCardinality::One,
                    required: true,
                },
                PortSpec {
                    name: "feedback".to_owned(),
                    collection: "PipelineFeedback".to_owned(),
                    schema: "PipelineFeedback/v1".to_owned(),
                    correlation_field: "run_id".to_owned(),
                    cardinality: PortCardinality::One,
                    required: false,
                },
            ],
            output_ports: vec![PortSpec {
                name: "result".to_owned(),
                collection: "PipelineIntermediate".to_owned(),
                schema: "PipelineIntermediate/v1".to_owned(),
                correlation_field: "run_id".to_owned(),
                cardinality: PortCardinality::One,
                required: false,
            }],
            allowed_callers: vec!["did:key:owner".to_owned()],
        }
    }

    fn review_capability() -> StageCapability {
        StageCapability {
            capability_id: "reviewer".to_owned(),
            revision: "v1".to_owned(),
            task_id: "existing-reviewer-task".to_owned(),
            input_ports: vec![PortSpec {
                name: "input".to_owned(),
                collection: "PipelineIntermediate".to_owned(),
                schema: "PipelineIntermediate/v1".to_owned(),
                correlation_field: "run_id".to_owned(),
                cardinality: PortCardinality::One,
                required: true,
            }],
            output_ports: vec![PortSpec {
                name: "feedback".to_owned(),
                collection: "PipelineFeedback".to_owned(),
                schema: "PipelineFeedback/v1".to_owned(),
                correlation_field: "run_id".to_owned(),
                cardinality: PortCardinality::One,
                required: false,
            }],
            allowed_callers: vec!["did:key:owner".to_owned()],
        }
    }

    fn capabilities() -> Vec<StageCapability> {
        vec![capability(), review_capability()]
    }

    fn intent(capability_id: &str) -> GraphIntent {
        GraphIntent {
            graph_id: "model-pipeline".to_owned(),
            nodes: vec![GraphNode {
                node_id: "worker".to_owned(),
                capability_id: capability_id.to_owned(),
                capability_revision: "v1".to_owned(),
            }],
            edges: vec![],
            entries: vec![EntryBinding {
                name: "input".to_owned(),
                collection: "PipelineInput".to_owned(),
                schema: "PipelineInput/v1".to_owned(),
                input_contract: None,
                to: PortRef {
                    node_id: "worker".to_owned(),
                    port: "input".to_owned(),
                },
            }],
            results: vec![],
            limits: GraphLimits {
                max_nodes: 2,
                max_edges: 2,
                max_depth: 2,
                max_fan_out: 2,
                max_total_invocations: 2,
                max_runtime_secs: 60,
            },
        }
    }

    async fn tool() -> CompileGraphTool {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        for schema in [
            gents_protocol::schemas::GRAPH_DEFINITION,
            gents_protocol::schemas::GRAPH_REVISION,
            gents_protocol::schemas::GRAPH_RUN,
            gents_protocol::schemas::AGENT_REQUEST,
            gents_protocol::schemas::EVENT_TRIGGER_GROUP_STATE,
            gents_protocol::schemas::TASK,
            gents_protocol::schemas::EVENT_TRIGGER,
        ] {
            node.add_schema(schema).await.unwrap();
        }
        node.execute(
            r#"mutation { create_Task(input: {
                task_id: "existing-worker-task",
                behavior_id: "worker",
                prompt_template: "operator approved prompt",
                enabled: true
            }) { _docID } }"#,
        )
        .await;
        node.execute(
            r#"mutation { create_Task(input: {
                task_id: "existing-reviewer-task",
                behavior_id: "reviewer",
                prompt_template: "operator approved review prompt",
                enabled: true
            }) { _docID } }"#,
        )
        .await;
        CompileGraphTool::new(
            node,
            Did::new("did:key:owner".to_owned()).unwrap(),
            capabilities(),
            CompilerPolicy::default(),
        )
    }

    #[tokio::test]
    async fn invalid_graph_returns_repairable_diagnostics_without_writes() {
        let tool = tool().await;
        let response = tool
            .call(CompileGraphArgs {
                intent: intent("unapproved"),
            })
            .await
            .unwrap();
        assert!(!response.accepted);
        assert!(response.published.is_none());
        assert!(!response.diagnostics.is_empty());
        tool.node.shutdown().await;
    }

    #[tokio::test]
    async fn valid_graph_recompiles_and_publishes_existing_task_routes() {
        let tool = tool().await;
        let response = tool
            .call(CompileGraphArgs {
                intent: intent("worker"),
            })
            .await
            .unwrap();
        assert!(response.accepted);
        assert_eq!(response.published.unwrap().trigger_ids.len(), 1);
        tool.node.shutdown().await;
    }

    #[tokio::test]
    async fn checked_in_evaluation_cases_match_compiler_results() {
        let cases: Vec<EvalCase> = serde_json::from_str(include_str!(
            "../../../../demo/graph-pipeline/eval_cases.json"
        ))
        .unwrap();
        let caller = "did:key:owner";
        for test_case in cases {
            let result = compile_graph(
                &test_case.intent,
                &capabilities(),
                caller,
                &CompilerPolicy::default(),
            );
            assert_eq!(
                result.is_ok(),
                test_case.expected_accepted,
                "{}",
                test_case.name
            );
            let actual_codes = result
                .err()
                .into_iter()
                .flat_map(|error| error.diagnostics)
                .map(|diagnostic| {
                    serde_json::to_value(&diagnostic.code)
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .to_owned()
                })
                .collect::<std::collections::BTreeSet<_>>();
            for expected in test_case.expected_diagnostic_codes {
                assert!(
                    actual_codes.contains(&expected),
                    "{} missing {expected:?}; actual: {actual_codes:?}",
                    test_case.name
                );
            }
        }
    }
}
