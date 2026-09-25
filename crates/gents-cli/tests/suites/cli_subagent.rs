use crate::support::*;

use std::fs;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subagent_list_shows_two_level_dispatch_lineage() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-subagent-list-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;

    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            "cli-subagent-list",
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let runtime_agent_did = agent_did_from_init(&init)?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &runtime_agent_did, Duration::from_secs(30)).await?;

    let test_agent_did = format!("did:key:zSubagentList{}", Uuid::new_v4().simple());
    let root_request_id = format!("root-{}", Uuid::new_v4().simple());
    let first_child_request_id = format!("child-a-{}", Uuid::new_v4().simple());
    let second_child_request_id = format!("child-b-{}", Uuid::new_v4().simple());
    let grandchild_request_id = format!("grandchild-{}", Uuid::new_v4().simple());

    let root_doc_id = seed_request(
        &graphql,
        &test_agent_did,
        &root_request_id,
        "parent-behavior",
        None,
        0,
        "2026-05-20T12:00:00Z",
    )
    .await?;
    let (first_tool_call_id, first_bridge_doc_id) = seed_spawn_bridge(
        &graphql,
        &test_agent_did,
        &root_request_id,
        &root_doc_id,
        &first_child_request_id,
        1,
        "2026-05-20T12:00:01Z",
        "pending",
    )
    .await?;
    let first_child_doc_id = seed_request(
        &graphql,
        &test_agent_did,
        &first_child_request_id,
        "first-child-behavior",
        Some(&ParentLink {
            parent_request_id: &root_request_id,
            parent_doc_id: &root_doc_id,
            tool_call_id: &first_tool_call_id,
            tool_call_doc_id: &first_bridge_doc_id,
        }),
        1,
        "2026-05-20T12:00:01Z",
    )
    .await?;
    let (second_tool_call_id, second_bridge_doc_id) = seed_spawn_bridge(
        &graphql,
        &test_agent_did,
        &root_request_id,
        &root_doc_id,
        &second_child_request_id,
        2,
        "2026-05-20T12:00:02Z",
        "pending",
    )
    .await?;
    seed_request(
        &graphql,
        &test_agent_did,
        &second_child_request_id,
        "second-child-behavior",
        Some(&ParentLink {
            parent_request_id: &root_request_id,
            parent_doc_id: &root_doc_id,
            tool_call_id: &second_tool_call_id,
            tool_call_doc_id: &second_bridge_doc_id,
        }),
        1,
        "2026-05-20T12:00:02Z",
    )
    .await?;
    let (grandchild_tool_call_id, grandchild_bridge_doc_id) = seed_spawn_bridge(
        &graphql,
        &test_agent_did,
        &first_child_request_id,
        &first_child_doc_id,
        &grandchild_request_id,
        1,
        "2026-05-20T12:00:03Z",
        "pending",
    )
    .await?;
    seed_request(
        &graphql,
        &test_agent_did,
        &grandchild_request_id,
        "grandchild-behavior",
        Some(&ParentLink {
            parent_request_id: &first_child_request_id,
            parent_doc_id: &first_child_doc_id,
            tool_call_id: &grandchild_tool_call_id,
            tool_call_doc_id: &grandchild_bridge_doc_id,
        }),
        2,
        "2026-05-20T12:00:03Z",
    )
    .await?;

    let output = run_cli_json(
        &home_dir,
        &[
            "subagent",
            "list",
            "--graphql",
            &graphql,
            "--root",
            &root_request_id,
            "--output",
            "json",
        ],
    )?;
    let rows = output
        .get("rows")
        .and_then(Value::as_array)
        .context("subagent list JSON output missing rows array")?;
    assert_eq!(
        rows.len(),
        4,
        "expected root, two children, and grandchild: {output}"
    );
    assert_lineage_row(rows, &root_request_id, None, 0, "parent-behavior")?;
    assert_lineage_row(
        rows,
        &first_child_request_id,
        Some(&root_request_id),
        1,
        "first-child-behavior",
    )?;
    assert_lineage_row(
        rows,
        &second_child_request_id,
        Some(&root_request_id),
        1,
        "second-child-behavior",
    )?;
    assert_lineage_row(
        rows,
        &grandchild_request_id,
        Some(&first_child_request_id),
        2,
        "grandchild-behavior",
    )?;
    assert_tree_shape(
        &output,
        &root_request_id,
        &first_child_request_id,
        &second_child_request_id,
        &grandchild_request_id,
    )?;

    let text = run_cli_text(
        &home_dir,
        &[
            "subagent",
            "list",
            "--graphql",
            &graphql,
            "--root",
            &root_request_id,
        ],
    )?;
    assert!(
        text.contains("CHILD_REQUEST_ID"),
        "missing tree header: {text}"
    );
    assert!(text.contains(&root_request_id), "missing root row: {text}");
    let lines = text.lines().collect::<Vec<_>>();
    let root_idx = line_index_starting_with(&lines, &root_request_id)?;
    let first_child_idx = line_index_starting_with(&lines, &format!("  {first_child_request_id}"))?;
    let grandchild_idx = line_index_starting_with(&lines, &format!("    {grandchild_request_id}"))?;
    let second_child_idx =
        line_index_starting_with(&lines, &format!("  {second_child_request_id}"))?;
    assert!(
        root_idx < first_child_idx
            && first_child_idx < grandchild_idx
            && grandchild_idx < second_child_idx,
        "default tree output must render descendants before later siblings: {text}"
    );

    let table = run_cli_text(
        &home_dir,
        &[
            "subagent",
            "list",
            "--graphql",
            &graphql,
            "--root",
            &root_request_id,
            "--output",
            "table",
        ],
    )?;
    assert!(
        table.contains("PARENT_REQUEST_ID"),
        "missing table header: {table}"
    );
    assert!(
        table
            .lines()
            .any(|line| line.starts_with(&first_child_request_id)),
        "flat table output must include child row without leading indentation: {table}"
    );
    assert!(
        table
            .lines()
            .any(|line| line.starts_with(&second_child_request_id)),
        "flat table output must include sibling row without leading indentation: {table}"
    );
    assert!(
        !table
            .lines()
            .any(|line| line.starts_with(&format!("  {first_child_request_id}"))),
        "flat table output must not indent child rows: {table}"
    );

    let depth_limited = run_cli_json(
        &home_dir,
        &[
            "subagent",
            "list",
            "--graphql",
            &graphql,
            "--root",
            &root_request_id,
            "--depth",
            "1",
            "--output",
            "json",
        ],
    )?;
    let depth_rows = depth_limited
        .get("rows")
        .and_then(Value::as_array)
        .context("depth-limited output missing rows array")?;
    assert_eq!(
        depth_rows.len(),
        3,
        "--depth 1 should include root and direct children only: {depth_limited}"
    );
    assert!(
        !depth_rows.iter().any(|row| {
            row.get("child_request_id").and_then(Value::as_str)
                == Some(grandchild_request_id.as_str())
        }),
        "--depth 1 must exclude grandchild: {depth_limited}"
    );

    Ok(())
}

/// #1783: a non-terminal fan-out carries bridge edge states that are not
/// request lifecycle states. The rooted list must render them, and keep each
/// child's own request lifecycle as a separate field.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subagent_list_renders_live_fan_out_edge_states() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-subagent-fanout-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            "cli-subagent-fanout",
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let runtime_agent_did = agent_did_from_init(&init)?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &runtime_agent_did, Duration::from_secs(30)).await?;

    let agent_did = format!("did:key:zSubagentFanOut{}", Uuid::new_v4().simple());
    let root_request_id = format!("root-{}", Uuid::new_v4().simple());
    let running_child = format!("running-{}", Uuid::new_v4().simple());
    let awaiting_child = format!("awaiting-{}", Uuid::new_v4().simple());
    let unauthorized_child = format!("unauthorized-{}", Uuid::new_v4().simple());

    let root_doc_id = seed_request_in_state(
        &graphql,
        &agent_did,
        &root_request_id,
        "parent-behavior",
        None,
        0,
        "2026-09-25T14:13:33Z",
        "processing",
    )
    .await?;

    let (running_tool_call_id, running_bridge_doc_id) = seed_spawn_bridge(
        &graphql,
        &agent_did,
        &root_request_id,
        &root_doc_id,
        &running_child,
        1,
        "2026-09-25T14:13:34Z",
        "running",
    )
    .await?;
    seed_request_in_state(
        &graphql,
        &agent_did,
        &running_child,
        "running-behavior",
        Some(&ParentLink {
            parent_request_id: &root_request_id,
            parent_doc_id: &root_doc_id,
            tool_call_id: &running_tool_call_id,
            tool_call_doc_id: &running_bridge_doc_id,
        }),
        1,
        "2026-09-25T14:13:34Z",
        "processing",
    )
    .await?;

    // Durable bridge whose child has not materialized yet.
    seed_spawn_bridge(
        &graphql,
        &agent_did,
        &root_request_id,
        &root_doc_id,
        &awaiting_child,
        2,
        "2026-09-25T14:13:35Z",
        "running",
    )
    .await?;

    // A child row exists under the bridged id but does not corroborate the
    // bridge's physical provenance.
    seed_spawn_bridge(
        &graphql,
        &agent_did,
        &root_request_id,
        &root_doc_id,
        &unauthorized_child,
        3,
        "2026-09-25T14:13:36Z",
        "running",
    )
    .await?;
    seed_request_in_state(
        &graphql,
        &agent_did,
        &unauthorized_child,
        "unauthorized-behavior",
        None,
        1,
        "2026-09-25T14:13:36Z",
        "pending",
    )
    .await?;

    let output = run_cli_json(
        &home_dir,
        &[
            "subagent",
            "list",
            "--graphql",
            &graphql,
            "--root",
            &root_request_id,
            "--output",
            "json",
        ],
    )?;
    let rows = output
        .get("rows")
        .and_then(Value::as_array)
        .context("subagent list JSON output missing rows array")?;
    let states = |request_id: &str| -> Result<(Option<String>, Option<String>)> {
        let row = rows
            .iter()
            .find(|row| row.get("child_request_id").and_then(Value::as_str) == Some(request_id))
            .with_context(|| format!("missing row for {request_id}: {output}"))?;
        Ok((
            row.get("edge_state")
                .and_then(Value::as_str)
                .map(str::to_owned),
            row.get("request_lifecycle_state")
                .and_then(Value::as_str)
                .map(str::to_owned),
        ))
    };
    assert_eq!(rows.len(), 4, "root plus three fan-out edges: {output}");
    assert_eq!(
        states(&root_request_id)?,
        (None, Some("processing".to_owned()))
    );
    assert_eq!(
        states(&running_child)?,
        (Some("running".to_owned()), Some("processing".to_owned()))
    );
    assert_eq!(
        states(&awaiting_child)?,
        (
            Some(gents::descendant_graph::AWAITING_CHILD_MATERIALIZATION.to_owned()),
            None
        )
    );
    assert_eq!(
        states(&unauthorized_child)?,
        (
            Some(gents::descendant_graph::PENDING_CHILD_AUTHORIZATION.to_owned()),
            None
        )
    );

    let tree = run_cli_text(
        &home_dir,
        &[
            "subagent",
            "list",
            "--graphql",
            &graphql,
            "--root",
            &root_request_id,
        ],
    )?;
    assert!(
        tree.contains("EDGE_STATE") && tree.contains("REQUEST_STATE"),
        "tree output must name both states: {tree}"
    );
    assert!(tree.contains(&format!("  {awaiting_child}")), "{tree}");

    Ok(())
}

fn line_index_starting_with(lines: &[&str], prefix: &str) -> Result<usize> {
    lines
        .iter()
        .position(|line| line.starts_with(prefix))
        .with_context(|| format!("missing line starting with {prefix:?}: {lines:?}"))
}

fn assert_tree_shape(
    output: &Value,
    root_request_id: &str,
    first_child_request_id: &str,
    second_child_request_id: &str,
    grandchild_request_id: &str,
) -> Result<()> {
    let roots = output
        .get("tree")
        .and_then(Value::as_array)
        .context("subagent list JSON output missing tree array")?;
    assert_eq!(roots.len(), 1, "expected one root tree: {output}");
    assert_tree_node_id(&roots[0], root_request_id)?;

    let root_children = children(&roots[0])?;
    assert_eq!(
        root_children.len(),
        2,
        "root should have two direct children: {output}"
    );
    assert_tree_node_id(&root_children[0], first_child_request_id)?;
    assert_tree_node_id(&root_children[1], second_child_request_id)?;

    let first_child_children = children(&root_children[0])?;
    assert_eq!(
        first_child_children.len(),
        1,
        "first child should have one grandchild: {output}"
    );
    assert_tree_node_id(&first_child_children[0], grandchild_request_id)?;
    assert!(
        children(&root_children[1])?.is_empty(),
        "second child should not receive first child's grandchild: {output}"
    );
    Ok(())
}

fn children(node: &Value) -> Result<&[Value]> {
    node.get("children")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .context("tree node missing children array")
}

fn assert_tree_node_id(node: &Value, request_id: &str) -> Result<()> {
    assert_eq!(
        node.get("child_request_id").and_then(Value::as_str),
        Some(request_id),
        "unexpected tree node id: {node}"
    );
    Ok(())
}

/// Full physical linkage from a child request back to the parent request and
/// the spawn bridge, matching what the runtime stamps on real dispatches. The
/// durable descendant graph fails closed on any missing or contradictory
/// value (`child_corroborates` in `gents::descendant_graph`), so seeded
/// lineage must carry all four references.
struct ParentLink<'a> {
    parent_request_id: &'a str,
    parent_doc_id: &'a str,
    tool_call_id: &'a str,
    tool_call_doc_id: &'a str,
}

async fn seed_request(
    graphql: &str,
    agent_did: &str,
    request_id: &str,
    behavior_id: &str,
    parent: Option<&ParentLink<'_>>,
    subagent_depth: i64,
    created_at: &str,
) -> Result<String> {
    seed_request_in_state(
        graphql,
        agent_did,
        request_id,
        behavior_id,
        parent,
        subagent_depth,
        created_at,
        "pending",
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn seed_request_in_state(
    graphql: &str,
    agent_did: &str,
    request_id: &str,
    behavior_id: &str,
    parent: Option<&ParentLink<'_>>,
    subagent_depth: i64,
    created_at: &str,
    lifecycle_state: &str,
) -> Result<String> {
    let session_id = format!("session-{request_id}");
    let parent_fields = parent
        .map(|link| {
            format!(
                r#"
                    ,
                    caused_by_parent_request_id: "{}",
                    caused_by_parent_request_doc_id: "{}",
                    caused_by_parent_tool_call_id: "{}",
                    caused_by_parent_tool_call_doc_id: "{}",
                    caused_by_trigger_id: "{}",
                    caused_by_trigger_kind: "subagent","#,
                escape_graphql_string(link.parent_request_id),
                escape_graphql_string(link.parent_doc_id),
                escape_graphql_string(link.tool_call_id),
                escape_graphql_string(link.tool_call_doc_id),
                escape_graphql_string(link.tool_call_id),
            )
        })
        .unwrap_or_default();
    let response = graphql_query(
        graphql,
        &format!(
            r#"mutation {{
                create_AgentRequest(input: {{
                    request_id: "{request_id}",
                    agent_did: "{agent_did}",
                    behavior_id: "{behavior_id}",
                    session_id: "{session_id}",
                    retry_parent_request: "",
                    retry_root_request: "{request_id}",
                    superseded_by_request: "",
                    content: "seeded subagent list row",
                    lifecycle_state: "{lifecycle_state}",
                    backend_id: "",
                    execution_origin: "interactive",
                    failure_reason: "",
                    created_at: "{created_at}",
                    retry_count: 0,
                    max_retries: 3,
                    subagent_depth: {subagent_depth}{parent_fields}
                }}) {{ _docID }}
            }}"#,
            request_id = escape_graphql_string(request_id),
            agent_did = escape_graphql_string(agent_did),
            behavior_id = escape_graphql_string(behavior_id),
            session_id = escape_graphql_string(&session_id),
            created_at = escape_graphql_string(created_at),
            lifecycle_state = escape_graphql_string(lifecycle_state),
        ),
    )
    .await?;
    doc_id_from_create(&response, "add_AgentRequest")
}

/// Seed the durable spawn bridge and its accepted canonical tool-call header.
/// The descendant graph requires both the physical child link and the exact
/// accepted invocation; neither a bare tool row nor nearby transcript content
/// grants lineage. Returns `(tool_call_id, _docID)` for the child provenance.
#[allow(clippy::too_many_arguments)]
async fn seed_spawn_bridge(
    graphql: &str,
    agent_did: &str,
    parent_request_id: &str,
    parent_doc_id: &str,
    child_request_id: &str,
    message_sequence: u32,
    started_at: &str,
    bridge_state: &str,
) -> Result<(String, String)> {
    use gents::config_client::ConfigAccess;
    use gents::session::canonical_rows::{
        output_segment_create_variables, transcript_message_create_variables,
        CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    use gents_protocol::output::{
        MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
        OutputWriter, PayloadRef, SegmentRun, SourceClose, StreamDeclaration, StreamPayload,
        TranscriptMessage,
    };
    use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};

    let tool_call_id = format!("spawn-{child_request_id}");
    let session_id = format!("session-{parent_request_id}");
    let response = graphql_query(
        graphql,
        &format!(
            r#"mutation {{
                create_AgentToolCall(input: {{
                    tool_call_key: "bridge-{child_request_id}",
                    request_id: "{parent_request_id}",
                    request_doc_id: "{parent_doc_id}",
                    session_id: "{session_id}",
                    agent_did: "{agent_did}",
                    message_sequence: {message_sequence},
                    tool_name: "spawn_subagent",
                    tool_call_id: "{tool_call_id}",
                    status: "{bridge_state}",
                    lifecycle_state: "{bridge_state}",
                    started_at: "{started_at}",
                    await_mode: "foreground",
                    child_request_id: "{child_request_id}",
                    spawn_target_did: "{agent_did}"
                }}) {{ _docID }}
            }}"#,
            child_request_id = escape_graphql_string(child_request_id),
            parent_request_id = escape_graphql_string(parent_request_id),
            parent_doc_id = escape_graphql_string(parent_doc_id),
            session_id = escape_graphql_string(&session_id),
            agent_did = escape_graphql_string(agent_did),
            tool_call_id = escape_graphql_string(&tool_call_id),
            started_at = escape_graphql_string(started_at),
            bridge_state = escape_graphql_string(bridge_state),
        ),
    )
    .await?;
    let doc_id = doc_id_from_create(&response, "add_AgentToolCall")?;
    let access = ConfigAccess::Graphql(graphql.to_owned());
    let generation = format!("subagent-list:{parent_doc_id}:{message_sequence}");
    let arguments = "{}";
    let segment = OutputSegment {
        agent_did: agent_did.into(),
        requester_did: None,
        session_id: session_id.clone(),
        request_doc_id: parent_doc_id.into(),
        source: OutputSource::ProviderTurn {
            scope: CaptureScope {
                kind: CaptureScopeKind::Inference,
                seq: u64::from(message_sequence),
            },
            turn_index: 0,
            attempt: 0,
        },
        writer: OutputWriter::RequestExecution {
            execution_generation: generation.clone(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: arguments.len() as u32,
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::ToolArguments {
                    id: tool_call_id.clone(),
                    call_id: Some(tool_call_id.clone()),
                    name: "spawn_subagent".into(),
                },
            }),
        }],
        payload: arguments.into(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![arguments.len() as u64],
        }),
        created_at: started_at.into(),
    };
    let segment_response = crate::support::graphql::graphql_mutation_with_variables(
        &access,
        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
        &output_segment_create_variables(&segment)?,
    )
    .await?;
    let close_doc_id =
        gents_protocol::graphql::extract_mutation_doc_id(&segment_response, "AgentOutputSegment")?;
    let header = TranscriptMessage {
        message_key: gents::session::sequence_message_key(
            agent_did,
            &session_id,
            None,
            message_sequence,
        ),
        session_id,
        agent_did: agent_did.into(),
        requester_did: None,
        request_doc_id: Some(parent_doc_id.into()),
        publication: MessagePublication::RequestExecution {
            execution_generation: generation,
        },
        outcome: OutputOutcome::Complete,
        sequence: message_sequence,
        role: MessageRole::Assistant,
        native_id: None,
        blocks: vec![MessageBlock::ToolCall {
            tool_call_doc_id: doc_id.clone(),
            id: tool_call_id.clone(),
            call_id: Some(tool_call_id.clone()),
            name: "spawn_subagent".into(),
            arguments: PayloadRef {
                close_doc_id,
                stream: 0,
            },
            signature: None,
            additional_params: None,
        }],
        created_at: started_at.into(),
    };
    crate::support::graphql::graphql_mutation_with_variables(
        &access,
        CREATE_AGENT_MESSAGE_MUTATION,
        &transcript_message_create_variables(&header)?,
    )
    .await?;
    Ok((tool_call_id, doc_id))
}

fn assert_lineage_row(
    rows: &[Value],
    request_id: &str,
    parent_request_id: Option<&str>,
    depth: i64,
    behavior_id: &str,
) -> Result<()> {
    let row = rows
        .iter()
        .find(|row| row.get("child_request_id").and_then(Value::as_str) == Some(request_id))
        .with_context(|| format!("missing row for {request_id}: {rows:?}"))?;
    assert_eq!(
        row.get("parent_request_id").and_then(Value::as_str),
        parent_request_id
    );
    assert_eq!(row.get("depth").and_then(Value::as_i64), Some(depth));
    assert_eq!(
        row.get("behavior_id").and_then(Value::as_str),
        Some(behavior_id)
    );
    assert_eq!(
        row.get("request_lifecycle_state").and_then(Value::as_str),
        Some("pending")
    );
    let expected_edge_state = parent_request_id.map(|_| "pending");
    assert_eq!(
        row.get("edge_state").and_then(Value::as_str),
        expected_edge_state,
        "edge state comes from the parent bridge, never the request row: {row}"
    );
    assert!(
        row.get("started_at")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty()),
        "row must expose started_at: {row}"
    );
    Ok(())
}
