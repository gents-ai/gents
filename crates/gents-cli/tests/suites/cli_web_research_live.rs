use crate::support::*;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
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
                version: "0.1.10"
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
                    AgentBehaviorReadiness(
                        filter: {{ agent_did: {{ _eq: "{agent_did}" }} }},
                        limit: 1
                    ) {{
                        snapshot_json
                    }}
                }}"#
            ),
        )
        .await?;
        let snapshot = response
            .pointer("/data/AgentBehaviorReadiness/0/snapshot_json")
            .and_then(Value::as_str)
            .and_then(|snapshot| serde_json::from_str::<Value>(snapshot).ok());
        if snapshot.as_ref().is_some_and(|snapshot| {
            snapshot.get("process_state").and_then(Value::as_str) == Some("ready")
                && snapshot
                    .get("active_generation")
                    .and_then(Value::as_u64)
                    .is_some_and(|generation| {
                        generation > 0
                            && snapshot.get("router_generation").and_then(Value::as_u64)
                                == Some(generation)
                    })
                && snapshot
                    .get("behaviors")
                    .and_then(Value::as_array)
                    .is_some_and(|behaviors| {
                        behaviors.len() == 5
                            && behaviors.iter().all(|behavior| {
                                behavior.get("state").and_then(Value::as_str) == Some("ready")
                                    && behavior.get("reason").is_some_and(Value::is_null)
                            })
                    })
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
                row.get("tool_count").and_then(Value::as_i64) == Some(2),
                "research gateway must expose exactly collect + stored-find, got: {row}"
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

fn ledger_rows<'a>(ledger: &'a Value, collection: &str) -> Result<&'a [Value]> {
    ledger
        .pointer(&format!("/data/{collection}"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .with_context(|| format!("research ledger is missing {collection}: {ledger}"))
}

fn string_count(row: &Value, field: &str) -> Result<usize> {
    string_field(row, field)?
        .parse::<usize>()
        .with_context(|| format!("ledger row has invalid count {field}: {row}"))
}

fn string_number(row: &Value, field: &str) -> Result<f64> {
    string_field(row, field)?
        .parse::<f64>()
        .with_context(|| format!("ledger row has invalid number {field}: {row}"))
}

fn string_json_array(row: &Value, field: &str) -> Result<Vec<Value>> {
    serde_json::from_str::<Value>(string_field(row, field)?)
        .with_context(|| format!("ledger row has invalid JSON {field}: {row}"))?
        .as_array()
        .cloned()
        .with_context(|| format!("ledger row field {field} is not a JSON array: {row}"))
}

fn string_field<'a>(row: &'a Value, field: &str) -> Result<&'a str> {
    row.get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("ledger row has no string {field}: {row}"))
}

fn markdown_http_link_targets(markdown: &str) -> BTreeSet<&str> {
    markdown
        .split("](")
        .skip(1)
        .filter_map(|suffix| suffix.split(')').next())
        .map(str::trim)
        .filter(|target| target.starts_with("http://") || target.starts_with("https://"))
        .collect()
}

fn expected_research_tool_surfaces() -> [(&'static str, &'static [&'static str]); 4] {
    [
        (
            "Web research planner",
            &[
                "discover_tools",
                "describe_tool",
                "call_tool",
                "get_goal",
                "update_goal",
                "write_research_assignment",
                "write_research_plan",
            ],
        ),
        (
            "Web evidence investigator",
            &[
                "discover_tools",
                "describe_tool",
                "call_tool",
                "get_goal",
                "update_goal",
                "write_research_source",
                "write_research_claim",
                "write_research_evidence",
                "write_research_investigation",
            ],
        ),
        (
            "Research evidence adjudicator",
            &[
                "read_research_investigation",
                "read_research_source",
                "read_research_claim",
                "read_research_evidence",
                "write_research_claim_verdict",
                "write_research_draft",
            ],
        ),
        (
            "Cited research reporter",
            &[
                "read_report_research_source",
                "read_report_research_evidence",
                "read_report_claim_verdict",
                "write_research_result",
            ],
        ),
    ]
}

fn verify_exact_research_tool_surfaces(explanation: &Value) -> Result<()> {
    for (display_name, expected_tools) in expected_research_tool_surfaces() {
        let behavior = explanation
            .get("behaviors")
            .and_then(Value::as_array)
            .and_then(|behaviors| {
                behaviors.iter().find(|behavior| {
                    behavior.get("display_name").and_then(Value::as_str) == Some(display_name)
                })
            })
            .with_context(|| format!("tool explanation is missing {display_name}"))?;
        anyhow::ensure!(
            behavior.get("tool_policy_version").and_then(Value::as_str) == Some("tool-policy/v1"),
            "{display_name} is not using secure-default tool policy decoding: {behavior}"
        );
        let actual = behavior
            .pointer("/surface/tool_names")
            .and_then(Value::as_array)
            .with_context(|| format!("{display_name} has no explained tool surface"))?
            .iter()
            .filter_map(Value::as_str)
            .collect::<BTreeSet<_>>();
        let expected = expected_tools.iter().copied().collect::<BTreeSet<_>>();
        anyhow::ensure!(
            actual == expected,
            "{display_name} has authority beyond its exact stage surface; expected {expected:?}, got {actual:?}"
        );
    }
    Ok(())
}

async fn wait_for_exact_research_tool_surfaces(
    home: &Path,
    graphql: &str,
    agent_did: &str,
    timeout: Duration,
) -> Result<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let explanation = run_cli_json(
            home,
            &[
                "tools",
                "explain",
                "--home",
                home.to_str().context("live research home is not UTF-8")?,
                "--graphql",
                graphql,
                "--agent-did",
                agent_did,
            ],
        )?;
        match verify_exact_research_tool_surfaces(&explanation) {
            Ok(()) => return Ok(explanation),
            Err(error) if Instant::now() < deadline => {
                tracing::debug!(%error, "waiting for exact research tool surfaces");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(error) => {
                return Err(error).context(format!(
                    "runtime did not project exact research tool surfaces before timeout: {explanation}"
                ));
            }
        }
    }
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
            "pack",
            "install",
            "web_deep_research",
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
    wait_for_exact_research_tool_surfaces(&home_dir, &graphql, &agent_did, Duration::from_secs(45))
        .await?;

    let question = std::env::var("GENTS_WEB_RESEARCH_QUESTION")
        .unwrap_or_else(|_| DEFAULT_RESEARCH_QUESTION.to_string());
    let run = run_cli_json(
        &home_dir,
        &[
            "graph",
            "run",
            "web_deep_research",
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
    let gateway_calls = tool_calls
        .iter()
        .filter(|call| call.get("selected_service_id").and_then(Value::as_str) == Some(SERVICE_ID))
        .collect::<Vec<_>>();
    for call in &gateway_calls {
        anyhow::ensure!(
            matches!(
                call.get("selected_tool_name").and_then(Value::as_str),
                Some("web_collect_evidence" | "web_find_in_fetch")
            ),
            "investigator reached a gateway tool outside its least-privilege surface: {call}"
        );
    }
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
        gateway_calls
            .iter()
            .filter(|call| {
                call.get("selected_tool_name").and_then(Value::as_str)
                    == Some("web_collect_evidence")
            })
            .count()
            == 4
            && completed_real_tool_count("web_collect_evidence") == 4,
        "the planner and each investigator must make exactly one successful bounded collection, with no retry: {watch}"
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
    let claim_results = result_documents(&result, "claims")?;
    anyhow::ensure!(
        claim_results.len() >= 6,
        "too few persisted evidence claims: {result}"
    );
    let evidence_results = result_documents(&result, "evidence")?;
    anyhow::ensure!(
        evidence_results.len() >= claim_results.len(),
        "every claim needs at least one typed evidence link: {result}"
    );
    anyhow::ensure!(
        result_documents(&result, "verdicts")?.len() >= 6,
        "too few persisted adjudicated verdicts: {result}"
    );

    let correlation = escape_graphql_string(&run_id);
    let ledger = graphql_query(
        &graphql,
        &format!(
            r#"{{
                WebResearchPlan(filter: {{ run_id: {{ _eq: "{correlation}" }} }}) {{ question assignment_count }}
                WebResearchAssignment(filter: {{ run_id: {{ _eq: "{correlation}" }} }}) {{ assignment_id question lens query_plan expected_total }}
                WebResearchInvestigation(filter: {{ run_id: {{ _eq: "{correlation}" }} }}) {{ assignment_id expected_total requested_source_count candidate_count scrape_attempt_count evidence_shortfall accepted_search_engines search_degradation retrieval_failures status }}
                WebResearchSource(filter: {{ run_id: {{ _eq: "{correlation}" }} }}) {{ source_id assignment_id url fetch_id content_hash matched_query retrieval_queries search_engines candidate_relevance_score content_relevance_score extraction_method content_integrity_verified verified_quote quote_verified }}
                WebResearchClaim(filter: {{ run_id: {{ _eq: "{correlation}" }} }}) {{ claim_id assignment_id statement }}
                WebResearchEvidence(filter: {{ run_id: {{ _eq: "{correlation}" }} }}) {{ evidence_id assignment_id claim_id source_id relationship locator supporting_excerpt }}
                WebResearchClaimVerdict(filter: {{ run_id: {{ _eq: "{correlation}" }} }}) {{ claim_id statement verdict confidence rationale evidence_summary quote_verification }}
                WebResearchDraft(filter: {{ run_id: {{ _eq: "{correlation}" }} }}) {{ title thesis }}
            }}"#
        ),
    )
    .await?;

    let plans = ledger_rows(&ledger, "WebResearchPlan")?;
    anyhow::ensure!(
        plans.len() == 1
            && string_field(&plans[0], "question")? == question
            && string_count(&plans[0], "assignment_count")? == 3,
        "planner did not close the requested three-member assignment set: {ledger}"
    );
    let assignments = ledger_rows(&ledger, "WebResearchAssignment")?;
    let assignment_ids = assignments
        .iter()
        .map(|row| string_field(row, "assignment_id"))
        .collect::<Result<BTreeSet<_>>>()?;
    anyhow::ensure!(
        assignments.len() == 3
            && assignment_ids.len() == 3
            && assignments
                .iter()
                .all(|row| row.get("expected_total").and_then(Value::as_str) == Some("3"))
            && assignments.iter().all(|row| {
                row.get("question").and_then(Value::as_str) == Some(question.as_str())
                    && row
                        .get("lens")
                        .and_then(Value::as_str)
                        .is_some_and(|value| !value.trim().is_empty())
                    && row
                        .get("query_plan")
                        .and_then(Value::as_str)
                        .is_some_and(|value| !value.trim().is_empty())
            }),
        "assignment identities or expected totals are inconsistent: {ledger}"
    );

    let sources = ledger_rows(&ledger, "WebResearchSource")?;
    let mut sources_by_id = BTreeMap::new();
    let mut source_counts = BTreeMap::<&str, usize>::new();
    for source in sources {
        let source_id = string_field(source, "source_id")?;
        let assignment_id = string_field(source, "assignment_id")?;
        anyhow::ensure!(
            assignment_ids.contains(assignment_id),
            "orphan source: {source}"
        );
        for field in ["url", "fetch_id", "content_hash"] {
            anyhow::ensure!(
                !string_field(source, field)?.trim().is_empty(),
                "source lacks required provenance: {source}"
            );
        }
        for field in ["matched_query", "extraction_method"] {
            anyhow::ensure!(
                !string_field(source, field)?.trim().is_empty(),
                "source lacks retrieval-quality handoff field {field}: {source}"
            );
        }
        anyhow::ensure!(
            !string_json_array(source, "retrieval_queries")?.is_empty()
                && !string_json_array(source, "search_engines")?.is_empty(),
            "source retrieval query and engine arrays must be non-empty: {source}"
        );
        anyhow::ensure!(
            string_number(source, "candidate_relevance_score")? >= 24.0
                && string_number(source, "content_relevance_score")? >= 28.0,
            "source did not preserve the gateway relevance thresholds: {source}"
        );
        anyhow::ensure!(
            string_field(source, "content_integrity_verified")? == "true",
            "source was persisted without gateway content integrity: {source}"
        );
        anyhow::ensure!(
            matches!(string_field(source, "quote_verified")?, "true" | "false"),
            "source quote verification must be an explicit boolean string: {source}"
        );
        anyhow::ensure!(
            sources_by_id.insert(source_id, source).is_none(),
            "duplicate source ID: {source_id}"
        );
        *source_counts.entry(assignment_id).or_default() += 1;
    }

    let claims = ledger_rows(&ledger, "WebResearchClaim")?;
    let mut claims_by_id = BTreeMap::new();
    let mut claim_counts = BTreeMap::<&str, usize>::new();
    for claim in claims {
        let claim_id = string_field(claim, "claim_id")?;
        let assignment_id = string_field(claim, "assignment_id")?;
        anyhow::ensure!(
            assignment_ids.contains(assignment_id),
            "orphan claim: {claim}"
        );
        anyhow::ensure!(
            !string_field(claim, "statement")?.trim().is_empty(),
            "claim has no statement: {claim}"
        );
        anyhow::ensure!(
            claims_by_id.insert(claim_id, claim).is_none(),
            "duplicate claim ID: {claim_id}"
        );
        *claim_counts.entry(assignment_id).or_default() += 1;
    }

    let evidence = ledger_rows(&ledger, "WebResearchEvidence")?;
    let mut evidence_ids = BTreeSet::new();
    let mut evidence_counts = BTreeMap::<&str, usize>::new();
    let mut links_per_claim = BTreeMap::<&str, usize>::new();
    for link in evidence {
        let evidence_id = string_field(link, "evidence_id")?;
        let assignment_id = string_field(link, "assignment_id")?;
        let claim_id = string_field(link, "claim_id")?;
        let source_id = string_field(link, "source_id")?;
        let claim = claims_by_id
            .get(claim_id)
            .with_context(|| format!("evidence links an absent claim: {link}"))?;
        let source = sources_by_id
            .get(source_id)
            .with_context(|| format!("evidence links an absent source: {link}"))?;
        anyhow::ensure!(
            string_field(claim, "assignment_id")? == assignment_id
                && string_field(source, "assignment_id")? == assignment_id,
            "cross-assignment evidence link: {link}"
        );
        anyhow::ensure!(
            matches!(
                string_field(link, "relationship")?,
                "supports" | "contradicts" | "context"
            ),
            "evidence relationship is outside the contract: {link}"
        );
        anyhow::ensure!(
            evidence_ids.insert(evidence_id),
            "duplicate evidence ID: {evidence_id}"
        );
        *evidence_counts.entry(assignment_id).or_default() += 1;
        *links_per_claim.entry(claim_id).or_default() += 1;
    }
    anyhow::ensure!(
        claims_by_id.keys().all(|claim_id| links_per_claim
            .get(claim_id)
            .copied()
            .unwrap_or_default()
            >= 1),
        "at least one claim has no typed evidence link: {ledger}"
    );
    anyhow::ensure!(
        assignment_ids.iter().all(|assignment_id| {
            source_counts.get(assignment_id).copied().unwrap_or_default() >= 2
                && (6..=8).contains(
                    &claim_counts
                        .get(assignment_id)
                        .copied()
                        .unwrap_or_default(),
                )
                && evidence_counts
                    .get(assignment_id)
                    .copied()
                    .unwrap_or_default()
                    >= claim_counts
                        .get(assignment_id)
                        .copied()
                        .unwrap_or_default()
        }),
        "every investigator must persist at least two fetched sources, six to eight claims, and enough typed evidence links: {ledger}"
    );

    let investigations = ledger_rows(&ledger, "WebResearchInvestigation")?;
    let investigation_ids = investigations
        .iter()
        .map(|row| string_field(row, "assignment_id"))
        .collect::<Result<BTreeSet<_>>>()?;
    anyhow::ensure!(
        investigations.len() == 3 && investigation_ids == assignment_ids,
        "investigation closure membership does not match assignments: {ledger}"
    );
    for closure in investigations {
        anyhow::ensure!(
            string_field(closure, "expected_total")? == "3"
                && matches!(string_field(closure, "status")?, "complete" | "partial"),
            "investigation closure metadata is inconsistent: {closure}"
        );
        anyhow::ensure!(
            string_count(closure, "requested_source_count")? == 6
                && string_count(closure, "candidate_count")? >= 2
                && string_count(closure, "scrape_attempt_count")? <= 12
                && matches!(
                    string_field(closure, "evidence_shortfall")?,
                    "true" | "false"
                )
                && !string_json_array(closure, "accepted_search_engines")?.is_empty(),
            "investigation closure did not preserve bounded retrieval diagnostics: {closure}"
        );
        let _ = string_json_array(closure, "search_degradation")?;
        let _ = string_json_array(closure, "retrieval_failures")?;
    }

    let verdicts = ledger_rows(&ledger, "WebResearchClaimVerdict")?;
    let mut verdict_claim_ids = BTreeSet::new();
    for verdict in verdicts {
        let claim_id = string_field(verdict, "claim_id")?;
        let claim = claims_by_id
            .get(claim_id)
            .with_context(|| format!("verdict refers to absent claim: {verdict}"))?;
        anyhow::ensure!(
            string_field(verdict, "statement")? == string_field(claim, "statement")?,
            "verdict did not carry the exact claim statement: {verdict}"
        );
        for field in [
            "confidence",
            "rationale",
            "evidence_summary",
            "quote_verification",
        ] {
            anyhow::ensure!(
                !string_field(verdict, field)?.trim().is_empty(),
                "verdict has an empty {field}: {verdict}"
            );
        }
        match string_field(verdict, "verdict")? {
            "supported" | "disputed" | "insufficient" => {}
            other => anyhow::bail!("invalid verdict {other:?}: {verdict}"),
        }
        anyhow::ensure!(
            verdict_claim_ids.insert(claim_id),
            "duplicate verdict: {verdict}"
        );
    }
    anyhow::ensure!(
        verdict_claim_ids == claims_by_id.keys().copied().collect(),
        "adjudication did not produce exactly one verdict per claim: {ledger}"
    );
    let drafts = ledger_rows(&ledger, "WebResearchDraft")?;
    anyhow::ensure!(
        drafts.len() == 1,
        "expected one adjudicated draft: {ledger}"
    );

    let reports = result_documents(&result, "report")?;
    anyhow::ensure!(reports.len() == 1, "expected exactly one report: {result}");
    let report = &reports[0];
    let report_markdown = string_field(report, "report_markdown")?;
    anyhow::ensure!(
        report_markdown.len() >= 1_500,
        "final report is not substantive: {report}"
    );
    let report_source_ledger: Value = serde_json::from_str(string_field(report, "sources_json")?)
        .context("final report sources_json is not valid JSON")?;
    let report_sources = report_source_ledger
        .as_array()
        .context("final report sources_json must be a JSON array")?;
    anyhow::ensure!(
        report_sources.len() >= 3,
        "final report must cite at least three ledger-backed sources: {report}"
    );
    let mut cited_urls = BTreeSet::new();
    let mut cited_source_ids = BTreeSet::new();
    for cited in report_sources {
        let source_id = string_field(cited, "source_id")?;
        let source = sources_by_id
            .get(source_id)
            .with_context(|| format!("report cites absent source {source_id:?}"))?;
        for field in ["url", "fetch_id", "content_hash"] {
            anyhow::ensure!(
                string_field(cited, field)? == string_field(source, field)?,
                "report source provenance disagrees with persisted source: {cited}"
            );
        }
        anyhow::ensure!(
            cited_source_ids.insert(source_id),
            "report source ledger duplicates {source_id:?}"
        );
        cited_urls.insert(string_field(cited, "url")?);
    }
    let markdown_urls = markdown_http_link_targets(report_markdown);
    anyhow::ensure!(
        markdown_urls.len() >= 3 && markdown_urls.is_subset(&cited_urls),
        "Markdown contains citations absent from the validated source ledger; markdown={markdown_urls:?}, ledger={cited_urls:?}"
    );

    let (_stdout, stderr) = serve.captured_output()?;
    anyhow::ensure!(
        !stderr.contains("mock"),
        "live runtime unexpectedly referenced a mock service:\n{stderr}"
    );
    Ok(())
}
