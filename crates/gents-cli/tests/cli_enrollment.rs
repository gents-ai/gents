#[path = "cli_enrollment/contract.rs"]
mod contract;
#[path = "cli_enrollment/household.rs"]
mod household;
#[path = "cli_enrollment/live_delegation.rs"]
mod live_delegation;
#[path = "cli_enrollment/live_goal.rs"]
mod live_goal;
#[path = "cli_enrollment/live_plugin_denial.rs"]
mod live_plugin_denial;
#[path = "cli_enrollment/live_two_agent.rs"]
mod live_two_agent;
mod support;
use support::*;

use std::fs;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use gents::session::canonical_rows::{OutputSegmentRow, TranscriptMessageRow};
use gents_desktop_core::client::canonical_output::{
    project_canonical_message, CanonicalMessageProjection,
};
use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
use gents_desktop_core::local_runtime::fetch_runtime_connection_payload;
use gents_protocol::output::{MessageRole, TerminalOutput};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::time::{sleep, Instant};
use uuid::Uuid;

static ENROLLMENT_E2E_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn enrollment_e2e_lock() -> &'static Mutex<()> {
    ENROLLMENT_E2E_LOCK.get_or_init(|| Mutex::new(()))
}

async fn grant_configured_workspace_root_readers(
    access: &gents::ConfigAccess,
    graphql: &str,
    init: &Value,
    owner_home: &std::path::Path,
    bindings: &[&gents_protocol::agent_identity::AgentIdentityBinding],
) -> Result<()> {
    let node_did = node_did_from_init(init)?;
    let authored_root = init["tool_root"]
        .as_str()
        .context("init has no configured host root")?;
    let root =
        gents::tools::surface::resolve_configured_tool_root(std::path::Path::new(authored_root))?;
    let root = root.to_str().context("configured host root is not UTF-8")?;
    let bootstrap_user = gents_desktop_core::client::PrincipalIdentity::load_existing(
        &DesktopPaths::from_root(owner_home.join("cli")),
    )?;
    anyhow::ensure!(
        bootstrap_user.did() != node_did,
        "workspace root creator must be a distinct User"
    );
    let owner_access = gents::ConfigAccess::Graphql(
        gents::store::config_client::GraphqlEndpoint::as_principal(graphql, bootstrap_user.did())
            .with_client_signing_capability(gents::NodeIdentity::client_signing_capability(
            &bootstrap_user,
        )?)?,
    );
    let query = format!(
        "{{ WorkspaceRoot(filter: {{node_did: {{_eq: \"{}\"}}, root_path: {{_eq: \"{}\"}}}}) {{_docID node_did root_path enabled}} }}",
        escape_graphql_string(&node_did), escape_graphql_string(root),
    );
    let roots = owner_access
        .execute(&query)
        .await
        .context("reading configured WorkspaceRoot as the bootstrap User")?;
    let rows = roots["data"]["WorkspaceRoot"]
        .as_array()
        .context("User cannot observe configured WorkspaceRoot")?;
    let [row] = rows.as_slice() else {
        bail!("configured WorkspaceRoot is unavailable or ambiguous: {roots}")
    };
    anyhow::ensure!(
        row["enabled"] == true,
        "configured WorkspaceRoot is disabled"
    );
    let doc = row["_docID"]
        .as_str()
        .context("WorkspaceRoot has no physical ID")?
        .to_owned();
    let owners = owner_access
        .transact_readonly("test.live_workspace_root_owner", |txn| {
            let doc = &doc;
            Box::pin(async move {
                txn.document_owners("WorkspaceRoot", std::slice::from_ref(doc))
                    .await
                    .map_err(anyhow::Error::from)
            })
        })
        .await
        .context("reading native WorkspaceRoot ownership as the bootstrap User")?;
    anyhow::ensure!(
        owners.len() == 1
            && owners[0].owner.as_ref().map(|did| did.as_str()) == Some(bootstrap_user.did()),
        "WorkspaceRoot was not authored by the bootstrap User"
    );
    let runtime = identity_from_init(init)?;
    for binding in bindings {
        gents::tools::surface::grant_workspace_root_reader(access, root, &binding.agent_did)
            .await
            .with_context(|| {
                format!(
                    "granting configured WorkspaceRoot reader to Agent {} as the enrolled User",
                    binding.agent_did
                )
            })?;
        let agent = gents::store::identity::resolve_agent_identity(&runtime, binding)?;
        let reader = gents::ConfigAccess::Graphql(
            gents::store::config_client::GraphqlEndpoint::as_principal(graphql, &binding.agent_did)
                .with_client_signing_capability(agent.client_signing_capability()?)?,
        );
        let visible = reader.execute(&query).await.with_context(|| {
            format!(
                "reading configured WorkspaceRoot as Agent {}",
                binding.agent_did
            )
        })?;
        let rows = visible["data"]["WorkspaceRoot"]
            .as_array()
            .context("selected Agent cannot read WorkspaceRoot")?;
        anyhow::ensure!(
            rows.len() == 1 && rows[0]["_docID"] == doc,
            "root reader grant did not select the expected document: {visible}"
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_enrollment_from_fresh_desktop_observes_chat_without_client_node() -> Result<()> {
    let _guard = enrollment_e2e_lock().lock().await;

    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("agent-home");
    let desktop_home = tempdir.path().join("desktop-home");
    let owner_home = tempdir.path().join("bootstrap-owner");
    let owner_env = [(
        "GENTS_DESKTOP_HOME",
        owner_home
            .to_str()
            .context("bootstrap owner path is not UTF-8")?,
    )];
    fs::create_dir_all(&home_dir)?;
    let home_arg = home_dir
        .to_str()
        .ok_or_else(|| anyhow!("agent home path is not UTF-8"))?;

    let (model_endpoint, model_name) = resolve_live_inference().await?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let agent_name = format!("cli-enroll-{}", Uuid::new_v4().simple());
    let prompt = format!(
        "Enrollment check {}: in one short sentence, say hello to the newly paired desktop.",
        Uuid::new_v4().simple()
    );

    let init = run_cli_json_with_env(
        &home_dir,
        &[
            "init",
            "--store-key-custody",
            "file",
            "--home",
            home_arg,
            "--node-name",
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
        &owner_env,
    )?;
    let bootstrap_user = gents_desktop_core::client::PrincipalIdentity::load_existing(
        &DesktopPaths::from_root(owner_home.join("cli")),
    )
    .context("loading held bootstrap CLI User")?;
    let node_did = node_did_from_init(&init)?;
    assert_ne!(
        bootstrap_user.did(),
        node_did,
        "bootstrap User and runtime must hold distinct identities"
    );
    let default_agent_did = agent_did_from_init(&init)?;

    let (mut serve, _readiness) =
        spawn_server_with_ready_json(&home_dir, port, &["--home", home_arg], &owner_env)?;
    wait_for_port(port, &mut serve)?;

    serve
        .capturing(async {
            wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;

            let status_url = format!("http://127.0.0.1:{port}");
            let (status, _) = wait_for_enrollment_token(&status_url).await?;
            assert_eq!(
                status.get("node_name").and_then(Value::as_str),
                Some(agent_name.as_str())
            );
            assert_eq!(
                status.get("node_did").and_then(Value::as_str),
                Some(node_did.as_str())
            );

            let core = ClientCore::start_with_paths_and_options(
                DesktopPaths::from_root(&desktop_home),
                ClientCoreOptions::local_only(),
            )
            .await
            .context("starting fresh desktop ClientCore")?;

            let pending = core
                .request_status_enrollment_with_label(&status, Some(&agent_name))
                .await
                .context("requesting status enrollment from /status offer")?;
            assert_eq!(pending.state, "pending_approval");
            assert_eq!(pending.owner_node, node_did);

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

            core.resend_status_enrollment(&pending.request_id)
                .await
                .context("resending the persisted enrollment request")?;
            assert_eq!(
                core.active_status_enrollment_requests()
                    .await?
                    .iter()
                    .filter(|request| request.owner_node == node_did)
                    .count(),
                1,
                "a resend never authors another request"
            );
            wait_for_runtime_enrollment_request(&graphql, &pending.request_id).await?;
            let requests = gents_server::server_host::pending_managed_enrollments(&home_dir, &graphql)
                .await
                .context("listing managed pending enrollments")?;
            let client_did = core.node_identity().did();
            assert_ne!(client_did, node_did);
            assert_ne!(client_did, bootstrap_user.did());
            let matching = requests.iter().filter(|item| item.request_id == pending.request_id).collect::<Vec<_>>();
            let [request] = matching.as_slice() else {
                bail!("expected exactly one pending request for the fresh desktop: {requests:?}");
            };
            assert_eq!(request.owner_node_did, node_did);
            assert_eq!(request.candidate_did, client_did);
            let owner_endpoint = gents::store::config_client::GraphqlEndpoint::as_principal(
                &graphql, bootstrap_user.did(),
            ).with_client_signing_capability(gents::NodeIdentity::client_signing_capability(&bootstrap_user)?)?;
            let owner_access = gents::ConfigAccess::Graphql(owner_endpoint);
            let nodes = owner_access.execute(&format!(
                r#"{{ Node(filter: {{node_did: {{_eq: "{}"}}}}, limit: 2) {{_docID}} }}"#,
                escape_graphql_string(&node_did),
            )).await?;
            let [node] = nodes["data"]["Node"].as_array().context("bootstrap User cannot observe Node")?.as_slice() else {
                bail!("bootstrap User must observe exactly the initialized Node: {nodes}");
            };
            let node_doc = node["_docID"].as_str().context("Node has no physical identity")?.to_owned();
            let owners = owner_access.transact_readonly("test.enrollment.bootstrap_owner", |txn| {
                let node_doc = &node_doc;
                Box::pin(async move { txn.document_owners("Node", std::slice::from_ref(node_doc)).await.map_err(anyhow::Error::from) })
            }).await?;
            anyhow::ensure!(owners.len() == 1 && owners[0].owner.as_ref().map(|did| did.as_str()) == Some(bootstrap_user.did()),
                "the bootstrap CLI User must be the native Node owner");
            let grant_home = home_dir.clone();
            let grant_owner = owner_home.clone();
            let grant_request = pending.request_id.clone();
            let granted = tokio::task::spawn_blocking(move || {
                let home = grant_home.to_str().context("runtime home is not UTF-8")?;
                let owner = grant_owner.to_str().context("bootstrap owner home is not UTF-8")?;
                run_cli_json_with_env(&grant_home,
                    &["p2p", "enrollment", "grant", &grant_request, "--home", home],
                    &[("GENTS_DESKTOP_HOME", owner)],
                )
            }).await.context("joining enrollment grant")?
                .context("granting the fresh desktop User access through the bootstrap User")?;
            assert_eq!(granted["request_id"], pending.request_id);
            assert_eq!(granted["node_did"], node_did);
            assert_eq!(granted["candidate_did"], client_did);
            assert_eq!(granted["node_administrator_granted"], true);
            let outcome = gents_server::server_host::decide_managed_enrollment(
                &home_dir,
                &graphql,
                &pending.request_id,
                gents_server::server_host::ManagedEnrollmentDecision::Approve {
                    lease_seconds: 3600,
                },
            )
            .await
            .context("approving managed enrollment request")?;
            assert_eq!(outcome.request_id, pending.request_id);
            assert_eq!(outcome.state, "approved");
            assert!(!gents_server::server_host::pending_managed_enrollments(&home_dir, &graphql)
                .await?
                .iter()
                .any(|item| item.request_id == pending.request_id));

            let enrolled = wait_for_chat_ready_enrollment(&core, &node_did).await?;
            assert_ne!(
                enrolled.label.as_str(),
                "Enrolled Agent",
                "enrollment should seed the advertised agent name, not a placeholder"
            );
            assert_eq!(enrolled.label, agent_name);
            assert_eq!(enrolled.node_did, node_did);

            // The runtime returns its own Node to an enrolled client; the
            // client authors none, and the client-to-runtime route carries no
            // Node (`scope_templates` conformance).
            let principals = query_collection_dids(core.node(), "Node").await?;
            assert!(
                principals.iter().all(|did| did == &node_did),
                "the client may hold only the runtime's replicated Node; found {principals:?}"
            );
            let snapshot = core.store().snapshot();
            assert!(
                snapshot.nodes.iter().all(|node| node.node_did == node_did),
                "the client store may materialize only the runtime's Node; found {:?}",
                snapshot.nodes
            );

            wait_for_client_node_readiness(&core, &node_did).await?;

            let session_id = Uuid::new_v4().to_string();
            let submitted = core.submit_request(
                &session_id,
                &node_did,
                &prompt,
                Some(&default_agent_did),
            )
            .await
            .context("submitting chat request from the enrolled desktop")?;
            assert_eq!(submitted.node_did, node_did);
            assert_eq!(submitted.session_id, session_id);
            assert_eq!(submitted.agent_did.as_deref(), Some(default_agent_did.as_str()));
            let request_id = submitted.request_id;
            let access = core.operator_access(&node_did)?;
            let runtime_reply = wait_for_complete_agent_response_with_access(
                &access,
                &request_id,
                &session_id,
                Duration::from_secs(240),
            )
            .await?;
            let observed = access.execute(&format!(
                r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}, limit: 2) {{_docID node_did requester_did agent_did session_id content admission_kind admission_signer_did}} }}"#,
                escape_graphql_string(&request_id),
            )).await?;
            let [request] = observed["data"]["AgentRequest"].as_array().context("desktop User cannot observe its request")?.as_slice() else {
                bail!("desktop User must observe one physical request: {observed}");
            };
            anyhow::ensure!(request["node_did"] == node_did && request["requester_did"] == client_did
                && request["agent_did"] == default_agent_did && request["session_id"] == session_id
                && request["content"] == prompt && request["admission_kind"] == "client"
                && request["admission_signer_did"] == client_did && request["_docID"].as_str().is_some(),
                "desktop request lost its signed Node, User, Agent or session provenance: {request}");
            wait_for_client_complete_response(
                &core,
                &session_id,
                &node_did,
                &request_id,
                &runtime_reply,
            )
            .await?;

            let runtime_principals = graphql_query(
                &graphql,
                r#"{ Node { node_did display_name } }"#,
            )
            .await?;
            let runtime_rows = runtime_principals
                .pointer("/data/Node")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            assert!(
                !runtime_rows.is_empty()
                    && runtime_rows.iter().all(|row| {
                        row.get("node_did").and_then(Value::as_str) == Some(node_did.as_str())
                    }),
                "runtime must keep its own Node and receive none from the client: {runtime_principals}"
            );

            let remaining = core
                .active_status_enrollment_requests()
                .await
                .context("loading enrollment requests after pairing")?;
            assert!(
                remaining.is_empty(),
                "accepted pairing must clear the pending enrollment queue so another agent can be added; got {remaining:?}"
            );

            std::sync::Arc::new(core).shutdown().await?;
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
    client_node: &gents::defra_node::Node,
    node_did: &str,
    content: &str,
) -> Result<(String, String, String)> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match graphql_query(
            graphql,
            &format!(
                r#"{{
                    AgentRequest(
                        filter: {{ {} }},
                        order: {{ created_at: DESC }},
                        limit: 1
                    ) {{
                        request_id
                        session_id
                        agent_did
                        content
                    }}
                }}"#,
                gents::session::public_request_filter(&format!(
                    r#"node_did: {{ _eq: "{}" }}, content: {{ _eq: "{}" }}"#,
                    escape_graphql_string(node_did),
                    escape_graphql_string(content),
                )),
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
                        row.get("agent_did")
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
                .execute("{ AgentRequest { request_id node_did content requester_did } }")
                .await;
            let runtime_all = graphql_query(
                graphql,
                "{ AgentRequest { request_id node_did content requester_did } }",
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

async fn wait_for_complete_agent_response(
    graphql: &str,
    request_id: &str,
    session: &str,
    timeout: Duration,
) -> Result<RuntimeTerminalReply> {
    let access = gents::ConfigAccess::Graphql(crate::support::graphql::served_endpoint(graphql));
    wait_for_complete_agent_response_with_access(&access, request_id, session, timeout).await
}

async fn wait_for_complete_agent_response_with_access(
    access: &gents::ConfigAccess,
    request_id: &str,
    session: &str,
    timeout: Duration,
) -> Result<RuntimeTerminalReply> {
    let deadline = Instant::now() + timeout;
    let mut empty_terminal_since = None::<Instant>;
    loop {
        let response = access
            .execute(&format!(
                r#"{{
                    AgentRequest(
                        filter: {{
                            request_id: {{ _eq: "{}" }},
                            session_id: {{ _eq: "{}" }}
                        }},
                        order: {{ created_at: DESC }},
                        limit: 1
                    ) {{
                        _docID node_did requester_did session_id request_id
                        lifecycle_state failure_reason terminal_output
                    }}
                }}"#,
                escape_graphql_string(request_id),
                escape_graphql_string(session),
            ))
            .await?;
        if let Ok(row) = first_graphql_row(&response, "AgentRequest") {
            let request: gents_protocol::row::AgentRequestRow = serde_json::from_value(row.clone())
                .context("decoding canonical AgentRequest row")?;
            anyhow::ensure!(
                request.session_id.as_deref() == Some(session)
                    && request
                        .node_did
                        .as_deref()
                        .is_some_and(|did| !did.is_empty())
                    && request
                        .requester_did
                        .as_deref()
                        .is_some_and(|did| !did.is_empty()),
                "canonical AgentRequest row lost requester/session lineage for {request_id}: {row}"
            );
            if request
                .lifecycle_state
                .is_some_and(|state| state.as_str() == "failed")
            {
                bail!(
                    "live inference failed for {request_id}: failure_reason={:?}; row={row}",
                    request.failure_reason
                );
            }
            if request.is_terminal() {
                let output = gents::session::observe_request_output(access, &request).await?;
                let visible = match &output {
                    gents::session::CanonicalRequestOutput::TerminalMessage {
                        header,
                        presentation,
                        ..
                    } => {
                        anyhow::ensure!(
                            header.request_doc_id.as_deref() == request.doc_id.as_deref()
                                && header.session_id == session,
                            "canonical terminal header does not match the physical request for {request_id}: header={header:?}; row={row}"
                        );
                        Some(presentation.body_markdown.clone())
                    }
                    gents::session::CanonicalRequestOutput::TerminalNoMessage => bail!(
                        "live inference completed without a canonical terminal message for {request_id}: {output:?}"
                    ),
                    gents::session::CanonicalRequestOutput::Denied
                    | gents::session::CanonicalRequestOutput::Invalid
                    | gents::session::CanonicalRequestOutput::Conflicted
                    | gents::session::CanonicalRequestOutput::Retracted => bail!(
                        "canonical terminal output is not presentable for {request_id}: {output:?}"
                    ),
                    _ => None,
                };
                if let Some(visible) = visible.filter(|text| !text.trim().is_empty()) {
                    let Some(TerminalOutput::Message { message_doc_id }) =
                        request.terminal_output.clone()
                    else {
                        bail!("terminal message output lost its header identity for {request_id}: {row}");
                    };
                    return Ok(RuntimeTerminalReply {
                        header_doc_id: message_doc_id,
                        text: visible,
                    });
                }
                if empty_terminal_since
                    .get_or_insert_with(Instant::now)
                    .elapsed()
                    >= Duration::from_secs(15)
                {
                    bail!(
                        "live inference produced no visible canonical terminal content for {request_id} within 15s of the first terminal observation: {row}; output={output:?}"
                    );
                }
            }
        }
        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for canonical terminal output with visible text for {request_id}; last={response}"
            );
        }
        sleep(Duration::from_millis(250)).await;
    }
}

#[derive(Debug, Clone)]
struct RuntimeTerminalReply {
    header_doc_id: String,
    text: String,
}

fn client_selected_reply(
    requests: &[AgentRequestRow],
    messages: &[TranscriptMessageRow],
    segments: &[OutputSegmentRow],
    request_id: &str,
    header_doc_id: &str,
) -> Option<String> {
    let request = requests.iter().find(|row| {
        row.request_id == request_id
            && row.lifecycle_state == Some(RequestLifecycleState::Completed)
            && matches!(
                &row.terminal_output,
                Some(TerminalOutput::Message { message_doc_id }) if message_doc_id == header_doc_id
            )
    })?;
    let header = messages.iter().find(|row| {
        row.doc_id == header_doc_id
            && row.message.role == MessageRole::Assistant
            && row.message.request_doc_id.is_some()
            && row.message.request_doc_id == request.doc_id
    })?;
    match project_canonical_message(header, segments, &[], &[]) {
        CanonicalMessageProjection::Ready(message) => {
            Some(gents_protocol::transcript::present_message(&message).body_markdown)
        }
        _ => None,
    }
}

async fn wait_for_client_complete_response(
    core: &ClientCore,
    session_id: &str,
    node_did: &str,
    request_id: &str,
    runtime_reply: &RuntimeTerminalReply,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let page = gents_desktop_core::client::load_session_transcript_page_on(
            &core.operator_access(node_did)?,
            session_id,
            Some(node_did),
            Some(core.node_identity().did()),
            None,
            Some(40),
        )
        .await?;
        let page_request_doc_ids = page
            .store
            .transcript_messages
            .iter()
            .filter_map(|row| row.message.request_doc_id.clone())
            .collect::<Vec<_>>();
        core.refresh_local_request(
            node_did,
            session_id,
            Some(request_id),
            None,
            &page_request_doc_ids,
        )
        .await?;
        let snapshot = core.store().snapshot();
        let client_reply = client_selected_reply(
            &snapshot.requests,
            &page.store.transcript_messages,
            &page.store.output_segments,
            request_id,
            &runtime_reply.header_doc_id,
        );
        if client_reply.as_deref() == Some(runtime_reply.text.as_str()) {
            return Ok(());
        }
        if snapshot.requests.iter().any(|row| {
            row.request_id == request_id
                && row
                    .lifecycle_state
                    .is_some_and(|state| state.as_str() == "failed")
        }) {
            bail!(
                "client received a failed request for {request_id}: {:?}",
                snapshot.requests
            );
        }
        if Instant::now() >= deadline {
            bail!(
                "client never reconstructed the runtime-selected terminal header for {request_id} ({runtime_reply:?}); client_reply={client_reply:?}; requests={:?}; transcript_messages={:?}",
                snapshot.requests,
                snapshot.transcript_messages
            );
        }
        sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_for_chat_ready_enrollment(
    core: &ClientCore,
    node_did: &str,
) -> Result<gents_desktop_core::client::PeerRecord> {
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut updates = core.sync_state_updates();
    loop {
        let now = Utc::now();
        if let Some(record) = core
            .peer_records()
            .await
            .into_iter()
            .find(|record| record.node_did == node_did && record.is_chat_ready_at(now))
        {
            return Ok(record);
        }
        if Instant::now() >= deadline {
            let records = core.peer_records().await;
            bail!("timed out waiting for enrolled chat-ready route; peers={records:?}");
        }
        match tokio::time::timeout(
            deadline.saturating_duration_since(Instant::now()),
            updates.changed(),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(_)) => bail!("client sync-state channel closed"),
            Err(_) => {
                let records = core.peer_records().await;
                bail!("timed out waiting for enrolled chat-ready route; peers={records:?}");
            }
        }
    }
}

async fn wait_for_client_node_readiness(core: &ClientCore, node_did: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut updates = core.store_change_updates();
    loop {
        let snapshot = core.store().snapshot();
        let has_agent = snapshot.agents.iter().any(|row| row.node_did == node_did);
        let has_readiness = snapshot
            .node_readiness
            .iter()
            .any(|row| row.node_did == node_did);
        if has_agent && has_readiness {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for authenticated Agent/NodeReadiness observation; agents={} readiness={}",
                snapshot.agents.len(),
                snapshot.node_readiness.len()
            );
        }
        match tokio::time::timeout(
            deadline.saturating_duration_since(Instant::now()),
            updates.changed(),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(_)) => bail!("app projection channel closed"),
            Err(_) => {
                bail!(
                    "timed out waiting for authenticated Agent/NodeReadiness observation; agents={} readiness={}",
                    snapshot.agents.len(),
                    snapshot.node_readiness.len()
                );
            }
        }
    }
}

async fn query_collection_dids(
    node: &gents::defra_node::Node,
    collection: &str,
) -> Result<Vec<String>> {
    let response = node
        .execute(&format!("{{ {collection} {{ node_did }} }}"))
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
            row.get("node_did")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect())
}

#[test]
fn client_reply_requires_the_runtime_selected_terminal_header() {
    use gents_protocol::output::{
        MessageBlock, MessagePublication, OutputOutcome, OutputSegment, OutputSource, OutputWriter,
        PayloadPresentation, PayloadRef, PresentedPayload, SegmentRun, SourceClose,
        StreamDeclaration, StreamPayload, TranscriptMessage,
    };
    const REQUEST_DOC: &str = "bae-request";
    const TEXT: &str = "hello";
    let segment = |doc_id: &str, key: &str| OutputSegmentRow {
        doc_id: doc_id.into(),
        segment: OutputSegment {
            node_did: "did:key:agent".into(),
            requester_did: None,
            session_id: "session".into(),
            request_doc_id: REQUEST_DOC.into(),
            source: OutputSource::Authored { key: key.into() },
            writer: OutputWriter::RequestExecution {
                execution_generation: "generation".into(),
            },
            ordinal: Some(0),
            runs: vec![SegmentRun {
                stream: 0,
                bytes: TEXT.len() as u32,
                declaration: Some(StreamDeclaration {
                    block_index: 0,
                    part_index: 0,
                    payload: StreamPayload::Text,
                }),
            }],
            payload: TEXT.into(),
            close: Some(SourceClose::Closed {
                outcome: OutputOutcome::Complete,
                segments: 1,
                stream_bytes: vec![TEXT.len() as u64],
            }),
            created_at: "2026-09-25T00:00:00Z".into(),
        },
    };
    let header =
        |doc_id: &str, close_doc_id: &str, sequence: u32, role: MessageRole| TranscriptMessageRow {
            doc_id: doc_id.into(),
            message: TranscriptMessage {
                native_request_id: None,
                message_key: format!("session:{sequence}"),
                session_id: "session".into(),
                node_did: "did:key:agent".into(),
                requester_did: None,
                request_doc_id: Some(REQUEST_DOC.into()),
                publication: MessagePublication::RequestExecution {
                    execution_generation: "generation".into(),
                },
                outcome: OutputOutcome::Complete,
                sequence,
                role,
                native_id: None,
                blocks: vec![MessageBlock::Text {
                    text: PresentedPayload {
                        output: PayloadRef {
                            close_doc_id: close_doc_id.into(),
                            stream: 0,
                        },
                        presentation: PayloadPresentation::Full,
                    },
                }],
                created_at: "2026-09-25T00:00:00Z".into(),
            },
        };
    let request = |state: RequestLifecycleState, terminal: Option<&str>| AgentRequestRow {
        doc_id: Some(REQUEST_DOC.into()),
        request_id: "request".into(),
        lifecycle_state: Some(state),
        terminal_output: terminal.map(|message_doc_id| TerminalOutput::Message {
            message_doc_id: message_doc_id.into(),
        }),
        ..Default::default()
    };
    let prompt = header("bae-prompt", "bae-prompt-close", 1, MessageRole::User);
    let earlier = header(
        "bae-earlier",
        "bae-earlier-close",
        2,
        MessageRole::Assistant,
    );
    let selected = header(
        "bae-selected",
        "bae-selected-close",
        3,
        MessageRole::Assistant,
    );
    let segments = vec![
        segment("bae-prompt-close", "prompt"),
        segment("bae-earlier-close", "earlier"),
        segment("bae-selected-close", "selected"),
    ];
    let completed = [request(
        RequestLifecycleState::Completed,
        Some("bae-selected"),
    )];
    let replicated_prefix = [prompt.clone(), earlier.clone()];
    for (label, requests, messages) in [
        (
            "prompt and earlier turn only",
            &completed[..],
            &replicated_prefix[..],
        ),
        (
            "request not yet completed",
            &[request(RequestLifecycleState::Processing, None)][..],
            &[prompt.clone(), earlier.clone(), selected.clone()][..],
        ),
    ] {
        assert_eq!(
            client_selected_reply(requests, messages, &segments, "request", "bae-selected"),
            None,
            "{label} must not satisfy the client wait"
        );
    }
    let all = [prompt, earlier, selected];
    assert_eq!(
        client_selected_reply(&completed, &all, &segments[..2], "request", "bae-selected"),
        None,
        "the selected header must reconstruct Ready, not just exist"
    );
    assert_eq!(
        client_selected_reply(&completed, &all, &segments, "request", "bae-selected").as_deref(),
        Some(TEXT)
    );
}
