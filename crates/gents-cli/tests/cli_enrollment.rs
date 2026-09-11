//! Live enrollment e2e: a throwaway `gents server` plus a fresh desktop
//! `ClientCore`. This is the production phone/desktop pairing path — HTTP
//! `/status` offer, operator approve, two-leg client route — not the
//! hand-installed replicators used by the live desktop fixture.

#[path = "cli_enrollment/contract.rs"]
mod contract;
mod support;
use support::*;

use std::fs;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use gents::default_behavior_id_for_agent;
use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
use gents_desktop_core::local_runtime::fetch_runtime_connection_payload;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::time::{sleep, Instant};
use uuid::Uuid;

static ENROLLMENT_E2E_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn enrollment_e2e_lock() -> &'static Mutex<()> {
    ENROLLMENT_E2E_LOCK.get_or_init(|| Mutex::new(()))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_enrollment_from_fresh_desktop_replicates_chat_without_agent_principal() -> Result<()>
{
    let _guard = enrollment_e2e_lock().lock().await;

    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("agent-home");
    let desktop_home = tempdir.path().join("desktop-home");
    fs::create_dir_all(&home_dir)?;
    let home_arg = home_dir
        .to_str()
        .ok_or_else(|| anyhow!("agent home path is not UTF-8"))?;

    let (model_endpoint, model_name) = resolve_live_inference().await?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let agent_name = format!("cli-enroll-{}", Uuid::new_v4().simple());
    let reply_token = format!("ENROLL_LIVE_{}", Uuid::new_v4().simple());
    let prompt = format!(
        "This is an enrollment pairing smoke test. Reply with only the exact token {reply_token} and nothing else."
    );

    let init = run_init_json(
        &home_dir,
        &[
            "--home",
            home_arg,
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--max-concurrent",
            "2",
            "--max-queue-depth",
            "4",
            "--inference-url",
            &model_endpoint,
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let default_behavior_id = default_behavior_id_for_agent(&agent_did);

    let (mut serve, _readiness) =
        spawn_server_with_ready_json(&home_dir, port, &["--home", home_arg], &[])?;
    wait_for_port(port, &mut serve)?;

    serve
        .capturing(async {
            wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

            let status_url = format!("http://127.0.0.1:{port}");
            let (status, offer_token) = wait_for_enrollment_token(&status_url).await?;
            assert_eq!(
                status.get("agent_name").and_then(Value::as_str),
                Some(agent_name.as_str())
            );
            assert_eq!(
                status.get("agent_did").and_then(Value::as_str),
                Some(agent_did.as_str())
            );

            let core = ClientCore::start_with_paths_and_options(
                DesktopPaths::from_root(&desktop_home),
                ClientCoreOptions::local_only(),
            )
            .await
            .context("starting fresh desktop ClientCore")?;

            let pending = core
                .request_status_enrollment_with_label(&offer_token, Some(&agent_name))
                .await
                .context("requesting status enrollment from /status offer")?;
            assert_eq!(pending.state, "pending_approval");
            assert_eq!(pending.owner_agent, agent_did);

            let active = core
                .active_status_enrollment_requests()
                .await
                .context("loading pending enrollment requests")?;
            assert!(
                active.iter().any(|request| {
                    request.request_id == pending.request_id
                        && request.state == "pending_approval"
                }),
                "fresh desktop should surface the unpaired request as pending; got {active:?}"
            );

            wait_for_runtime_enrollment_request(&graphql, &pending.request_id).await?;
            run_cli_json(
                &home_dir,
                &[
                    "p2p",
                    "enrollment",
                    "approve",
                    &pending.request_id,
                    "--home",
                    home_arg,
                ],
            )
            .context("approving enrollment request")?;

            let enrolled = wait_for_chat_ready_enrollment(&core, &agent_did).await?;
            assert_ne!(
                enrolled.label.as_str(),
                "Enrolled Agent",
                "enrollment should seed the advertised agent name, not a placeholder"
            );
            assert_eq!(enrolled.label, agent_name);
            assert_eq!(enrolled.agent_did, agent_did);

            let principals = query_collection_dids(core.node(), "AgentPrincipal").await?;
            assert!(
                principals.is_empty(),
                "AgentPrincipal must stay on the runtime node; client gossiped {principals:?}"
            );
            assert!(
                core.store().snapshot().agent_principals.is_empty(),
                "client store must not materialize a gossiped AgentPrincipal"
            );

            wait_for_client_behavior_readiness(&core, &agent_did).await?;

            let session_id = Uuid::new_v4().to_string();
            core.submit_request(
                &session_id,
                &agent_did,
                &prompt,
                Some(&default_behavior_id),
            )
            .await
            .context("submitting chat request from the enrolled desktop")?;

            let (request_id, _, _) =
                wait_for_runtime_agent_request(&graphql, core.node(), &agent_did, &prompt).await?;
            let runtime_response = wait_for_complete_agent_response(
                &graphql,
                &request_id,
                Duration::from_secs(240),
            )
            .await?;
            let runtime_text = response_visible_text(&runtime_response);
            anyhow::ensure!(
                runtime_text.contains(&reply_token),
                "live inference response missing {reply_token}: {runtime_response}"
            );

            wait_for_client_complete_response(&core, &session_id, &agent_did, &request_id, &reply_token).await?;

            let runtime_principals = graphql_query(
                &graphql,
                r#"{ AgentPrincipal { agent_did display_name } }"#,
            )
            .await?;
            let runtime_rows = runtime_principals
                .pointer("/data/AgentPrincipal")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            assert!(
                runtime_rows.iter().any(|row| {
                    row.get("agent_did").and_then(Value::as_str) == Some(agent_did.as_str())
                }),
                "runtime must keep AgentPrincipal locally: {runtime_principals}"
            );

            let remaining = core
                .active_status_enrollment_requests()
                .await
                .context("loading enrollment requests after pairing")?;
            assert!(
                remaining.is_empty(),
                "accepted pairing must clear the pending enrollment queue so another agent can be added; got {remaining:?}"
            );

            core.shutdown().await?;
            Ok(())
        })
        .await
}

async fn resolve_live_inference() -> Result<(String, String)> {
    let endpoint = std::env::var("GENTS_CLI_E2E_MODEL_ENDPOINT")
        .unwrap_or_else(|_| DEFAULT_MODEL_ENDPOINT.to_string());
    let models_url = format!("{}/models", endpoint.trim_end_matches('/'));
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?
        .get(&models_url)
        .send()
        .await
        .with_context(|| {
            format!("live inference at {endpoint} is unreachable; set GENTS_CLI_E2E_MODEL_ENDPOINT")
        })?;
    anyhow::ensure!(
        response.status().is_success(),
        "live inference GET {models_url} returned {}",
        response.status()
    );
    let body: Value = response
        .json()
        .await
        .context("decoding live inference /models")?;
    let advertised = body
        .pointer("/data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| row.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect::<Vec<_>>();
    anyhow::ensure!(
        !advertised.is_empty(),
        "live inference at {endpoint} advertised no models: {body}"
    );
    let model_name = std::env::var("GENTS_CLI_E2E_MODEL_NAME").unwrap_or_else(|_| {
        advertised
            .iter()
            .find(|name| name.as_str() == "GLM-5.3-Flash-NVFP4")
            .cloned()
            .unwrap_or_else(|| advertised[0].clone())
    });
    anyhow::ensure!(
        advertised.iter().any(|name| name == &model_name),
        "live inference at {endpoint} does not serve {model_name}; advertised {advertised:?}"
    );
    Ok((endpoint, model_name))
}

async fn wait_for_enrollment_token(status_url: &str) -> Result<(Value, String)> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match fetch_runtime_connection_payload(status_url).await {
            Ok(status) => {
                let token = status
                    .pointer("/enrollment/token")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|token| !token.is_empty())
                    .map(str::to_owned);
                if let Some(token) = token {
                    return Ok((status, token));
                }
                if Instant::now() >= deadline {
                    bail!("timed out waiting for /status enrollment token; last={status}");
                }
            }
            Err(error) => {
                if Instant::now() >= deadline {
                    bail!("timed out waiting for /status enrollment token: {error:#}");
                }
            }
        }
        sleep(Duration::from_millis(200)).await;
    }
}

async fn wait_for_runtime_enrollment_request(graphql: &str, request_id: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let response =
            graphql_query(graphql, r#"{ NetworkEnrollmentRequest { request_id } }"#).await?;
        let found = response
            .pointer("/data/NetworkEnrollmentRequest")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|row| row.get("request_id").and_then(Value::as_str) == Some(request_id));
        if found {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for runtime to observe enrollment request {request_id}; last={response}"
            );
        }
        sleep(Duration::from_millis(200)).await;
    }
}

async fn wait_for_runtime_agent_request(
    graphql: &str,
    client_node: &gents::defra_node::EmbeddedNode,
    agent_did: &str,
    content: &str,
) -> Result<(String, String, String)> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match graphql_query(
            graphql,
            &format!(
                r#"{{
                    AgentRequest(
                        filter: {{
                            agent_did: {{ _eq: "{}" }},
                            content: {{ _eq: "{}" }}
                        }},
                        order: {{ created_at: DESC }},
                        limit: 1
                    ) {{
                        request_id
                        session_id
                        behavior_id
                        content
                    }}
                }}"#,
                escape_graphql_string(agent_did),
                escape_graphql_string(content),
            ),
        )
        .await
        {
            Ok(response) => {
                if let Ok(row) = first_graphql_row(&response, "AgentRequest") {
                    return Ok((
                        row.get("request_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        row.get("session_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        row.get("behavior_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    ));
                }
            }
            Err(error) => {
                if Instant::now() >= deadline {
                    bail!("runtime AgentRequest query failed: {error:#}");
                }
            }
        }
        if Instant::now() >= deadline {
            let client_requests = client_node
                .execute("{ AgentRequest { request_id agent_did content requester_did } }")
                .await;
            let runtime_all = graphql_query(
                graphql,
                "{ AgentRequest { request_id agent_did content requester_did } }",
            )
            .await
            .unwrap_or_else(|error| serde_json::json!({ "error": error.to_string() }));
            bail!(
                "runtime never received the enrolled client's AgentRequest; \
                 runtime_all={runtime_all}; client_requests={:?}",
                client_requests.data
            );
        }
        sleep(Duration::from_millis(250)).await;
    }
}

async fn assistant_message_text(graphql: &str, request_id: &str) -> Result<String> {
    let response = graphql_query(
        graphql,
        &format!(
            r#"{{
                AgentMessage(filter: {{ request_id: {{ _eq: "{}" }} }}) {{
                    role content
                }}
            }}"#,
            escape_graphql_string(request_id),
        ),
    )
    .await?;
    Ok(response
        .pointer("/data/AgentMessage")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|row| {
            row.get("role")
                .and_then(Value::as_str)
                .is_some_and(|role| role == "assistant" || role == "agent")
        })
        .filter_map(|row| {
            row.get("content").and_then(Value::as_str).map(|content| {
                gents_protocol::transcript::present_persisted_message("assistant", content)
                    .body_markdown
            })
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

fn response_visible_text(row: &Value) -> String {
    row.get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

async fn wait_for_complete_agent_response(
    graphql: &str,
    request_id: &str,
    timeout: Duration,
) -> Result<Value> {
    let deadline = Instant::now() + timeout;
    let mut empty_complete_since = None::<Instant>;
    loop {
        let response = graphql_query(
            graphql,
            &format!(
                r#"{{
                    AgentResponse(
                        filter: {{ request_id: {{ _eq: "{}" }} }},
                        limit: 1
                    ) {{
                        request_id
                        status
                        content
                        reasoning
                        error_message
                    }}
                }}"#,
                escape_graphql_string(request_id),
            ),
        )
        .await?;
        if let Ok(row) = first_graphql_row(&response, "AgentResponse") {
            match row.get("status").and_then(Value::as_str) {
                Some("complete") => {
                    let mut visible = response_visible_text(row);
                    if visible.trim().is_empty() {
                        visible = assistant_message_text(graphql, request_id).await?;
                    }
                    if !visible.trim().is_empty() {
                        let mut row = row.clone();
                        if let Some(map) = row.as_object_mut() {
                            map.insert("content".to_string(), Value::String(visible));
                        }
                        return Ok(row);
                    }
                    if empty_complete_since
                        .get_or_insert_with(Instant::now)
                        .elapsed()
                        >= Duration::from_secs(15)
                    {
                        let messages = assistant_message_text(graphql, request_id).await?;
                        bail!(
                            "live inference completed with empty content for {request_id}: {row}; messages={messages:?}"
                        );
                    }
                }
                Some("error") => {
                    bail!("live inference returned an error response for {request_id}: {row}")
                }
                _ => {}
            }
        }
        if Instant::now() >= deadline {
            let messages = graphql_query(
                graphql,
                &format!(
                    r#"{{
                        AgentMessage(filter: {{ request_id: {{ _eq: "{}" }} }}) {{
                            role content
                        }}
                    }}"#,
                    escape_graphql_string(request_id),
                ),
            )
            .await
            .unwrap_or_else(|error| serde_json::json!({ "error": error.to_string() }));
            bail!(
                "timed out waiting for complete AgentResponse with visible text for {request_id}; last={response}; messages={messages}"
            );
        }
        sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_for_client_complete_response(
    core: &ClientCore,
    session_id: &str,
    agent_did: &str,
    request_id: &str,
    token: &str,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        // Match desktop_session_snapshot: transcript rows deliberately do not
        // live in the global observer. Read the app's bounded, scoped page.
        core.ensure_session_hydration_started(session_id, agent_did)
            .await?;
        core.refresh_local_request(agent_did, request_id).await?;
        let page = gents_desktop_core::client::load_session_transcript_page(
            core.node(),
            session_id,
            Some(agent_did),
            Some(core.principal().did()),
            None,
            Some(40),
        )
        .await?;
        let snapshot = core.store().snapshot();
        let response_text = snapshot
            .responses
            .iter()
            .filter(|row| row.request_id.as_deref() == Some(request_id))
            .map(|row| row.content.as_deref().unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        let message_text = page
            .store
            .messages
            .iter()
            .filter(|row| row.request_id.as_deref() == Some(request_id))
            .filter(|row| matches!(row.role.as_deref(), Some("assistant" | "agent")))
            .map(|row| {
                gents_protocol::transcript::present_persisted_message(
                    "assistant",
                    row.content.as_deref().unwrap_or_default(),
                )
                .body_markdown
            })
            .collect::<Vec<_>>()
            .join("\n");
        let complete = snapshot.responses.iter().any(|row| {
            row.request_id.as_deref() == Some(request_id)
                && row.status.as_deref() == Some("complete")
        });
        if complete && (response_text.contains(token) || message_text.contains(token)) {
            return Ok(());
        }
        if snapshot.responses.iter().any(|row| {
            row.request_id.as_deref() == Some(request_id) && row.status.as_deref() == Some("error")
        }) {
            bail!(
                "client received an error response for {request_id}: {:?}",
                snapshot.responses
            );
        }
        if Instant::now() >= deadline {
            let node_messages = core
                .node()
                .execute(&format!(
                    r#"{{ AgentMessage(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ role content }} }}"#,
                    escape_graphql_string(request_id),
                ))
                .await;
            bail!(
                "client never received the complete live response for {request_id} containing {token}; responses={:?}; messages={:?}; node_messages={:?}",
                snapshot.responses,
                snapshot.messages,
                node_messages.data
            );
        }
        sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_for_chat_ready_enrollment(
    core: &ClientCore,
    agent_did: &str,
) -> Result<gents_desktop_core::client::PeerRecord> {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let now = Utc::now();
        if let Some(record) = core
            .peer_records()
            .await
            .into_iter()
            .find(|record| record.agent_did == agent_did && record.is_chat_ready_at(now))
        {
            return Ok(record);
        }
        if Instant::now() >= deadline {
            let records = core.peer_records().await;
            bail!("timed out waiting for enrolled chat-ready route; peers={records:?}");
        }
        sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_for_client_behavior_readiness(core: &ClientCore, agent_did: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let snapshot = core.store().snapshot();
        let has_behavior = snapshot
            .behaviors
            .iter()
            .any(|row| row.agent_did == agent_did);
        let has_readiness = snapshot
            .behavior_readiness
            .iter()
            .any(|row| row.agent_did == agent_did);
        if has_behavior && has_readiness {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for gossiped AgentBehavior/AgentBehaviorReadiness; behaviors={} readiness={}",
                snapshot.behaviors.len(),
                snapshot.behavior_readiness.len()
            );
        }
        sleep(Duration::from_millis(250)).await;
    }
}

async fn query_collection_dids(
    node: &gents::defra_node::EmbeddedNode,
    collection: &str,
) -> Result<Vec<String>> {
    let response = node
        .execute(&format!("{{ {collection} {{ agent_did }} }}"))
        .await;
    if response.has_errors() {
        bail!("query {collection} failed: {:?}", response.errors);
    }
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get(collection))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| {
            row.get("agent_did")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect())
}
