use crate::support::*;

use std::fs;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use uuid::Uuid;

const SERVICE_ID: &str = "web-research-mcp";
const DEFAULT_RESEARCH_QUESTION: &str = "How should an organization design a production deployment of the Model Context Protocol in 2026 to minimize prompt-injection and credential risks? Compare current MCP authorization and security guidance, the OAuth security best-current-practice, and at least two independent security analyses. Distinguish normative requirements from recommendations, identify disagreements, and cite primary sources.";

async fn register_real_web_research_service(graphql: &str, agent_did: &str) -> Result<()> {
    let hostname = hostname::get()
        .context("reading local hostname")?
        .into_string()
        .map_err(|_| anyhow!("local hostname is not UTF-8"))?;
    let service_id = escape_graphql_string(SERVICE_ID);
    let hostname = escape_graphql_string(&hostname);
    let agent_did = escape_graphql_string(agent_did);
    let mutation = format!(
        r#"mutation {{
            create_ToolServiceRegistry(input: {{
                service_id: "{service_id}",
                display_name: "Real Web Research MCP",
                description: "Live SearXNG and Firecrawl evidence gateway for {agent_did}",
                hostname: "{hostname}",
                tailscale_ip: null,
                lan_ip: null,
                mcp_port: 19213,
                mcp_path: "/mcp",
                send_agent_did: true,
                status: "online",
                version: "0.1.4"
            }}) {{ _docID }}
        }}"#
    );
    graphql_query(graphql, &mutation).await?;
    Ok(())
}

async fn configure_live_research_inference_profile(graphql: &str, agent_did: &str) -> Result<()> {
    let profile_id = escape_graphql_string(&format!("{agent_did}:default-profile"));
    let response = graphql_query(
        graphql,
        &format!(
            r#"{{
                InferenceProfile(
                    filter: {{ profile_id: {{ _eq: "{profile_id}" }} }},
                    limit: 1
                ) {{ _docID profile_id }}
            }}"#
        ),
    )
    .await?;
    let profile = response
        .pointer("/data/InferenceProfile/0")
        .context("initialized default inference profile is missing")?;
    let doc_id = profile
        .get("_docID")
        .and_then(Value::as_str)
        .context("initialized default inference profile has no _docID")?;
    graphql_query(
        graphql,
        &format!(
            r#"mutation {{
                update_InferenceProfile(
                    docID: "{}",
                    input: {{
                        max_output_tokens: 8192,
                        max_turns: 64,
                        reasoning_effort: "low"
                    }}
                ) {{ _docID }}
            }}"#,
            escape_graphql_string(doc_id),
        ),
    )
    .await?;
    Ok(())
}

async fn wait_for_all_research_behaviors_runnable(
    graphql: &str,
    agent_did: &str,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let agent_did = escape_graphql_string(agent_did);
    loop {
        let response = graphql_query(
            graphql,
            &format!(
                r#"{{
                    AgentRuntime(
                        filter: {{ agent_did: {{ _eq: "{agent_did}" }} }},
                        limit: 1
                    ) {{
                        active_generation
                        runnable_behavior_count
                        unavailable_behavior_count
                        last_reconcile_error
                    }}
                }}"#
            ),
        )
        .await?;
        if response.pointer("/data/AgentRuntime/0").is_some_and(|row| {
            row.get("runnable_behavior_count").and_then(Value::as_i64) == Some(5)
                && row
                    .get("unavailable_behavior_count")
                    .and_then(Value::as_i64)
                    == Some(0)
        }) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            anyhow::bail!(
                "runtime never made the default plus four research behaviors runnable: {response}"
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_for_runtime_mcp_health(
    graphql: &str,
    agent_did: &str,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let service_id = escape_graphql_string(SERVICE_ID);
    let agent_did = escape_graphql_string(agent_did);
    loop {
        let response = graphql_query(
            graphql,
            &format!(
                r#"{{
                    ToolServiceHealthState(
                        filter: {{ _and: [
                            {{ service_id: {{ _eq: "{service_id}" }} }},
                            {{ agent_did: {{ _eq: "{agent_did}" }} }}
                        ] }},
                        limit: 1
                    ) {{ status endpoint tool_count last_error_message }}
                }}"#
            ),
        )
        .await?;
        if let Some(row) = response
            .pointer("/data/ToolServiceHealthState/0")
            .filter(|row| row.get("status").and_then(Value::as_str) == Some("healthy"))
        {
            anyhow::ensure!(
                row.get("endpoint")
                    .and_then(Value::as_str)
                    .is_some_and(|endpoint| endpoint == "http://127.0.0.1:19213/mcp"),
                "runtime resolved the wrong MCP endpoint: {row}"
            );
            anyhow::ensure!(
                row.get("tool_count")
                    .and_then(Value::as_i64)
                    .is_some_and(|count| count >= 7),
                "runtime did not discover the complete research tool surface: {row}"
            );
            return Ok(());
        }
        if Instant::now() >= deadline {
            anyhow::bail!("runtime never marked {SERVICE_ID} healthy: {response}");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

fn result_documents<'a>(result: &'a Value, name: &str) -> Result<&'a [Value]> {
    result
        .get("results")
        .and_then(Value::as_array)
        .and_then(|results| {
            results
                .iter()
                .find(|entry| entry.get("name").and_then(Value::as_str) == Some(name))
        })
        .and_then(|entry| entry.get("documents"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .with_context(|| format!("graph result is missing {name:?} documents: {result}"))
}

fn usage_value(watch: &Value, field: &str) -> i64 {
    watch
        .pointer(&format!("/usage/{field}"))
        .and_then(Value::as_i64)
        .unwrap_or_default()
        .max(0)
}

fn gateway_tool_metric(metrics: &str, tool: &str, outcome: &str) -> u64 {
    metrics
        .lines()
        .find(|line| {
            line.starts_with("web_research_tool_calls_total{")
                && line.contains(&format!("tool=\"{tool}\""))
                && line.contains(&format!("outcome=\"{outcome}\""))
        })
        .and_then(|line| line.split_whitespace().last())
        .and_then(|value| value.parse::<f64>().ok())
        .map(|value| value as u64)
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "starts the real Docker search/extraction stack and consumes substantial real model tokens"]
async fn full_stack_web_deep_research_consumes_real_search_and_inference() -> Result<()> {
    let endpoint = std::env::var("GENTS_WEB_RESEARCH_MCP_ENDPOINT")
        .context("GENTS_WEB_RESEARCH_MCP_ENDPOINT must point at the real Docker fixture")?;
    anyhow::ensure!(
        endpoint == "http://127.0.0.1:19213/mcp",
        "live acceptance requires the real Docker fixture endpoint, got {endpoint:?}"
    );

    let tempdir = tempfile::tempdir().context("creating live web research tempdir")?;
    let home_dir = tempdir.path().join("agent-home");
    fs::create_dir_all(&home_dir)?;
    let home_arg = home_dir
        .to_str()
        .context("live web research home path is not UTF-8")?;

    let model_endpoint = std::env::var("GENTS_CLI_E2E_MODEL_ENDPOINT")
        .context("GENTS_CLI_E2E_MODEL_ENDPOINT must identify a real model endpoint")?;
    let model_name = std::env::var("GENTS_CLI_E2E_MODEL_NAME")
        .context("GENTS_CLI_E2E_MODEL_NAME must identify a real model")?;
    let mut init_args = vec![
        "--home".to_string(),
        home_arg.to_string(),
        "--agent-name".to_string(),
        format!("web-research-live-{}", Uuid::new_v4().simple()),
        "--model-name".to_string(),
        model_name,
        "--max-concurrent".to_string(),
        "6".to_string(),
        "--max-queue-depth".to_string(),
        "16".to_string(),
    ];
    if std::env::var("GENTS_CLI_E2E_API_KEY").is_ok_and(|api_key| !api_key.trim().is_empty()) {
        init_args.push("--api-key-env-var".to_string());
        init_args.push("GENTS_CLI_E2E_API_KEY".to_string());
    }
    init_args.push(model_endpoint);
    let init_arg_refs = init_args.iter().map(String::as_str).collect::<Vec<_>>();
    let init = run_init_json(&home_dir, &init_arg_refs)?;
    let agent_did = agent_did_from_init(&init)?;

    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let (serve, readiness) = spawn_server_with_ready_json(
        &home_dir,
        port,
        &["--home", home_arg],
        &[("RUST_LOG", "warn")],
    )?;
    anyhow::ensure!(
        readiness.get("status").and_then(Value::as_str) == Some("serving"),
        "Gents server did not become ready: {readiness}"
    );
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;
    configure_live_research_inference_profile(&graphql, &agent_did).await?;

    register_real_web_research_service(&graphql, &agent_did).await?;
    let probe = run_cli_json(
        &home_dir,
        &[
            "mcp",
            "probe",
            "--graphql",
            &graphql,
            "--timeout",
            "30s",
            "--output",
            "json",
            SERVICE_ID,
        ],
    )?;
    anyhow::ensure!(
        probe
            .pointer("/items/0/health_state")
            .and_then(Value::as_str)
            == Some("healthy"),
        "real MCP service probe failed: {probe}"
    );
    wait_for_runtime_mcp_health(&graphql, &agent_did, Duration::from_secs(45)).await?;

    let install = run_cli_json(
        &home_dir,
        &[
            "graph",
            "install",
            "web-deep-research",
            "--home",
            home_arg,
            "--output",
            "json",
        ],
    )?;
    anyhow::ensure!(
        install.pointer("/install/revision_digest").is_some(),
        "web research graph installation failed: {install}"
    );
    wait_for_runtime_quiescence(&graphql, &agent_did, 2, Duration::from_secs(6)).await?;
    wait_for_all_research_behaviors_runnable(&graphql, &agent_did, Duration::from_secs(30)).await?;
    let tool_explanation = run_cli_json(
        &home_dir,
        &[
            "tools",
            "explain",
            "--home",
            home_arg,
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
        ],
    )?;
    for display_name in ["Web evidence investigator"] {
        let behavior = tool_explanation
            .get("behaviors")
            .and_then(Value::as_array)
            .and_then(|behaviors| {
                behaviors.iter().find(|behavior| {
                    behavior.get("display_name").and_then(Value::as_str) == Some(display_name)
                })
            })
            .with_context(|| {
                format!("tool explanation is missing {display_name}: {tool_explanation}")
            })?;
        let tool_names = behavior
            .pointer("/surface/tool_names")
            .and_then(Value::as_array)
            .with_context(|| format!("{display_name} has no explained tool surface: {behavior}"))?;
        for required_tool in ["discover_tools", "describe_tool", "call_tool"] {
            anyhow::ensure!(
                tool_names
                    .iter()
                    .any(|name| name.as_str() == Some(required_tool)),
                "{display_name} cannot reach MCP because {required_tool} is absent: {behavior}"
            );
        }
    }

    let question = std::env::var("GENTS_WEB_RESEARCH_QUESTION")
        .unwrap_or_else(|_| DEFAULT_RESEARCH_QUESTION.to_string());
    let run = run_cli_json(
        &home_dir,
        &[
            "graph",
            "run",
            "web-deep-research",
            "--home",
            home_arg,
            "--question",
            &question,
            "--investigator-count",
            "3",
            "--output",
            "json",
        ],
    )?;
    let run_id = run
        .get("run_id")
        .and_then(Value::as_str)
        .context("graph run receipt is missing run_id")?
        .to_string();

    let watch_text = run_cli_text(
        &home_dir,
        &[
            "graph",
            "watch",
            &run_id,
            "--home",
            home_arg,
            "--interval-ms",
            "2000",
        ],
    )?;
    anyhow::ensure!(
        watch_text.contains("succeeded"),
        "live graph did not report success:\n{watch_text}"
    );
    let watch = run_cli_json(
        &home_dir,
        &[
            "graph", "watch", &run_id, "--home", home_arg, "--output", "json",
        ],
    )?;
    anyhow::ensure!(
        watch.pointer("/run/status").and_then(Value::as_str) == Some("succeeded"),
        "live graph watch was not terminal-successful: {watch}"
    );

    let inference_calls = watch
        .pointer("/activity/inference_calls")
        .and_then(Value::as_array)
        .context("watch output is missing inference calls")?;
    anyhow::ensure!(
        inference_calls.len() >= 10,
        "deep research used too few real model calls ({}): {watch}",
        inference_calls.len()
    );
    // Provider prompt/input totals already include their cached subset. Keep the
    // cached counter observable, but do not double-count it toward acceptance.
    let total_tokens = usage_value(&watch, "reported_input_tokens")
        + usage_value(&watch, "reported_output_tokens")
        + usage_value(&watch, "estimated_input_tokens");
    anyhow::ensure!(
        total_tokens >= 20_000,
        "deep research did not clear the 20k real/estimated token acceptance floor: {watch}"
    );

    let tool_calls = watch
        .pointer("/activity/tool_calls")
        .and_then(Value::as_array)
        .context("watch output is missing tool calls")?;
    let completed_real_tool_count = |tool_name: &str| {
        tool_calls
            .iter()
            .filter(|call| {
                call.get("selected_service_id").and_then(Value::as_str) == Some(SERVICE_ID)
                    && call
                        .get("selected_tool_name")
                        .and_then(Value::as_str)
                        .or_else(|| call.get("tool_name").and_then(Value::as_str))
                        == Some(tool_name)
                    && call
                        .get("lifecycle_state")
                        .and_then(Value::as_str)
                        .or_else(|| call.get("status").and_then(Value::as_str))
                        == Some("completed")
            })
            .count()
    };
    anyhow::ensure!(
        completed_real_tool_count("web_collect_evidence") >= 3,
        "investigators did not complete one bounded real evidence collection each: {watch}"
    );
    let gateway_metrics = reqwest::Client::new()
        .get("http://127.0.0.1:19213/metrics")
        .send()
        .await
        .context("GET real web research gateway metrics")?
        .error_for_status()
        .context("real web research gateway metrics status")?
        .text()
        .await
        .context("read real web research gateway metrics")?;
    anyhow::ensure!(
        gateway_tool_metric(&gateway_metrics, "web_search", "ok") >= 9,
        "bounded collections did not execute at least nine real SearXNG searches: {gateway_metrics}"
    );
    anyhow::ensure!(
        gateway_tool_metric(&gateway_metrics, "web_scrape_url", "ok") >= 8,
        "bounded collections did not execute at least eight real Firecrawl extractions: {gateway_metrics}"
    );
    anyhow::ensure!(
        gateway_tool_metric(&gateway_metrics, "web_verify_quote", "ok") >= 3,
        "bounded collections did not verify at least three exact stored excerpts: {gateway_metrics}"
    );

    let result = run_cli_json(
        &home_dir,
        &[
            "graph", "result", &run_id, "--home", home_arg, "--output", "json",
        ],
    )?;
    anyhow::ensure!(result.get("status").and_then(Value::as_str) == Some("succeeded"));
    anyhow::ensure!(result_documents(&result, "plan")?.len() == 1);
    anyhow::ensure!(
        result_documents(&result, "sources")?.len() >= 8,
        "too few persisted fetched sources: {result}"
    );
    anyhow::ensure!(
        result_documents(&result, "claims")?.len() >= 6,
        "too few persisted evidence claims: {result}"
    );
    anyhow::ensure!(
        result_documents(&result, "verdicts")?.len() >= 6,
        "too few persisted adjudicated verdicts: {result}"
    );
    let reports = result_documents(&result, "report")?;
    anyhow::ensure!(reports.len() == 1, "expected exactly one report: {result}");
    let report = &reports[0];
    anyhow::ensure!(
        report
            .get("report_markdown")
            .and_then(Value::as_str)
            .is_some_and(|markdown| markdown.len() >= 1_500 && markdown.contains("http")),
        "final report is not a substantive cited document: {report}"
    );
    anyhow::ensure!(
        report
            .get("sources_json")
            .and_then(Value::as_str)
            .is_some_and(|ledger| {
                ledger.contains("fetch_id")
                    && ledger.contains("content_hash")
                    && ledger.contains("http")
            }),
        "final report is missing the fetched evidence ledger: {report}"
    );

    let (_stdout, stderr) = serve.captured_output()?;
    anyhow::ensure!(
        !stderr.contains("mock"),
        "live runtime unexpectedly referenced a mock service:\n{stderr}"
    );
    Ok(())
}
