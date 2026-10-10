mod support;
use support::graphql::graphql_mutation_with_variables;
use support::*;

use std::fs;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use gents::default_agent_id_for_node;
use gents::NodeIdentity as _;
use serde_json::{json, Value};
use uuid::Uuid;

fn generated_tools_id_for_agent(node_did: &str) -> String {
    let default_agent_id = default_agent_id_for_node(node_did);
    format!("{default_agent_id}-tools")
}

fn find_snapshot_row<'a>(
    snapshot: &'a Value,
    collection: &str,
    key: &str,
    expected: &str,
) -> Result<&'a Value> {
    snapshot
        .get(collection)
        .and_then(Value::as_array)
        .and_then(|rows| {
            rows.iter()
                .find(|row| row.get(key).and_then(Value::as_str) == Some(expected))
        })
        .ok_or_else(|| anyhow!("missing {collection} row with {key}={expected}: {snapshot}"))
}

async fn wait_for_inference_call_state(
    graphql: &str,
    request_id: &str,
    expected_state: &str,
) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let response = graphql_query(
            graphql,
            &format!(
                r#"{{
                    InferenceCall(
                        filter: {{ request_id: {{ _eq: "{}" }} }},
                        order: {{ call_seq: ASC }}
                    ) {{
                        request_id
                        backend_id
                        agent_id
                        call_state
                    }}
                }}"#,
                escape_graphql_string(request_id),
            ),
        )
        .await?;
        let rows = response
            .pointer("/data/InferenceCall")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if let Some(row) = rows
            .iter()
            .find(|row| row.get("call_state").and_then(Value::as_str) == Some(expected_state))
        {
            return Ok(row.clone());
        }

        if Instant::now() >= deadline {
            return Err(anyhow!(
                "timed out waiting for InferenceCall request_id={request_id} call_state={expected_state}; last rows={}",
                Value::Array(rows)
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn active_inference_calls_for_backend(graphql: &str, backend_id: &str) -> Result<Vec<Value>> {
    let response = graphql_query(
        graphql,
        &format!(
            r#"{{
                InferenceCall(
                    filter: {{ backend_id: {{ _eq: "{}" }} }},
                    order: {{ call_seq: ASC }}
                ) {{
                    request_id
                    backend_id
                    agent_id
                    call_state
                }}
            }}"#,
            escape_graphql_string(backend_id),
        ),
    )
    .await?;
    Ok(response
        .pointer("/data/InferenceCall")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|row| {
            matches!(
                row.get("call_state").and_then(Value::as_str),
                Some("running" | "queued")
            )
        })
        .collect())
}

fn count_inference_calls(rows: &[Value], agent_id: Option<&str>, call_state: &str) -> i64 {
    rows.iter()
        .filter(|row| {
            row.get("call_state").and_then(Value::as_str) == Some(call_state)
                && agent_id.is_none_or(|expected| {
                    row.get("agent_id").and_then(Value::as_str) == Some(expected)
                })
        })
        .count() as i64
}

fn wait_for_server_exit(
    serve: &mut ServeProcess,
    timeout: Duration,
) -> Result<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = serve.child.try_wait().context("checking server exit")? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            let (stdout, stderr) = serve.captured_output()?;
            return Err(anyhow!(
                "server did not exit within {timeout:?}\nstdout:\n{stdout}\nstderr:\n{stderr}"
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The per-user supervisor sends SIGTERM to the foreground `gents server`
/// process. This deliberately signals the exact fixture PID rather than
/// dropping its handle (which would use the test fixture's abrupt SIGKILL
/// cleanup) so success proves the server's SIGTERM handler ran its shutdown
/// epilogue.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_sigterm_runs_the_graceful_shutdown_path() -> Result<()> {
    use std::os::unix::process::ExitStatusExt as _;

    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let agent_name = format!("cli-sigterm-{}", Uuid::new_v4().simple());

    // Point the normal runtime configuration at an unopened local port: this
    // is a fully initialized but provider-free/degraded runtime. The test is
    // about owned-loop shutdown, not inference.
    run_init_json(
        &home_dir,
        &[
            "--node-name",
            &agent_name,
            "--inference-url",
            "http://127.0.0.1:9/v1",
            "--model-name",
            "sigterm-no-provider",
        ],
    )?;
    let port = allocate_port()?;
    let (mut serve, readiness) =
        spawn_server_with_ready_json(&home_dir, port, &["--p2p-transport", "none"], &[])?;
    assert_eq!(
        readiness.get("status").and_then(Value::as_str),
        Some("serving"),
        "server must be fully ready before SIGTERM: {readiness}"
    );

    let pid = serve.child.id();
    let signal = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .context("sending SIGTERM to the exact gents server fixture PID")?;
    assert!(signal.success(), "kill -TERM {pid} failed: {signal}");

    let status = wait_for_server_exit(&mut serve, Duration::from_secs(15))?;
    assert!(
        status.success(),
        "SIGTERM must be handled by the server and exit successfully, not terminate the fixture by signal: {status}"
    );
    assert_eq!(
        status.signal(),
        None,
        "server exited because of a signal instead of completing its graceful SIGTERM shutdown: {status}"
    );
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", port)).is_err(),
        "the terminated foreground runtime still owns its HTTP port"
    );
    Ok(())
}
/// An error returned after the runtime is spawned must still run the runtime's
/// shutdown epilogue. A closed stdout makes the post-readiness report write
/// fail; the durable readiness row must then end at `shutdown`, not stay
/// `ready` as it does when the runtime task is dropped with the process.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_error_after_runtime_spawn_drains_the_runtime() -> Result<()> {
    use gents::defra_node::{EmbeddedNode, StorageBackend};
    use gents_protocol::row::{
        decode_node_readiness_snapshot, NodeReadinessProcessState, NodeReadinessRow,
    };
    use std::process::Stdio;

    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let agent_name = format!("cli-broken-stdout-{}", Uuid::new_v4().simple());
    let init = run_init_json(
        &home_dir,
        &[
            "--node-name",
            &agent_name,
            "--inference-url",
            "http://127.0.0.1:9/v1",
            "--model-name",
            "broken-stdout-no-provider",
        ],
    )?;
    let node_did = node_did_from_init(&init)?;

    let port = allocate_port()?;
    let stderr_log = tempfile::NamedTempFile::new().context("creating gents stderr log")?;
    let mut command = Command::new(cli_bin());
    command
        .env("HOME", &home_dir)
        .env("RUST_LOG", "error")
        .current_dir(&home_dir)
        .args(["server", "--http-port", &port.to_string()])
        .args(["--no-codex-shim", "--p2p-transport", "none"])
        .stdout(Stdio::piped())
        .stderr(Stdio::from(
            stderr_log.reopen().context("opening gents stderr log")?,
        ));
    support::process::configure_foreground_server_env(&mut command, &[]);
    let mut child = command.spawn().context("spawning gents server")?;
    // Close the only reader so the readiness report hits a broken pipe.
    drop(child.stdout.take());
    let mut serve = ServeProcess {
        child,
        stdout_log: None,
        stderr_log: Some(stderr_log),
    };

    let status = wait_for_server_exit(&mut serve, Duration::from_secs(60))?;
    let (_, stderr) = serve.captured_output()?;
    assert_eq!(
        status.code(),
        Some(1),
        "a failed readiness report must be a returned error, not a panic or signal: {status}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("writing JSON report to stdout"),
        "server exited without the report write failure:\n{stderr}"
    );

    let data_dir = home_dir.join(".gents").join("data");
    let node = gents::store_key::open_home_store_key(&home_dir.join(".gents"), &data_dir)
        .await?
        .encrypt(
            EmbeddedNode::builder()
                .data_path(&data_dir)
                .with_storage_backend(StorageBackend::Regolith),
        )
        .build()
        .await
        .with_context(|| format!("opening embedded node at {}", data_dir.display()))?;
    let response = node
        .execute(&format!(
            r#"{{ NodeReadiness(filter: {{ node_did: {{ _eq: "{}" }} }}, limit: 1) {{
                node_did snapshot_json updated_at
            }} }}"#,
            escape_graphql_string(&node_did),
        ))
        .await;
    node.shutdown().await;
    anyhow::ensure!(
        !response.has_errors(),
        "reading agent readiness: {:?}",
        response.errors
    );
    let row = response
        .data
        .as_ref()
        .and_then(|data| data["NodeReadiness"].get(0).cloned())
        .context("server left no agent readiness row")?;
    let row: NodeReadinessRow = serde_json::from_value(row)?;
    let snapshot = decode_node_readiness_snapshot(&row, &node_did)
        .map_err(|reason| anyhow!("undecodable agent readiness: {reason:?}"))?;
    assert_eq!(
        snapshot.process_state,
        NodeReadinessProcessState::Shutdown,
        "the runtime was not drained after the post-spawn error\nstderr:\n{stderr}"
    );
    Ok(())
}

// Covers OS process loading and Tokio startup under concurrent builds. The
// port-zero and occupied-port checks still reject before readiness is emitted.
const SERVER_REJECTION_EXIT_TIMEOUT: Duration = Duration::from_secs(30);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_rejects_ephemeral_http_port_before_publishing_readiness() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let mut serve = spawn_server(&home_dir, 0)?;
    // This is a process-startup budget, not an HTTP/readiness deadline. On
    // macOS the instrumented CLI can spend several seconds in _dyld_start
    // before main executes. Keep the semantic checks below (rejection with an
    // actionable error and no published readiness) independent of that delay.
    let status = wait_for_server_exit(&mut serve, SERVER_REJECTION_EXIT_TIMEOUT)?;
    let (stdout, stderr) = serve.captured_output()?;

    assert!(!status.success(), "server unexpectedly exited successfully");
    assert!(
        !stdout.contains("\"status\": \"serving\""),
        "server published readiness for an unknowable ephemeral port:\n{stdout}"
    );
    assert!(
        stderr.contains("--http-port 0 is not supported")
            && stderr.contains("choose an explicit non-zero port"),
        "missing actionable ephemeral-port diagnostic:\n{stderr}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_fails_closed_when_http_port_is_occupied() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-bind-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = format!("cli-bind-{}", Uuid::new_v4().simple());
    run_init_json(
        &home_dir,
        &[
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;

    let listener =
        std::net::TcpListener::bind(("127.0.0.1", 0)).context("reserving occupied HTTP port")?;
    let port = listener.local_addr()?.port();
    let mut serve = spawn_server(&home_dir, port)?;
    let status = wait_for_server_exit(&mut serve, SERVER_REJECTION_EXIT_TIMEOUT)?;
    let (stdout, stderr) = serve.captured_output()?;

    assert!(!status.success(), "server unexpectedly exited successfully");
    assert!(
        !stdout.contains("\"status\": \"serving\""),
        "server published readiness after bind failure:\n{stdout}"
    );
    assert!(
        stderr.contains("embedded HTTP listener cannot bind")
            && stderr.contains(&format!("127.0.0.1:{port}")),
        "missing actionable bind diagnostic:\n{stderr}"
    );
    drop(listener);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ready_json_recovers_when_a_foreign_listener_holds_the_allocated_port() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-stolen-port-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = format!("cli-stolen-port-{}", Uuid::new_v4().simple());
    run_init_json(
        &home_dir,
        &[
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;

    let stolen = allocate_port()?;
    // Hold the reserved port for the whole test. The child's preflight bind
    // keeps losing it, so a readiness JSON can only come from a replacement
    // port -- a TCP-connect check would instead accept this listener and
    // report the stolen port as ready.
    let thief =
        std::net::TcpListener::bind(("127.0.0.1", stolen)).context("holding the test port")?;

    let (_serve, bound, readiness) = spawn_server_with_ready_json_recovering(
        &home_dir,
        stolen,
        &["--p2p-transport", "none"],
        &[],
    )?;

    assert_ne!(
        bound, stolen,
        "harness reported the port a foreign listener still holds"
    );
    let expected_graphql = graphql_url(bound);
    assert_eq!(
        readiness.get("graphql").and_then(Value::as_str),
        Some(expected_graphql.as_str()),
        "readiness must come from the child on its replacement port: {readiness}"
    );
    drop(thief);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ready_json_without_recovery_still_fails_on_a_held_port() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-held-port-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = format!("cli-held-port-{}", Uuid::new_v4().simple());
    run_init_json(
        &home_dir,
        &[
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;

    let held = allocate_port()?;
    let thief =
        std::net::TcpListener::bind(("127.0.0.1", held)).context("holding the test port")?;

    let error = spawn_server_with_ready_json(&home_dir, held, &["--p2p-transport", "none"], &[])
        .err()
        .ok_or_else(|| anyhow!("the non-recovering spawn must fail while the port is held"))?;

    let message = format!("{error:#}");
    assert!(
        message.contains("embedded HTTP listener cannot bind"),
        "must surface the child's own bind diagnostic, not a replacement port:\n{message}"
    );
    drop(thief);
    Ok(())
}

/// A Secure Enclave key cannot own the served node's access control, so
/// `gents server` refuses the home before building a node, naming the
/// backend and the remedy.
#[test]
fn server_refuses_a_secure_enclave_home_before_building_a_node() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let gents_home = home_dir.join(".gents");
    fs::create_dir_all(&gents_home)?;
    fs::write(
        gents_home.join("init.json"),
        serde_json::json!({
            "home": gents_home,
            "node_name": "enclave",
            "node_did": "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK",
            "key_path": null,
            "identity_backend": "macos-secure-enclave",
            "secure_enclave_label": "gents-test-enclave",
            "tool_ceiling": "Readonly",
            "tool_root": null,
        })
        .to_string(),
    )?;
    let port = allocate_port()?;
    let stderr = run_cli_failure_stderr(
        &home_dir,
        &[
            "server",
            "--http-port",
            &port.to_string(),
            "--no-codex-shim",
        ],
    )?;
    assert!(stderr.contains("macos-secure-enclave"), "{stderr}");
    assert!(stderr.contains("--identity-backend file"), "{stderr}");
    let store_entries = fs::read_dir(gents_home.join("data"))
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(store_entries, 0, "no store was opened: {stderr}");
    Ok(())
}

/// `gents server` turns on DefraDB node access control owned by the home's
/// principal: anonymous HTTP writes are refused, the principal's signed
/// writes and the CLI's P2P administration through the runtime state are
/// admitted, anonymous reads and `/self` and `/sessions` keep working, and a
/// restart of the same store keeps all of it. `/mcp` is covered by
/// `mcp_endpoint_serves_defra_query` on the same served-home setup.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn served_home_admits_only_its_principal_over_http() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let model_name = format!("mock-nac-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = format!("cli-nac-{}", Uuid::new_v4().simple());
    run_init_json(
        &home_dir,
        &[
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;

    for phase in ["first", "restarted"] {
        let (serve, port, _) =
            spawn_server_with_ready_json_recovering(&home_dir, allocate_port()?, &[], &[])?;
        let graphql = graphql_url(port);
        let mutation = format!(
            r#"mutation {{ create_CompactionEntry(input: {{ compaction_key: "nac-{phase}", node_did: "did:test:nac", session_id: "nac-session", sequence: 1, original_tokens: 2, compacted_tokens: 1, created_at: "2026-06-02T10:00:00Z" }}) {{ _docID }} }}"#
        );
        let refused = gents::config_client::ConfigAccess::graphql(graphql.clone())
            .write("test.nac.anonymous", &mutation)
            .await
            .expect_err("anonymous HTTP writes must be refused");
        assert!(
            refused.to_string().contains("not authorized"),
            "{phase}: unexpected refusal: {refused:#}"
        );
        gents::config_client::ConfigAccess::Graphql(support::graphql::served_endpoint(&graphql))
            .write("test.nac.signed", &mutation)
            .await
            .with_context(|| format!("{phase}: principal-signed HTTP write"))?;
        let replicators = run_cli_json(&home_dir, &["p2p", "admin", "replicators", "list"])
            .with_context(|| format!("{phase}: CLI P2P administration over HTTP"))?;
        assert_eq!(replicators["status"], "ok", "{phase}: {replicators}");
        // Reads stay anonymous: DefraDB's HTTP server does not gate them, and
        // the runtime's unauthenticated read surfaces read as anonymous.
        let rows = gents::config_client::ConfigAccess::graphql(graphql.clone())
            .execute(r#"{ CompactionEntry(filter: { node_did: { _eq: "did:test:nac" } }) { compaction_key } }"#)
            .await
            .with_context(|| format!("{phase}: anonymous HTTP read"))?;
        assert!(
            rows["data"]["CompactionEntry"]
                .as_array()
                .is_some_and(|rows| rows
                    .iter()
                    .any(|row| row["compaction_key"] == format!("nac-{phase}"))),
            "{phase}: {rows}"
        );
        for surface in ["self", "sessions"] {
            let response = reqwest::Client::new()
                .get(format!("http://127.0.0.1:{port}/{surface}"))
                .send()
                .await?;
            assert!(
                response.status().is_success(),
                "{phase}: /{surface} returned {}",
                response.status()
            );
        }
        drop(serve);
    }
    Ok(())
}

#[test]
fn port_replacement_requires_address_in_use_for_the_requested_address() {
    let addr = "127.0.0.1:20001";
    let context = "embedded HTTP listener cannot bind to 127.0.0.1:20001; if another Gents \
                   runtime is serving there, stop it or pass --http-port";

    assert!(support::process::is_address_in_use(
        &format!("Error: {context}\n\nCaused by:\n    Address already in use (os error 48)\n"),
        addr
    ));
    // serve.rs attaches the same context to every bind failure; only
    // address-in-use may be recovered, the rest must fail unchanged.
    assert!(!support::process::is_address_in_use(
        &format!("Error: {context}\n\nCaused by:\n    Permission denied (os error 13)\n"),
        addr
    ));
    assert!(!support::process::is_address_in_use(
        "Error: embedded HTTP listener cannot bind to 127.0.0.1:20002\n\nCaused by:\n    \
         Address already in use (os error 48)\n",
        addr
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fresh_home_apply_root_precedes_grok_agent_binding() -> Result<()> {
    const GROK_BEHAVIOR: &str = "port-live";
    const APPLIED_BACKEND: &str = "fresh-applied-grok-backend";

    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("pack");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-grok-apply-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = format!("cli-grok-apply-{}", Uuid::new_v4().simple());
    run_init_json(
        &home_dir,
        &[
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;

    // Export the freshly initialized home as a self-contained pack, then add
    // the agent that only --apply-root can make available to this server
    // invocation. The home itself deliberately still has no port-live row.
    run_cli_text(
        &home_dir,
        &["config", "export", "--root", &root.to_string_lossy()],
    )?;

    // The applied agent uses a backend that does not exist when the
    // runtime's recurring prober takes its immediate startup tick. Its
    // exported runtime-owned health fields are deliberately absent, so the
    // post-apply path must probe and promote it before readiness can publish.
    let config_path = root.join("pack_config.json");
    let mut config = read_json_file(&config_path)?;
    let mut backend = config["inference_backends"][0].clone();
    backend["backend_id"] = Value::String(APPLIED_BACKEND.to_string());
    if let Some(object) = backend.as_object_mut() {
        object.remove("probe_status");
        object.remove("last_probe");
    }
    config["inference_backends"]
        .as_array_mut()
        .context("inference_backends is not an array")?
        .push(backend);
    let mut profile = config["inference_profiles"][0].clone();
    let applied_profile = format!("{APPLIED_BACKEND}:profile");
    profile["profile_id"] = Value::String(applied_profile.clone());
    profile["backend_id"] = Value::String(APPLIED_BACKEND.to_string());
    config["inference_profiles"]
        .as_array_mut()
        .context("inference_profiles is not an array")?
        .push(profile);
    let mut agent = config["agents"][0].clone();
    agent["agent_id"] = Value::String(GROK_BEHAVIOR.to_string());
    agent["inference_profile_id"] = Value::String(applied_profile);
    config["agents"]
        .as_array_mut()
        .context("agents is not an array")?
        .push(agent);
    write_json_file(&config_path, &config)?;

    let port = allocate_port()?;
    let socket_path = tempdir.path().join("grok.sock");
    let (mut serve, readiness) = spawn_server_with_ready_json(
        &home_dir,
        port,
        &[
            "--p2p-transport",
            "none",
            "--apply-root",
            root.to_str().context("pack root path is not UTF-8")?,
            "--grok-shim",
            "--grok-shim-socket-path",
            socket_path.to_str().context("socket path is not UTF-8")?,
            "--grok-shim-agent-id",
            GROK_BEHAVIOR,
        ],
        &[],
    )?;
    wait_for_port(port, &mut serve)?;
    let (_stdout, stderr) = serve.captured_output()?;

    assert_eq!(
        readiness.pointer("/apply_root/ok").and_then(Value::as_bool),
        Some(true),
        "the pack must apply before readiness: {readiness}"
    );
    assert_eq!(
        readiness
            .pointer("/grok_shim/bound")
            .and_then(Value::as_bool),
        Some(true),
        "the agent supplied by --apply-root must bind in the same invocation: {readiness}; stderr: {stderr}"
    );
    assert_eq!(
        readiness
            .pointer("/grok_shim/socket")
            .and_then(Value::as_str),
        socket_path.to_str()
    );
    assert!(
        socket_path.exists(),
        "the bound Grok leader must publish its socket"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_exposes_prometheus_metrics_endpoint() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-metrics-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let agent_name = format!("cli-metrics-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);

    let init = run_init_json(
        &home_dir,
        &[
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let node_did = node_did_from_init(&init)?;
    let default_agent_id = default_agent_id_for_node(&node_did);

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;

    let client = reqwest::Client::new();

    let version_response = client
        .get(format!("http://127.0.0.1:{port}/version"))
        .send()
        .await
        .context("fetching /version")?;
    assert!(
        version_response.status().is_success(),
        "unexpected /version status: {version_response:?}"
    );
    let version: Value = version_response
        .json()
        .await
        .context("reading /version body")?;
    assert_eq!(
        version.get("service").and_then(Value::as_str),
        Some("gents")
    );
    assert_eq!(version.get("binary").and_then(Value::as_str), Some("gents"));
    assert_eq!(
        version.get("package").and_then(Value::as_str),
        Some("gents-cli")
    );
    assert_eq!(
        version.get("version").and_then(Value::as_str),
        Some(env!("CARGO_PKG_VERSION"))
    );
    assert!(
        version.get("build").and_then(Value::as_object).is_some(),
        "expected build metadata in /version body: {version}"
    );

    let health_response = client
        .get(format!("http://127.0.0.1:{port}/healthz"))
        .send()
        .await
        .context("fetching /healthz")?;
    assert!(
        health_response.status().is_success(),
        "unexpected /healthz status: {health_response:?}"
    );
    let health: Value = health_response
        .json()
        .await
        .context("reading /healthz body")?;
    assert_eq!(health.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(health.get("status").and_then(Value::as_str), Some("ok"));
    assert_eq!(health.get("service").and_then(Value::as_str), Some("gents"));
    assert_eq!(
        health
            .pointer("/checks/runtime/ready")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert!(
        health
            .get("runtimes")
            .and_then(Value::as_array)
            .is_some_and(|runtimes| runtimes.iter().any(|runtime| {
                runtime.get("node_did").and_then(Value::as_str) == Some(node_did.as_str())
            })),
        "expected runtime row for {node_did} in /healthz body: {health}"
    );

    let status_response = client
        .get(format!("http://127.0.0.1:{port}/status"))
        .send()
        .await
        .context("fetching /status")?;
    assert!(
        status_response.status().is_success(),
        "unexpected /status response: {status_response:?}"
    );
    let status: Value = status_response
        .json()
        .await
        .context("reading /status body")?;
    assert_eq!(
        status.get("node_name").and_then(Value::as_str),
        Some(agent_name.as_str())
    );
    assert_eq!(
        status.get("node_did").and_then(Value::as_str),
        Some(node_did.as_str())
    );
    assert_eq!(
        status.get("graphql").and_then(Value::as_str),
        Some(graphql.as_str())
    );
    assert!(
        matches!(
            status.get("tool_ceiling").and_then(Value::as_str),
            Some("meta-only" | "readonly" | "readwrite")
        ),
        "desktop start reads tool_ceiling from /status: {status}"
    );
    assert!(
        status
            .as_object()
            .is_some_and(|map| map.contains_key("tool_root")),
        "desktop start reads tool_root from /status: {status}"
    );
    assert!(
        status
            .get("p2p_listen_addresses")
            .and_then(Value::as_array)
            .is_some_and(|rows| !rows.is_empty()),
        "expected /status to include P2P listen addresses: {status}"
    );
    assert!(
        status
            .pointer("/liveness/active_native_executors")
            .and_then(Value::as_array)
            .is_some(),
        "expected /status liveness to include active_native_executors: {status}"
    );
    assert_eq!(
        status
            .pointer("/liveness/active_native_executors_available")
            .and_then(Value::as_bool),
        Some(true)
    );

    for mutation in [
        format!(
            r#"mutation {{ create_AgentSession(input: {{ session_id: "self-budget-session", node_did: "{}", agent_id: "{}", created_at: "2026-06-02T09:59:00Z" }}) {{ _docID }} }}"#,
            escape_graphql_string(&node_did),
            escape_graphql_string(&default_agent_id),
        ),
        format!(
            r#"mutation {{ create_AgentRequest(input: {{purpose: "normal",  request_id: "self-budget-req", node_did: "{node_did}", session_id: "self-budget-session", lifecycle_state: "completed", created_at: "2026-06-02T10:00:00Z" }}) {{ _docID }} }}"#
        ),
        format!(
            r#"mutation {{ create_CompactionEntry(input: {{ compaction_key: "self-budget-ce", node_did: "{}", session_id: "self-budget-session", sequence: 1, original_tokens: 1234, compacted_tokens: 567, created_at: "2026-06-02T10:00:00Z" }}) {{ _docID }} }}"#,
            escape_graphql_string(&node_did),
        ),
    ] {
        graphql_query(&graphql, &mutation)
            .await
            .context("seeding self-view fixtures")?;
    }

    // Keep the local transcript fixture under the canonical header/source
    // contract. Its visible text is no longer an AgentMessage column.
    use gents::session::canonical_rows::{
        output_segment_create_variables, transcript_message_create_variables,
        CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    use gents_protocol::output::{
        MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
        OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, SegmentRun, SourceClose,
        StreamDeclaration, StreamPayload, TranscriptMessage,
    };
    let request = graphql_query(
        &graphql,
        r#"{ AgentRequest(filter: { request_id: { _eq: "self-budget-req" } }, limit: 2) { _docID } }"#,
    )
    .await?;
    let request_doc_id = first_graphql_row(&request, "AgentRequest")?["_docID"]
        .as_str()
        .context("self-budget request physical ID")?;
    let access = gents::config_client::ConfigAccess::Graphql(
        crate::support::graphql::served_endpoint(&graphql),
    );
    let segment = OutputSegment {
        node_did: node_did.clone(),
        requester_did: None,
        session_id: "self-budget-session".into(),
        request_doc_id: request_doc_id.into(),
        source: OutputSource::Authored {
            key: "self-budget-user:1".into(),
        },
        writer: OutputWriter::RequestExecution {
            execution_generation: "self-budget-fixture".into(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: 5,
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::Text,
            }),
        }],
        payload: "hello".into(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![5],
        }),
        created_at: "2026-06-02T10:01:00Z".into(),
    };
    let segment_response = graphql_mutation_with_variables(
        &access,
        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
        &output_segment_create_variables(&segment)?,
    )
    .await?;
    let close_doc_id =
        gents_protocol::graphql::extract_mutation_doc_id(&segment_response, "AgentOutputSegment")?;
    let message = TranscriptMessage {
        message_key: "self-budget-session:1".into(),
        session_id: "self-budget-session".into(),
        node_did: node_did.clone(),
        requester_did: None,
        request_doc_id: Some(request_doc_id.into()),
        publication: MessagePublication::RequestExecution {
            execution_generation: "self-budget-fixture".into(),
        },
        outcome: OutputOutcome::Complete,
        sequence: 1,
        role: MessageRole::User,
        native_id: None,
        blocks: vec![MessageBlock::Text {
            text: PresentedPayload {
                output: PayloadRef {
                    close_doc_id,
                    stream: 0,
                },
                presentation: PayloadPresentation::Full,
            },
        }],
        created_at: "2026-06-02T10:01:00Z".into(),
    };
    graphql_mutation_with_variables(
        &access,
        CREATE_AGENT_MESSAGE_MUTATION,
        &transcript_message_create_variables(&message)?,
    )
    .await?;

    // Foreign-principal rows on the same node. Every liveness and readiness
    // projection is scoped to this server's own node DID, so none of these
    // rows may count toward the local runtime's health, budget, or readiness
    // series — they exist to prove the queries carry that scope. The foreign
    // readiness row claims Ready but stays current, exercising the /metrics
    // fleet inventory's per-DID projection.
    let foreign_readiness_snapshot = serde_json::json!({
        "format_version": gents_protocol::row::NODE_READINESS_FORMAT_VERSION,
        "process_state": "ready",
        "active_generation": 1,
        "router_generation": 1,
        "default_agent_id": "foreign-agent",
        "agents": [{
            "agent_id": "foreign-agent",
            "state": "ready",
            "reason": null,
        }],
    })
    .to_string();
    let foreign_readiness_updated_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let foreign_readiness_snapshot = escape_graphql_string(&foreign_readiness_snapshot);
    for mutation in [
        format!(
            r#"mutation {{ create_AgentRequest(input: {{purpose: "normal",  request_id: "foreign-metrics-req", node_did: "did:test:foreign-cli", agent_id: "foreign-agent", session_id: "foreign-metrics-session", lifecycle_state: "processing", created_at: "2026-06-02T11:00:00Z" }}) {{ _docID }} }}"#
        ),
        format!(
            r#"mutation {{ create_AgentToolCall(input: {{ tool_call_key: "foreign-metrics-session:tc-foreign", request_id: "foreign-metrics-req", request_doc_id: "", session_id: "foreign-metrics-session", node_did: "did:test:foreign-cli", tool_name: "bash", tool_call_id: "tc-foreign", status: "running", lifecycle_state: "running", started_at: "2026-06-02T10:00:00Z" }}) {{ _docID }} }}"#
        ),
        format!(
            r#"mutation {{ create_ToolServiceHealthState(input: {{ service_id: "runtime-mcp-pool-obs", node_did: "did:test:foreign-cli", endpoint: "http://127.0.0.1:9/mcp", status: "unreachable", tool_count: 5, failure_count: 9, k_max: 3, last_probe_at: "2026-06-06T00:00:00Z", last_seen: "2026-06-06T00:00:00Z", updated_at: "2026-06-06T00:00:00Z" }}) {{ _docID }} }}"#
        ),
        format!(
            r#"mutation {{
                create_CompactionEntry(input: {{
                    compaction_key: "foreign-metrics-ce",
                    session_id: "foreign-metrics-session",
                    node_did: "did:test:foreign-cli",
                    sequence: 1,
                    original_tokens: 4321,
                    compacted_tokens: 21,
                    created_at: "2026-06-02T11:00:00Z"
                }}) {{ _docID }}
            }}"#
        ),
        format!(
            r#"mutation {{
                create_NodeReadiness(input: {{
                    node_did: "did:test:foreign-cli",
                    snapshot_json: "{foreign_readiness_snapshot}",
                    updated_at: "{foreign_readiness_updated_at}"
                }}) {{ _docID }}
            }}"#
        ),
    ] {
        graphql_query(&graphql, &mutation)
            .await
            .context("seeding foreign-principal fences")?;
    }

    let status_response = client
        .get(format!("http://127.0.0.1:{port}/status"))
        .send()
        .await
        .context("fetching /status after seeding context fixtures")?;
    assert!(
        status_response.status().is_success(),
        "unexpected /status response: {status_response:?}"
    );
    let status: Value = status_response
        .json()
        .await
        .context("reading /status body")?;
    let agents = status
        .get("agents")
        .and_then(Value::as_array)
        .filter(|rows| !rows.is_empty())
        .unwrap_or_else(|| panic!("expected /status to include agents: {status}"));
    assert!(
        agents.iter().any(|agent| {
            agent.get("model_name").and_then(Value::as_str) == Some(model_name.as_str())
                && agent
                    .get("endpoint")
                    .and_then(Value::as_str)
                    .is_some_and(|endpoint| !endpoint.is_empty())
        }),
        "expected /status agent joined with backend endpoint for model {model_name}: {status}"
    );
    let budget = status
        .get("context_budget")
        .unwrap_or_else(|| panic!("expected /status to include context_budget: {status}"));
    assert_eq!(
        budget.get("compaction_count").and_then(Value::as_i64),
        Some(1),
        "expected agent-scoped context_budget to count exactly the seeded compaction: {status}"
    );
    assert_eq!(
        budget.get("latest_original_tokens").and_then(Value::as_i64),
        Some(1234),
        "expected context_budget latest tokens from the seeded compaction: {status}"
    );
    let context = status
        .get("context")
        .unwrap_or_else(|| panic!("expected /status to include context indicator: {status}"));
    assert_eq!(
        context.get("compaction_count").and_then(Value::as_i64),
        Some(1),
        "expected /status context to mirror compaction count: {status}"
    );
    assert_eq!(
        context.get("current_estimate").and_then(Value::as_i64),
        Some(567),
        "expected /status context current_estimate from latest compacted tokens: {status}"
    );

    // The foreign-principal seeds above must not leak into any local
    // projection: the runtime's own liveness excludes them (they surface only
    // in the ignored-foreign counters), the context budget counts only the
    // locally seeded compaction.
    assert!(
        status
            .pointer("/liveness/active_request_ids")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        "foreign processing rows must not count as local activity: {status}"
    );
    assert!(
        status
            .pointer("/liveness/active_tool_calls")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        "foreign running tool calls must not count as local activity: {status}"
    );
    assert_eq!(
        status
            .pointer("/liveness/expired_processing_count")
            .and_then(Value::as_i64),
        Some(0),
        "foreign processing rows must not degrade local liveness: {status}"
    );
    assert_eq!(
        status
            .pointer("/liveness/ignored_foreign_processing_count")
            .and_then(Value::as_i64),
        Some(1),
        "the foreign processing row must be accounted as ignored-foreign: {status}"
    );
    assert_eq!(
        status
            .pointer("/liveness/ignored_foreign_tool_call_count")
            .and_then(Value::as_i64),
        Some(1),
        "the foreign running tool call must be accounted as ignored-foreign: {status}"
    );
    assert_eq!(
        budget.get("compaction_count").and_then(Value::as_i64),
        Some(1),
        "the foreign session's compaction must not join the local context budget: {status}"
    );
    assert_eq!(
        budget.get("latest_original_tokens").and_then(Value::as_i64),
        Some(1234),
        "the foreign compaction (4321 tokens) must not win the latest-token slot: {status}"
    );
    assert_eq!(
        status.get("ok").and_then(Value::as_bool),
        Some(true),
        "foreign rows must not degrade the local runtime verdict: {status}"
    );
    assert_eq!(
        status.get("status").and_then(Value::as_str),
        Some("ok"),
        "foreign rows must not leave /status degraded: {status}"
    );

    // /healthz is where the readiness owner projects the local row: it must
    // select readiness by this server's own DID (never a foreign row), and the
    // foreign liveness rows may not flip its liveness check to degraded.
    let health_response = client
        .get(format!("http://127.0.0.1:{port}/healthz"))
        .send()
        .await
        .context("fetching /healthz after seeding foreign fences")?;
    assert!(
        health_response.status().is_success(),
        "unexpected /healthz status after foreign seeds: {health_response:?}"
    );
    let health: Value = health_response
        .json()
        .await
        .context("reading /healthz body after foreign seeds")?;
    assert_eq!(
        health
            .pointer("/checks/runtime/ready")
            .and_then(Value::as_bool),
        Some(true),
        "the local readiness row must stay selected under /healthz: {health}"
    );
    assert_eq!(
        health
            .pointer("/checks/liveness/expired_processing_count")
            .and_then(Value::as_i64),
        Some(0),
        "foreign processing rows must not degrade the local liveness check: {health}"
    );
    assert_eq!(
        health
            .pointer("/checks/liveness/ignored_foreign_processing_count")
            .and_then(Value::as_i64),
        Some(1),
        "the foreign processing row must be accounted as ignored-foreign under /healthz: {health}"
    );
    assert_eq!(
        health.get("status").and_then(Value::as_str),
        Some("ok"),
        "foreign rows must not leave /healthz degraded: {health}"
    );

    let sessions_response = client
        .get(format!("http://127.0.0.1:{port}/sessions?limit=1"))
        .send()
        .await
        .context("fetching /sessions")?;
    let sessions_status = sessions_response.status();
    let sessions_body = sessions_response
        .text()
        .await
        .context("reading /sessions body")?;
    assert!(
        sessions_status.is_success(),
        "unexpected /sessions response: {sessions_status}: {sessions_body}"
    );
    let sessions: Value =
        serde_json::from_str(&sessions_body).context("decoding /sessions body")?;
    assert_eq!(
        sessions.get("node_did").and_then(Value::as_str),
        Some(node_did.as_str())
    );
    let session = sessions
        .get("sessions")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .unwrap_or_else(|| panic!("expected /sessions to include the seeded row: {sessions}"));
    assert_eq!(
        session.get("session_id").and_then(Value::as_str),
        Some("self-budget-session")
    );
    assert_eq!(
        session.get("request_count").and_then(Value::as_i64),
        Some(1)
    );
    assert_eq!(
        session.get("message_count").and_then(Value::as_i64),
        Some(1)
    );
    assert_eq!(
        session.get("compaction_count").and_then(Value::as_i64),
        Some(1)
    );

    let fleet_response = client
        .get(format!("http://127.0.0.1:{port}/fleet"))
        .send()
        .await
        .context("fetching /fleet")?;
    assert!(
        fleet_response.status().is_success(),
        "unexpected /fleet response: {fleet_response:?}"
    );
    let fleet: Value = fleet_response.json().await.context("reading /fleet body")?;
    assert!(
        fleet
            .get("agents")
            .and_then(Value::as_array)
            .is_some_and(|agents| agents.iter().any(|agent| {
                agent.get("node_did").and_then(Value::as_str) == Some(node_did.as_str())
                    && agent.get("process_state").and_then(Value::as_str) == Some("ready")
            })),
        "expected /fleet to list this agent in ready state: {fleet}"
    );

    let escaped_node_did = escape_graphql_string(&node_did);
    for mutation in [
        r#"mutation {
            create_ToolServiceRegistry(input: {
                service_id: "runtime-mcp-pool-obs",
                display_name: "Runtime Observability",
                description: "Runtime endpoint fixture",
                hostname: "studio-1",
                tailscale_ip: "100.64.0.10",
                lan_ip: "192.168.1.10",
                mcp_port: 9201,
                mcp_path: "/mcp",
                send_node_did: true,
                status: "online",
                version: "test",
                updated_at: "2026-06-05T00:00:00Z"
            }) { _docID }
        }"#
        .to_string(),
        format!(
            r#"mutation {{
                create_ToolServiceHealthState(input: {{
                    service_id: "runtime-mcp-pool-obs",
                    node_did: "{escaped_node_did}",
                    endpoint: "http://100.64.0.10:9201/mcp",
                    status: "healthy",
                    tool_count: 3,
                    failure_count: 0,
                    k_max: 3,
                    last_probe_at: "2026-06-05T00:00:00Z",
                    last_seen: "2026-06-05T00:00:00Z",
                    updated_at: "2026-06-05T00:00:00Z"
                }}) {{ _docID }}
            }}"#
        ),
    ] {
        graphql_query(&graphql, &mutation)
            .await
            .context("seeding MCP pool fixtures")?;
    }

    let mcp_pool_response = client
        .get(format!("http://127.0.0.1:{port}/mcp/pool"))
        .send()
        .await
        .context("fetching /mcp/pool")?;
    assert!(
        mcp_pool_response.status().is_success(),
        "unexpected /mcp/pool response: {mcp_pool_response:?}"
    );
    let mcp_pool: Value = mcp_pool_response
        .json()
        .await
        .context("reading /mcp/pool body")?;
    assert_eq!(
        mcp_pool.get("node_did").and_then(Value::as_str),
        Some(node_did.as_str())
    );
    assert_eq!(
        mcp_pool.pointer("/totals/online").and_then(Value::as_i64),
        Some(1),
        "expected /mcp/pool totals to count the seeded online service: {mcp_pool}"
    );
    assert_eq!(
        mcp_pool.pointer("/totals/healthy").and_then(Value::as_i64),
        Some(1),
        "expected /mcp/pool totals to count the seeded healthy service: {mcp_pool}"
    );
    assert!(
        mcp_pool
            .get("services")
            .and_then(Value::as_array)
            .is_some_and(|services| services.iter().any(|service| {
                service.get("service_id").and_then(Value::as_str) == Some("runtime-mcp-pool-obs")
                    && service.get("tool_count").and_then(Value::as_i64) == Some(3)
                    && service.get("health_status").and_then(Value::as_str) == Some("healthy")
            })),
        "expected /mcp/pool to include the seeded service and tool count: {mcp_pool}"
    );
    // The foreign principal has its own ToolServiceHealthState row for the
    // SAME shared registry service, stamped "unreachable". The /mcp/pool join
    // must select the local agent's health row, not the foreign one.
    assert!(
        !mcp_pool
            .get("services")
            .and_then(Value::as_array)
            .is_some_and(|services| services.iter().any(|service| {
                service.get("service_id").and_then(Value::as_str) == Some("runtime-mcp-pool-obs")
                    && service.get("health_status").and_then(Value::as_str) == Some("unreachable")
            })),
        "the foreign agent's health row for the same service must not be joined: {mcp_pool}"
    );
    assert_eq!(
        mcp_pool.pointer("/totals/healthy").and_then(Value::as_i64),
        Some(1),
        "the foreign unreachable row must not flip the local healthy total: {mcp_pool}"
    );
    assert_eq!(
        mcp_pool
            .pointer("/totals/unreachable")
            .and_then(Value::as_i64),
        Some(0),
        "the foreign health row must not add a fleet-wide unreachable count: {mcp_pool}"
    );
    // "unreachable" is not even a persisted raw state: if the foreign row
    // somehow joined it would land in the unknown bucket instead.
    assert_eq!(
        mcp_pool.pointer("/totals/unknown").and_then(Value::as_i64),
        Some(0),
        "the foreign health row must not surface in any local total: {mcp_pool}"
    );

    let mcp_off = client
        .get(format!("http://127.0.0.1:{port}/mcp"))
        .send()
        .await
        .context("probing /mcp")?;
    assert_eq!(
        mcp_off.status(),
        reqwest::StatusCode::NOT_FOUND,
        "expected /mcp to be absent without --enable-mcp"
    );

    let response = client
        .get(format!("http://127.0.0.1:{port}/metrics"))
        .send()
        .await
        .context("fetching /metrics")?;
    assert!(
        response.status().is_success(),
        "unexpected status: {response:?}"
    );
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        content_type.starts_with("text/plain"),
        "unexpected content-type: {content_type}"
    );
    let body = response.text().await.context("reading /metrics body")?;
    assert!(
        body.contains("# HELP gents_up"),
        "expected gents_up help text in metrics body:\n{body}"
    );
    assert!(
        body.contains(r#"gents_up 1"#),
        "expected gents_up sample in metrics body:\n{body}"
    );
    assert!(
        body.contains(&format!(
            r#"gents_runtime_process_state{{node_did="{node_did}",state="ready"}} 1"#
        )),
        "expected ready process-state metric in metrics body:\n{body}"
    );
    assert!(
        body.contains(&format!(
            r#"gents_runtime_active_generation{{node_did="{node_did}"}}"#
        )),
        "expected active-generation metric in metrics body:\n{body}"
    );
    assert!(
        body.contains("gents_backend_enabled"),
        "expected backend metrics in metrics body:\n{body}"
    );

    // The readiness series is a fleet inventory: every NodeReadiness
    // row is projected under its own DID. The foreign row (claiming Ready with
    // a current timestamp) must appear with its own counts, and the local row
    // must stay observed — neither row may borrow the other's identity.
    assert!(
        body.contains(&format!(
            r#"gents_runtime_runnable_agents{{node_did="did:test:foreign-cli"}} 1"#
        )),
        "the foreign readiness row must be projected under its own DID:\n{body}"
    );
    assert!(
        body.contains(&format!(
            r#"gents_runtime_node_readiness_observed{{node_did="{node_did}"}} 1"#
        )),
        "the local readiness row must stay observed after the foreign seed:\n{body}"
    );
    assert!(
        body.contains(&format!(
            r#"gents_runtime_process_state{{node_did="did:test:foreign-cli",state="ready"}} 1"#
        )),
        "the foreign process-state one-hot must be projected from its own row:\n{body}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_exposes_fleet_slot_snapshot_endpoint() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-fleet-slots-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start_hanging(&model_name)?;
    let port = allocate_port()?;
    let agent_name = format!("cli-fleet-slots-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);
    let request_content = format!("fleet slots live request {}", Uuid::new_v4());

    let init = run_init_json(
        &home_dir,
        &[
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--max-concurrent",
            "1",
            "--max-queue-depth",
            "2",
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let node_did = node_did_from_init(&init)?;
    let backend_id = init
        .pointer("/init/backend_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("init output missing backend_id: {init}"))?
        .to_string();
    let default_agent_id = init
        .pointer("/init/default_agent_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| default_agent_id_for_node(&node_did));

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;

    let submitted = run_cli_json(
        &home_dir,
        &[
            "request",
            "submit",
            "--graphql",
            &graphql,
            "--node-did",
            &node_did,
            "--content",
            &request_content,
            "--no-wait",
        ],
    )?;
    let request_id = submitted
        .get("request_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("request submit output missing request_id: {submitted}"))?
        .to_string();
    wait_for_request_lifecycle_state(
        &graphql,
        &request_id,
        &["processing"],
        Duration::from_secs(30),
    )
    .await?;
    wait_for_inference_call_state(&graphql, &request_id, "running").await?;

    let client = reqwest::Client::new();
    let stable_deadline = Instant::now() + Duration::from_secs(5);
    let (
        snapshot,
        active_calls,
        expected_backend_running,
        expected_backend_queued,
        expected_agent_running,
        expected_agent_queued,
    ) = loop {
        let response = client
            .get(format!("http://127.0.0.1:{port}/fleet/slots"))
            .send()
            .await
            .context("fetching /fleet/slots")?;
        assert!(
            response.status().is_success(),
            "unexpected /fleet/slots response: {response:?}"
        );
        let snapshot: Value = response.json().await.context("reading /fleet/slots body")?;
        let active_calls = active_inference_calls_for_backend(&graphql, &backend_id).await?;
        let backend_running = count_inference_calls(&active_calls, None, "running");
        let backend_queued = count_inference_calls(&active_calls, None, "queued");
        let snapshot_running = snapshot.pointer("/totals/assigned").and_then(Value::as_i64);
        let snapshot_queued = snapshot.pointer("/totals/queued").and_then(Value::as_i64);
        if snapshot_running == Some(backend_running) && snapshot_queued == Some(backend_queued) {
            break (
                snapshot,
                active_calls.clone(),
                backend_running,
                backend_queued,
                count_inference_calls(&active_calls, Some(&default_agent_id), "running"),
                count_inference_calls(&active_calls, Some(&default_agent_id), "queued"),
            );
        }
        if Instant::now() >= stable_deadline {
            return Err(anyhow!(
                "fleet slot snapshot did not stabilize with active inference calls; snapshot={snapshot}; active_calls={}",
                Value::Array(active_calls)
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let expected_available = 1_i64.saturating_sub(expected_backend_running);
    assert!(
        expected_backend_running >= 1,
        "test setup should hold at least one running call; active calls={}",
        Value::Array(active_calls)
    );

    assert_eq!(
        snapshot.pointer("/source").and_then(Value::as_str),
        Some("graphql.derived_admission_state")
    );
    assert_eq!(
        snapshot.pointer("/totals/assigned").and_then(Value::as_i64),
        Some(expected_backend_running)
    );
    assert_eq!(
        snapshot
            .pointer("/totals/available")
            .and_then(Value::as_i64),
        Some(expected_available)
    );
    assert_eq!(
        snapshot.pointer("/totals/max").and_then(Value::as_i64),
        Some(1)
    );
    assert_eq!(
        snapshot.pointer("/totals/queued").and_then(Value::as_i64),
        Some(expected_backend_queued)
    );
    assert_eq!(
        snapshot
            .pointer("/expired/processing_requests")
            .and_then(Value::as_i64),
        Some(0)
    );

    let backend = find_snapshot_row(&snapshot, "backends", "backend_id", &backend_id)?;
    assert_eq!(
        backend.get("running").and_then(Value::as_i64),
        Some(expected_backend_running)
    );
    assert_eq!(
        backend.get("queued").and_then(Value::as_i64),
        Some(expected_backend_queued)
    );
    assert_eq!(
        backend.get("available").and_then(Value::as_i64),
        Some(expected_available)
    );
    assert_eq!(
        backend.get("max_concurrent").and_then(Value::as_i64),
        Some(1)
    );
    assert_eq!(
        backend.get("max_queue_depth").and_then(Value::as_i64),
        Some(2)
    );
    assert_eq!(
        backend.get("accepting_admission").and_then(Value::as_bool),
        Some(true)
    );

    let agent = find_snapshot_row(&snapshot, "agents", "agent_id", &default_agent_id)?;
    assert_eq!(
        agent.get("backend_id").and_then(Value::as_str),
        Some(backend_id.as_str())
    );
    assert_eq!(
        agent.get("assigned").and_then(Value::as_i64),
        Some(expected_agent_running)
    );
    assert_eq!(
        agent.get("available").and_then(Value::as_i64),
        Some(expected_available)
    );
    assert_eq!(agent.get("max").and_then(Value::as_i64), Some(1));
    assert_eq!(
        agent.get("queued").and_then(Value::as_i64),
        Some(expected_agent_queued)
    );

    let cli_snapshot = run_cli_json(&home_dir, &["fleet", "slots", "--graphql", &graphql])?;
    assert_eq!(
        cli_snapshot.pointer("/totals/assigned"),
        snapshot.pointer("/totals/assigned")
    );
    assert_eq!(
        cli_snapshot.pointer("/backends/0/backend_id"),
        snapshot.pointer("/backends/0/backend_id")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_rejects_real_initialized_did_without_key_path() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_env = tempdir.path().join("home-env");
    let agent_home = home_env.join(".gents");
    fs::create_dir_all(&agent_home)?;

    let node_did = format!("did:key:z{}", Uuid::new_v4().simple());
    write_json_file(
        &agent_home.join("init.json"),
        &serde_json::json!({
            "home": agent_home.to_string_lossy(),
            "node_name": "mini-1-steward",
            "node_did": node_did,
            "key_path": null,
            "tool_ceiling": "Readonly",
            "tool_root": tempdir.path().to_string_lossy()
        }),
    )?;

    let port = allocate_port()?;
    let stderr = run_cli_failure_stderr(
        &home_env,
        &[
            "server",
            "--home",
            agent_home.to_str().expect("utf-8 home"),
            "--http-port",
            &port.to_string(),
        ],
    )?;
    assert!(
        stderr.contains("has no key_path and unsupported identity_backend"),
        "expected no-key-path/backend error, got:\n{stderr}"
    );
    assert!(
        !agent_home.join("keys").exists(),
        "server must not create a fallback file-key identity for a no-key initialized home"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_rejects_macos_keychain_identity_without_label() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_env = tempdir.path().join("home-env");
    let agent_home = home_env.join(".gents");
    fs::create_dir_all(&agent_home)?;

    let node_did = format!("did:key:z{}", Uuid::new_v4().simple());
    write_json_file(
        &agent_home.join("init.json"),
        &serde_json::json!({
            "home": agent_home.to_string_lossy(),
            "node_name": "mini-1-steward",
            "node_did": node_did,
            "key_path": null,
            "identity_backend": "macos-keychain",
            "tool_ceiling": "Readonly",
            "tool_root": tempdir.path().to_string_lossy()
        }),
    )?;

    let port = allocate_port()?;
    let stderr = run_cli_failure_stderr(
        &home_env,
        &[
            "server",
            "--home",
            agent_home.to_str().expect("utf-8 home"),
            "--http-port",
            &port.to_string(),
        ],
    )?;
    assert!(
        stderr.contains("macos-keychain"),
        "expected macos-keychain error, got:\n{stderr}"
    );
    assert!(
        stderr.contains("no keychain_label"),
        "expected missing keychain label error, got:\n{stderr}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_rejects_real_initialized_did_with_missing_key_file_without_creating_it(
) -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_env = tempdir.path().join("home-env");
    let agent_home = home_env.join(".gents");
    let key_path = agent_home.join("keys").join("missing.key");
    fs::create_dir_all(&agent_home)?;

    let node_did = format!("did:key:z{}", Uuid::new_v4().simple());
    write_json_file(
        &agent_home.join("init.json"),
        &serde_json::json!({
            "home": agent_home.to_string_lossy(),
            "node_name": "mini-1-steward",
            "node_did": node_did,
            "key_path": key_path.to_string_lossy(),
            "tool_ceiling": "Readonly",
            "tool_root": tempdir.path().to_string_lossy()
        }),
    )?;

    let port = allocate_port()?;
    let stderr = run_cli_failure_stderr(
        &home_env,
        &[
            "server",
            "--home",
            agent_home.to_str().expect("utf-8 home"),
            "--http-port",
            &port.to_string(),
        ],
    )?;
    assert!(
        stderr.contains("requires identity key"),
        "expected missing-key error, got:\n{stderr}"
    );
    assert!(
        stderr.contains("to already exist"),
        "expected no-create hint, got:\n{stderr}"
    );
    assert!(
        !key_path.exists(),
        "server must not create a new key for a real initialized DID with missing key file"
    );
    assert!(
        !key_path.parent().expect("key parent").exists(),
        "server must not create the missing key directory for an initialized DID"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_startup_with_iroh_p2p_reports_runtime_connectivity() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-p2p-ready-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;

    let port = allocate_port()?;
    let agent_name = format!("cli-p2p-ready-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);

    let init = run_init_json(
        &home_dir,
        &[
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let node_did = node_did_from_init(&init)?;
    let default_agent_id = default_agent_id_for_node(&node_did);
    let (mut serve, readiness) = spawn_server_with_ready_json(
        &home_dir,
        port,
        &[
            "--p2p-bind-addr",
            "127.0.0.1",
            "--p2p-port",
            "0",
            "--p2p-relay-mode",
            "disabled",
            "--p2p-discovery",
            "disabled",
        ],
        &[],
    )?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;

    assert_eq!(
        readiness.get("node_did").and_then(Value::as_str),
        Some(node_did.as_str())
    );
    assert_eq!(
        readiness.get("graphql").and_then(Value::as_str),
        Some(graphql.as_str())
    );
    assert_eq!(
        readiness.get("default_agent_id").and_then(Value::as_str),
        Some(default_agent_id.as_str())
    );
    assert_eq!(
        readiness.get("p2p_transport").and_then(Value::as_str),
        Some("iroh")
    );
    assert!(readiness
        .get("p2p_peer_id")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty()));
    assert!(readiness
        .get("p2p_listen_addresses")
        .and_then(Value::as_array)
        .is_some_and(|rows| !rows.is_empty()));

    let status_response = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/status"))
        .send()
        .await
        .context("fetching /status")?;
    assert!(
        status_response.status().is_success(),
        "unexpected /status response: {status_response:?}"
    );
    let status: Value = status_response
        .json()
        .await
        .context("reading /status body")?;
    assert_eq!(
        status.get("node_did").and_then(Value::as_str),
        Some(node_did.as_str())
    );
    assert_eq!(
        status.get("node_name").and_then(Value::as_str),
        Some(agent_name.as_str())
    );
    assert_eq!(
        status.get("p2p_transport").and_then(Value::as_str),
        Some("iroh")
    );
    assert!(status
        .get("p2p_peer_id")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty()));
    assert!(status
        .get("p2p_listen_addresses")
        .and_then(Value::as_array)
        .is_some_and(|rows| !rows.is_empty()));

    let runtime_state = read_runtime_state_json(&home_dir)?;
    assert_eq!(
        runtime_state.get("p2p_transport").and_then(Value::as_str),
        Some("iroh")
    );
    assert_eq!(
        runtime_state.get("p2p_peer_id"),
        readiness.get("p2p_peer_id")
    );
    assert_eq!(
        runtime_state.get("p2p_listen_addresses"),
        readiness.get("p2p_listen_addresses")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_startup_defaults_to_iroh_p2p_for_desktop_pairing() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-default-iroh-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;

    let port = allocate_port()?;
    let agent_name = format!("cli-default-iroh-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);

    let init = run_init_json(
        &home_dir,
        &[
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let node_did = node_did_from_init(&init)?;
    let (mut serve, readiness) = spawn_server_with_ready_json(&home_dir, port, &[], &[])?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;

    assert_eq!(
        readiness.get("p2p_transport").and_then(Value::as_str),
        Some("iroh")
    );
    assert!(readiness
        .get("p2p_peer_id")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty()));
    assert!(readiness
        .get("p2p_listen_addresses")
        .and_then(Value::as_array)
        .is_some_and(|rows| !rows.is_empty()));

    let runtime_state = read_runtime_state_json(&home_dir)?;
    assert_eq!(
        runtime_state.get("p2p_transport").and_then(Value::as_str),
        Some("iroh")
    );
    assert_eq!(
        runtime_state.get("p2p_peer_id"),
        readiness.get("p2p_peer_id")
    );
    assert_eq!(
        runtime_state.get("p2p_listen_addresses"),
        readiness.get("p2p_listen_addresses")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_starts_in_degraded_mode_when_backend_is_unavailable() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-degraded-model-{}", Uuid::new_v4().simple());
    let warm_port = allocate_port()?;
    let port = allocate_port()?;
    let agent_name = format!("cli-degraded-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);

    let init = run_init_json(
        &home_dir,
        &[
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            "http://127.0.0.1:9/v1",
        ],
    )?;
    let node_did = node_did_from_init(&init)?;
    let backend_id = init
        .pointer("/init/backend_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("init output missing backend_id: {init}"))?
        .to_string();

    let mut warm_server = spawn_server(&home_dir, warm_port)?;
    wait_for_port(warm_port, &mut warm_server)?;
    wait_for_runtime_ready(&graphql_url(warm_port), &node_did, Duration::from_secs(30)).await?;
    graphql_query(
        &graphql_url(warm_port),
        &format!(
            r#"mutation {{
                update_InferenceBackend(
                    filter: {{
                        node_did: {{ _eq: "{}" }},
                        backend_id: {{ _eq: "{}" }}
                    }},
                    input: {{ probe_status: "unknown", last_probe: null }}
                ) {{ _docID }}
            }}"#,
            escape_graphql_string(&node_did),
            escape_graphql_string(&backend_id),
        ),
    )
    .await
    .context("seeding an unavailable backend observation")?;
    warm_server
        .child
        .kill()
        .context("stopping warm server after backend downgrade")?;
    warm_server
        .child
        .wait()
        .context("waiting for warm server shutdown")?;

    let (mut serve, readiness) = spawn_server_with_ready_json(&home_dir, port, &[], &[])?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;

    assert_eq!(
        readiness.get("status").and_then(Value::as_str),
        Some("serving")
    );
    assert_eq!(
        readiness.get("readiness_status").and_then(Value::as_str),
        Some("degraded")
    );
    assert_eq!(
        readiness
            .get("runnable_agents")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(0)
    );
    let unavailable = readiness
        .get("unavailable_agents")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("readiness missing unavailable_agents: {readiness}"))?;
    assert_eq!(unavailable.len(), 1);
    let reason = unavailable
        .first()
        .and_then(|entry| entry.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(
        reason.contains("inference backend is temporarily unavailable"),
        "unexpected unavailable reason: {reason}"
    );
    assert_eq!(
        readiness.get("graphql").and_then(Value::as_str),
        Some(graphql.as_str())
    );

    let status = run_cli_json(&home_dir, &["status"])?;
    assert_eq!(
        status.get("process_state").and_then(Value::as_str),
        Some("ready")
    );
    assert_eq!(
        status.get("readiness_status").and_then(Value::as_str),
        Some("degraded")
    );
    assert_eq!(
        status.get("runnable_agent_count").and_then(Value::as_i64),
        Some(0)
    );
    let status_unavailable = status
        .get("unavailable_agents")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("status output missing unavailable_agents: {status}"))?;
    assert_eq!(status_unavailable.len(), 1);
    let status_reason = status_unavailable
        .values()
        .next()
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(
        status_reason.contains("inference backend is temporarily unavailable"),
        "unexpected status unavailable reason: {status_reason}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_and_server_use_backend_specific_api_key_env_var() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-auth-model-{}", Uuid::new_v4().simple());
    let expected_reply = "AUTH_BACKEND_OK";
    let mock_endpoint = MockChatEndpoint::start_with_required_bearer(
        &model_name,
        expected_reply,
        Some("backend-key"),
    )?;

    let port = allocate_port()?;
    let agent_name = format!("cli-auth-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);

    let init = run_init_json(
        &home_dir,
        &[
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--api-key-env-var",
            "GENTS_TEST_CLI_BACKEND_KEY",
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    assert_eq!(
        init.pointer("/init/api_key_env_var")
            .and_then(Value::as_str),
        Some("GENTS_TEST_CLI_BACKEND_KEY")
    );
    let node_did = node_did_from_init(&init)?;
    let backend_id = init
        .pointer("/init/backend_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("init output missing backend_id: {init}"))?
        .to_string();
    let tools_id = generated_tools_id_for_agent(&node_did);

    let (_serve, readiness) = spawn_server_with_ready_json(
        &home_dir,
        port,
        &[],
        &[("GENTS_TEST_CLI_BACKEND_KEY", "backend-key")],
    )?;
    assert_eq!(
        readiness.get("graphql").and_then(Value::as_str),
        Some(graphql.as_str()),
        "serving payload must identify the allocated GraphQL endpoint"
    );

    assert_runtime_init_state(
        &graphql,
        &node_did,
        &backend_id,
        mock_endpoint.endpoint(),
        "OpenAiCompatible",
        None,
        Some("GENTS_TEST_CLI_BACKEND_KEY"),
        &model_name,
        &tools_id,
        "ReadOnly",
        "ReadOnly",
        "read-only operating mode",
    )
    .await?;

    let output = run_cli_text(
        &home_dir,
        &[
            "chat",
            "backend auth should flow through the configured env var",
        ],
    )?;
    assert!(
        output.contains(expected_reply),
        "expected chat output to contain {expected_reply}, got:\n{output}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn query_command_reconstructs_a_trace() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-query-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let agent_name = format!("cli-query-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);

    let init = run_init_json(
        &home_dir,
        &[
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let node_did = node_did_from_init(&init)?;

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;

    let node_did_literal = escape_graphql_string(&node_did);
    let request_response = graphql_query(
        &graphql,
        &format!(
            r#"mutation {{ create_AgentRequest(input: {{purpose: "normal",  request_id: "trace-req", node_did: "{node_did_literal}", session_id: "trace-session", lifecycle_state: "completed", content: "hi", created_at: "2026-06-03T10:00:00Z" }}) {{ _docID }} }}"#
        ),
    )
    .await
    .context("seeding trace request")?;
    let request_doc_id =
        gents_protocol::graphql::extract_mutation_doc_id(&request_response, "AgentRequest")?;

    let request_doc_id_literal = escape_graphql_string(&request_doc_id);
    graphql_query(
        &graphql,
        &format!(
            r#"mutation {{ create_AgentToolCall(input: {{ tool_call_key: "trace-tc", request_id: "trace-req", request_doc_id: "{request_doc_id_literal}", session_id: "trace-session", node_did: "{node_did_literal}", message_sequence: 1, tool_name: "query", tool_call_id: "trace-tc-1", status: "completed", lifecycle_state: "completed", started_at: "2026-06-03T10:00:01Z", completed_at: "2026-06-03T10:00:02Z" }}) {{ _docID }} }}"#
        ),
    )
    .await
    .context("seeding trace tool call")?;

    // Canonical durable output fixture (#1571): a closed provider-turn source
    // whose single text stream carries the answer bytes, then the transcript
    // header referencing it, then the terminalization owner's selection
    // stamped on the request. No retired AgentResponse or inline-content rows.
    use gents::session::canonical_rows::{
        output_segment_create_variables, transcript_message_create_variables,
        CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    use gents_protocol::output::{
        MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
        OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, SegmentRun, SourceClose,
        StreamDeclaration, StreamPayload, TerminalOutput, TranscriptMessage,
    };
    use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};

    let access = gents::config_client::ConfigAccess::Graphql(
        crate::support::graphql::served_endpoint(&graphql),
    );
    let segment = OutputSegment {
        node_did: node_did.clone(),
        requester_did: None,
        session_id: "trace-session".to_string(),
        request_doc_id: request_doc_id.clone(),
        source: OutputSource::ProviderTurn {
            scope: CaptureScope {
                kind: CaptureScopeKind::Inference,
                seq: 1,
            },
            turn_index: 0,
            attempt: 0,
        },
        writer: OutputWriter::RequestExecution {
            execution_generation: "trace-generation".to_string(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: "hello".len() as u32,
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::Text,
            }),
        }],
        payload: "hello".to_string(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec!["hello".len() as u64],
        }),
        created_at: "2026-06-03T10:00:03Z".to_string(),
    };
    let segment_response = graphql_mutation_with_variables(
        &access,
        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
        &output_segment_create_variables(&segment)?,
    )
    .await?;
    let close_doc_id =
        gents_protocol::graphql::extract_mutation_doc_id(&segment_response, "AgentOutputSegment")?;

    let header = TranscriptMessage {
        message_key: gents::session::sequence_message_key(&node_did, "trace-session", None, 1),
        session_id: "trace-session".to_string(),
        node_did: node_did.clone(),
        requester_did: None,
        request_doc_id: Some(request_doc_id.clone()),
        publication: MessagePublication::RequestExecution {
            execution_generation: "trace-generation".to_string(),
        },
        outcome: OutputOutcome::Complete,
        sequence: 1,
        role: MessageRole::Assistant,
        native_id: None,
        blocks: vec![MessageBlock::Text {
            text: PresentedPayload {
                output: PayloadRef {
                    close_doc_id: close_doc_id.clone(),
                    stream: 0,
                },
                presentation: PayloadPresentation::Full,
            },
        }],
        created_at: "2026-06-03T10:00:04Z".to_string(),
    };
    let header_response = graphql_mutation_with_variables(
        &access,
        CREATE_AGENT_MESSAGE_MUTATION,
        &transcript_message_create_variables(&header)?,
    )
    .await?;
    let message_doc_id =
        gents_protocol::graphql::extract_mutation_doc_id(&header_response, "AgentMessage")?;

    // Stamp the terminalization owner's selection onto the terminal request so
    // the canonical output owner resolves the exact header and dependencies.
    let selection = serde_json::to_value(TerminalOutput::Message {
        message_doc_id: message_doc_id.clone(),
    })?;
    graphql_mutation_with_variables(
        &access,
        r#"mutation($request_doc_id: String!, $terminal_output: JSON!, $lifecycle_state: String!) {
                update_AgentRequest(
                    filter: { _docID: { _eq: $request_doc_id } }
                    input: { terminal_output: $terminal_output, lifecycle_state: $lifecycle_state }
                ) { _docID }
            }"#,
        &serde_json::json!({
            "request_doc_id": request_doc_id,
            "terminal_output": selection,
            "lifecycle_state": "completed",
        }),
    )
    .await
    .context("stamping trace terminal selection")?;

    let request = run_cli_json(
        &home_dir,
        &[
            "query",
            "--graphql",
            &graphql,
            "--collection",
            "AgentRequest",
            "--field",
            "request_id",
            "--field",
            "session_id",
            "--field",
            "lifecycle_state",
            "--filter",
            r#"{"request_id":{"_eq":"trace-req"}}"#,
        ],
    )?;
    assert_eq!(
        request.get("returned_count").and_then(Value::as_i64),
        Some(1),
        "{request}"
    );
    let req_row = &request["results"][0];
    assert_eq!(req_row["session_id"].as_str(), Some("trace-session"));
    assert_eq!(req_row["lifecycle_state"].as_str(), Some("completed"));

    let tool_calls = run_cli_json(
        &home_dir,
        &[
            "query",
            "--graphql",
            &graphql,
            "--collection",
            "AgentToolCall",
            "--field",
            "request_id",
            "--field",
            "request_doc_id",
            "--field",
            "tool_name",
            "--field",
            "status",
            "--field",
            "lifecycle_state",
            "--filter",
            r#"{"request_id":{"_eq":"trace-req"}}"#,
        ],
    )?;
    assert_eq!(
        tool_calls.get("returned_count").and_then(Value::as_i64),
        Some(1),
        "{tool_calls}"
    );
    let tc = &tool_calls["results"][0];
    assert_eq!(tc["tool_name"].as_str(), Some("query"));
    assert_eq!(
        tc["lifecycle_state"].as_str(),
        Some("completed"),
        "tool lifecycle is the execution outcome owner"
    );

    // Export through the canonical owner: the request row decodes strictly and
    // the terminal selection resolves the exact header plus its dependencies —
    // no AgentResponse shape anywhere in the trace.
    let request_response = graphql_query(
        &graphql,
        &format!(
            r#"{{
                AgentRequest(
                    filter: {{ request_id: {{ _eq: "{}" }} }},
                    limit: 1
                ) {{
                    _docID node_did requester_did session_id request_id
                    lifecycle_state failure_reason terminal_output
                }}
            }}"#,
            escape_graphql_string("trace-req"),
        ),
    )
    .await?;
    let request_row_value = first_graphql_row(&request_response, "AgentRequest")?.clone();
    let request_row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(request_row_value).context("decoding canonical AgentRequest row")?;
    assert_eq!(
        request_row.session_id.as_deref(),
        Some("trace-session"),
        "request row lost session_id"
    );
    assert_eq!(
        request_row
            .lifecycle_state
            .context("request row lost lifecycle_state")?
            .as_str(),
        "completed"
    );
    let output = gents::session::observe_request_output(
        &gents::config_client::ConfigAccess::Graphql(crate::support::graphql::served_endpoint(
            &graphql,
        )),
        &request_row,
    )
    .await?;
    let gents::session::CanonicalRequestOutput::TerminalMessage {
        header: exported_header,
        message: exported_message,
        ..
    } = &output
    else {
        panic!("expected canonical terminal message export, got: {output:?}");
    };
    assert_eq!(
        exported_header.request_doc_id.as_deref(),
        request_row.doc_id.as_deref(),
        "export resolves the exact physical request"
    );
    assert_eq!(exported_header.session_id, "trace-session");
    let gents_protocol::message::Message::Assistant { content, .. } = exported_message else {
        panic!("expected reconstructed assistant message, got: {exported_message:?}");
    };
    assert_eq!(
        content.first().and_then(|block| match block {
            gents_protocol::message::AssistantContent::Text(gents_protocol::message::Text {
                text,
                ..
            }) => Some(text.as_str()),
            _ => None,
        }),
        Some("hello"),
        "exported transcript must reconstruct the streamed text"
    );

    let messages = run_cli_json(
        &home_dir,
        &[
            "query",
            "--graphql",
            &graphql,
            "--collection",
            "AgentMessage",
            "--field",
            "session_id",
            "--field",
            "role",
            "--field",
            "sequence",
            "--filter",
            r#"{"session_id":{"_eq":"trace-session"}}"#,
        ],
    )?;
    let msg_row = &messages["results"][0];
    assert_eq!(msg_row["role"].as_str(), Some("assistant"));

    assert_eq!(
        req_row["session_id"].as_str(),
        msg_row["session_id"].as_str()
    );
    assert_eq!(
        tc["request_id"].as_str(),
        Some("trace-req"),
        "tool call stays bound to the logical request coordinate"
    );
    assert_eq!(
        exported_header.request_doc_id.as_deref(),
        request_row.doc_id.as_deref(),
        "tool call request and terminal header resolve to one physical request"
    );

    let denied = run_cli_failure_stderr(
        &home_dir,
        &[
            "query",
            "--graphql",
            &graphql,
            "--collection",
            "InferenceBackend",
            "--field",
            "auth",
        ],
    )?;
    assert!(
        denied.contains("restricted"),
        "expected secret guard to fire: {denied}"
    );

    let diagnostic = run_cli_failure_stderr(
        &home_dir,
        &[
            "query",
            "--graphql",
            &graphql,
            "--collection",
            "AgentToolCall",
            "--field",
            "created_at",
        ],
    )?;
    assert!(diagnostic.contains("created_at"), "{diagnostic}");
    assert!(
        diagnostic.contains("started_at") && diagnostic.contains("completed_at"),
        "suggestions missing: {diagnostic}"
    );
    assert!(
        diagnostic.contains("tool_call_key"),
        "field inventory missing: {diagnostic}"
    );

    let inventory = run_cli_json(
        &home_dir,
        &[
            "query",
            "--graphql",
            &graphql,
            "--collection",
            "InferenceBackend",
            "--field",
            "*",
        ],
    )?;
    assert_eq!(inventory["discovery"], Value::Bool(true), "{inventory}");
    let field_names: Vec<&str> = inventory["fields"]
        .as_array()
        .context("discovery fields array")?
        .iter()
        .filter_map(|f| f["name"].as_str())
        .collect();
    assert!(field_names.contains(&"backend_id"), "{field_names:?}");
    assert!(
        !field_names.contains(&"auth"),
        "secret leaked into discovery inventory: {field_names:?}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_endpoint_serves_defra_query() -> Result<()> {
    use rmcp::model::CallToolRequestParams;
    use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
    use rmcp::ServiceExt;

    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-mcp-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let agent_name = format!("cli-mcp-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);

    let init = run_init_json(
        &home_dir,
        &[
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let node_did = node_did_from_init(&init)?;

    let mut serve = spawn_server_with_env(
        &home_dir,
        port,
        &["--enable-mcp", "--mcp-write-collection", "McpParcel"],
        &[],
    )?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;

    for mutation in [
        format!(
            r#"mutation {{ create_AgentRequest(input: {{purpose: "normal",  request_id: "mcp-req", node_did: "{node_did}", session_id: "mcp-session", lifecycle_state: "completed", created_at: "2026-06-03T10:00:00Z" }}) {{ _docID }} }}"#
        ),
        r#"mutation { create_AgentToolCall(input: { tool_call_key: "mcp-tc", request_id: "mcp-req", session_id: "mcp-session", tool_name: "query", status: "completed" }) { _docID } }"#.to_string(),
    ] {
        graphql_query(&graphql, &mutation)
            .await
            .context("seeding mcp trace")?;
    }

    let config =
        StreamableHttpClientTransportConfig::with_uri(format!("http://127.0.0.1:{port}/mcp"));
    let transport = rmcp::transport::StreamableHttpClientTransport::from_config(config);
    let mcp = ().serve(transport).await.context("MCP client handshake with /mcp")?;

    let tools = mcp.peer().list_tools(None).await.context("list_tools")?;
    assert!(
        tools.tools.iter().any(|tool| tool.name.as_ref() == "query"),
        "expected defra_query in advertised tools: {:?}",
        tools
            .tools
            .iter()
            .map(|t| t.name.as_ref())
            .collect::<Vec<_>>()
    );

    let args = serde_json::json!({
        "argv": ["find"], "collection": "AgentToolCall",
        "options": {"fields": ["request_id", "request_doc_id", "tool_name", "lifecycle_state"],
        "filter": { "request_id": { "_eq": "mcp-req" } }}
    });
    let params =
        CallToolRequestParams::new("query").with_arguments(args.as_object().unwrap().clone());
    let result = mcp
        .peer()
        .call_tool(params)
        .await
        .context("call_tool defra_query")?;
    let text = result
        .content
        .iter()
        .filter_map(|content| content.raw.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("");
    let payload: Value = serde_json::from_str(&text).context("MCP tool result is JSON")?;
    assert_eq!(payload["returned_count"].as_i64(), Some(1), "{payload}");
    let tc = &payload["results"][0];
    assert_eq!(tc["tool_name"].as_str(), Some("query"));
    assert_eq!(tc["request_id"].as_str(), Some("mcp-req"));

    let denied_args = serde_json::json!({"argv":["find"], "collection": "InferenceBackend", "options":{"fields": ["auth"]} });
    let denied_params = CallToolRequestParams::new("query")
        .with_arguments(denied_args.as_object().unwrap().clone());
    let denied = mcp.peer().call_tool(denied_params).await;
    let blocked = match denied {
        Err(_) => true,
        Ok(result) => {
            result.is_error == Some(true)
                || result.content.iter().any(|content| {
                    content
                        .raw
                        .as_text()
                        .map(|t| t.text.contains("restricted"))
                        .unwrap_or(false)
                })
        }
    };
    assert!(
        blocked,
        "expected MCP defra_query to block backend auth selection"
    );

    assert!(tools.tools.iter().any(|tool| tool.name.as_ref() == "write"));
    let preview = serde_json::json!({"argv":["preview","create"],"collection":"McpParcel","options":{"input":{"reference":"MCP-7","status":"queued"}}});
    let unsigned = mcp
        .peer()
        .call_tool(
            CallToolRequestParams::new("write")
                .with_arguments(preview.as_object().unwrap().clone()),
        )
        .await;
    assert!(
        match unsigned {
            Err(_) => true,
            Ok(result) => result.is_error == Some(true),
        },
        "anonymous MCP write must fail"
    );
    let _identity = identity_from_init(&init)?;
    let access = gents::config_client::ConfigAccess::graphql_as(graphql.clone(), node_did.clone());
    access
        .add_schema("type McpParcel { reference: String status: String }")
        .await?;
    let bearer =
        gents::identity::defradb_bearer_authorization(&node_did, &format!("127.0.0.1:{port}"))?;
    let config =
        StreamableHttpClientTransportConfig::with_uri(format!("http://127.0.0.1:{port}/mcp"))
            .auth_header(bearer.strip_prefix("Bearer ").unwrap());
    let authenticated = ()
        .serve(rmcp::transport::StreamableHttpClientTransport::from_config(
            config,
        ))
        .await?;
    for command in [
        preview,
        serde_json::json!({"argv":["preview","update"],"collection":"McpParcel","options":{"filter":{"reference":{"_eq":"MCP-7"}},"max_targets":1,"input":{"status":"delivered"}}}),
        serde_json::json!({"argv":["preview","delete"],"collection":"McpParcel","options":{"filter":{"reference":{"_eq":"MCP-7"}},"max_targets":1}}),
    ] {
        let result = authenticated
            .peer()
            .call_tool(
                CallToolRequestParams::new("write")
                    .with_arguments(command.as_object().unwrap().clone()),
            )
            .await?;
        assert_ne!(result.is_error, Some(true), "{result:?}");
        let text = result
            .content
            .iter()
            .filter_map(|v| v.raw.as_text().map(|v| v.text.as_str()))
            .collect::<String>();
        let preview: Value = serde_json::from_str(&text)?;
        assert_eq!(preview["effect"]["target_count"], 1);
        let applied = authenticated
            .peer()
            .call_tool(
                CallToolRequestParams::new("write")
                    .with_arguments(preview["next_call"]["args"].as_object().unwrap().clone()),
            )
            .await?;
        assert_ne!(applied.is_error, Some(true), "{applied:?}");
    }
    assert_eq!(
        access.execute("{McpParcel{reference status}}").await?["data"]["McpParcel"],
        serde_json::json!([])
    );
    let _ = authenticated.cancel().await;
    let _ = mcp.cancel().await;
    Ok(())
}

const MCP_GRAPH_READ_TOOLS: [&str; 3] = ["list_graphs", "get_graph_run", "get_graph_result"];

/// A served home whose owner key is loaded in this process, so the test can
/// mint the owner's DefraDB bearers.
struct McpGraphHome {
    tempdir: tempfile::TempDir,
    home: std::path::PathBuf,
    port: u16,
    owner_did: String,
    _owner: gents::KeyIdentity,
    _server: ServeProcess,
}

impl McpGraphHome {
    async fn start(label: &str, serve_flags: &[&str]) -> Result<Self> {
        let tempdir = tempfile::tempdir().context("creating tempdir")?;
        let home = tempdir.path().join("agent-home");
        let home_arg = home.to_str().context("home path is not UTF-8")?.to_owned();
        let init = run_init_json(
            tempdir.path(),
            &["--agent-name", label, "--home", home_arg.as_str()],
        )?;
        let owner_did = node_did_from_init(&init)?;
        let owner = identity_from_init(&init)?;
        let port = allocate_port()?;
        let mut serve = vec!["--home", home_arg.as_str()];
        serve.extend_from_slice(serve_flags);
        let (server, readiness) = spawn_server_with_ready_json(&home, port, &serve, &[])?;
        anyhow::ensure!(
            readiness.get("status").and_then(Value::as_str) == Some("serving"),
            "server did not become ready: {readiness}"
        );
        wait_for_runtime_ready(&graphql_url(port), &owner_did, Duration::from_secs(30)).await?;
        Ok(Self {
            tempdir,
            home,
            port,
            owner_did,
            _owner: owner,
            _server: server,
        })
    }

    fn audience(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    async fn client(
        &self,
        bearer: Option<&str>,
    ) -> Result<rmcp::service::RunningService<rmcp::RoleClient, ()>> {
        use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
        use rmcp::ServiceExt;
        let mut config = StreamableHttpClientTransportConfig::with_uri(format!(
            "http://127.0.0.1:{}/mcp",
            self.port
        ));
        if let Some(bearer) = bearer {
            config = config.auth_header(bearer.strip_prefix("Bearer ").context("bearer prefix")?);
        }
        ().serve(rmcp::transport::StreamableHttpClientTransport::from_config(
            config,
        ))
        .await
        .context("MCP handshake with /mcp")
    }
}

/// Build and install the `prepared_graph` fixture, then start one run of it
/// through `gents graph run`, as `cli_graph.rs` does; returns its run id.
async fn start_prepared_graph_run(served: &McpGraphHome) -> Result<String> {
    let root = served.tempdir.path();
    let utf8 = |path: &std::path::Path| {
        path.to_str()
            .map(str::to_owned)
            .context("path is not UTF-8")
    };
    let home = utf8(&served.home)?;
    let pack_dir = root.join("prepared_graph");
    copy_dir_all(&fixture_pack_dir("prepared_graph"), &pack_dir)?;
    let pack_file = root.join("prepared_graph.pack");
    run_cli_json(
        root,
        &[
            "pack",
            "build",
            &utf8(&pack_dir)?,
            "--out",
            &utf8(&pack_file)?,
        ],
    )?;
    let worker = format!("worker={}:default-profile", served.owner_did);
    run_cli_json(
        root,
        &[
            "pack",
            "install",
            &utf8(&pack_file)?,
            "--home",
            &home,
            "--grant-authority",
            "--node-did",
            &served.owner_did,
            "--inference-slot",
            &worker,
        ],
    )?;
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo)?;
    let git = |args: &[&str]| -> Result<()> {
        let output = std::process::Command::new("git")
            .current_dir(&repo)
            .args([
                "-c",
                "user.email=test@example.com",
                "-c",
                "user.name=Test",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .context("running git")?;
        anyhow::ensure!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    };
    git(&["init", "--quiet"])?;
    std::fs::write(repo.join("a.txt"), "one\n")?;
    git(&["add", "-A"])?;
    git(&["commit", "--quiet", "-m", "base"])?;
    std::fs::write(repo.join("a.txt"), "two\n")?;
    git(&["add", "-A"])?;
    git(&["commit", "--quiet", "-m", "head"])?;
    let receipt = run_cli_json(
        root,
        &[
            "graph",
            "run",
            "fixture/prepared_graph",
            "--output",
            "json",
            "--home",
            &home,
            "--graphql",
            &graphql_url(served.port),
            "--node-did",
            &served.owner_did,
            "--field",
            &format!("repository={}", utf8(&repo)?),
        ],
    )?;
    receipt
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .with_context(|| format!("graph run printed no run_id: {receipt}"))
}

async fn advertised(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
) -> Result<Vec<String>> {
    Ok(client
        .peer()
        .list_tools(None)
        .await?
        .tools
        .iter()
        .map(|tool| tool.name.to_string())
        .collect())
}

/// The JSON a graph read returned, or the refusal text it carried.
async fn call_mcp_tool(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    name: &str,
    args: Value,
) -> Result<Result<Value, String>> {
    use rmcp::model::CallToolRequestParams;
    let text = |result: &rmcp::model::CallToolResult| -> String {
        result
            .content
            .iter()
            .filter_map(|content| content.raw.as_text().map(|text| text.text.clone()))
            .collect()
    };
    let outcome = client
        .peer()
        .call_tool(
            CallToolRequestParams::new(name.to_owned())
                .with_arguments(args.as_object().cloned().unwrap_or_default()),
        )
        .await;
    Ok(match outcome {
        Err(error) => Err(error.to_string()),
        Ok(result) if result.is_error == Some(true) => Err(text(&result)),
        Ok(result) => {
            Ok(serde_json::from_str(&text(&result)).context("graph read result is JSON")?)
        }
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_graph_tools_refuse_a_request_without_authorization() -> Result<()> {
    let served = McpGraphHome::start(
        "mcp-graph-anonymous",
        &["--enable-mcp", "--mcp-graph-tools"],
    )
    .await?;
    let client = served.client(None).await?;
    for tool in MCP_GRAPH_READ_TOOLS {
        let args = if tool == "list_graphs" {
            json!({})
        } else {
            json!({"run_id": "any"})
        };
        let refused = call_mcp_tool(&client, tool, args).await?;
        anyhow::ensure!(
            matches!(&refused, Err(message) if message.contains("caller-signed DefraDB Bearer")),
            "{tool}: {refused:?}"
        );
    }
    let _ = client.cancel().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_graph_tools_refuse_a_bearer_for_another_audience() -> Result<()> {
    let served =
        McpGraphHome::start("mcp-graph-audience", &["--enable-mcp", "--mcp-graph-tools"]).await?;
    let bearer = gents::identity::defradb_bearer_authorization(
        &served.owner_did,
        &format!("localhost:{}", served.port),
    )?;
    let client = served.client(Some(&bearer)).await?;
    let refused = call_mcp_tool(&client, "list_graphs", json!({})).await?;
    anyhow::ensure!(
        matches!(&refused, Err(message) if message.contains("another host:port")),
        "{refused:?}"
    );
    let _ = client.cancel().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_graph_tools_are_absent_without_the_flag() -> Result<()> {
    let served = McpGraphHome::start("mcp-graph-off", &["--enable-mcp"]).await?;
    let client = served.client(None).await?;
    let names = advertised(&client).await?;
    anyhow::ensure!(names.iter().any(|name| name == "query"), "{names:?}");
    anyhow::ensure!(
        !names.iter().any(|name| name.contains("graph")),
        "graph tools must not be advertised without --mcp-graph-tools: {names:?}"
    );
    let _ = client.cancel().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_graph_tools_offer_only_the_read_tools() -> Result<()> {
    let served =
        McpGraphHome::start("mcp-graph-reads", &["--enable-mcp", "--mcp-graph-tools"]).await?;
    let bearer =
        gents::identity::defradb_bearer_authorization(&served.owner_did, &served.audience())?;
    let client = served.client(Some(&bearer)).await?;
    let names = advertised(&client).await?;
    for name in MCP_GRAPH_READ_TOOLS {
        anyhow::ensure!(
            names.iter().any(|advertised| advertised == name),
            "{name} missing from {names:?}"
        );
    }
    for absent in ["run_graph", "cancel_graph_run", "preview_graph", "config"] {
        anyhow::ensure!(
            !names.iter().any(|name| name == absent),
            "{absent} must not be offered over /mcp: {names:?}"
        );
    }
    let refused = call_mcp_tool(&client, "run_graph", json!({"package": "any"})).await?;
    anyhow::ensure!(refused.is_err(), "run_graph has no /mcp route: {refused:?}");
    let _ = client.cancel().await;
    Ok(())
}

/// A run the CLI starts is readable over `/mcp` with the owner's bearer:
/// every read is forwarded to DefraDB as the owner, and the listing names
/// no run tool because `/mcp` offers none.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_graph_tools_read_a_cli_started_run_with_the_owner_bearer() -> Result<()> {
    let served =
        McpGraphHome::start("mcp-graph-owner", &["--enable-mcp", "--mcp-graph-tools"]).await?;
    let run_id = start_prepared_graph_run(&served).await?;
    let bearer =
        gents::identity::defradb_bearer_authorization(&served.owner_did, &served.audience())?;
    let owner = served.client(Some(&bearer)).await?;

    let listed = call_mcp_tool(&owner, "list_graphs", json!({}))
        .await?
        .map_err(|refusal| anyhow::anyhow!("list_graphs refused the owner: {refusal}"))?;
    let graphs = listed["graphs"].as_array().context("graphs array")?;
    anyhow::ensure!(
        graphs
            .iter()
            .any(|graph| graph["active_plan"]["package"]["name"] == "prepared_graph"),
        "{listed}"
    );
    anyhow::ensure!(
        graphs.iter().all(|graph| graph.get("run_with").is_none()),
        "/mcp offers no run tool, so the listing names none: {listed}"
    );
    let observed = call_mcp_tool(&owner, "get_graph_run", json!({"run_id": run_id}))
        .await?
        .map_err(|refusal| anyhow::anyhow!("get_graph_run refused the owner: {refusal}"))?;
    anyhow::ensure!(
        observed["run_id"] == run_id.as_str() && observed["owner_did"] == served.owner_did.as_str(),
        "{observed}"
    );
    let result = call_mcp_tool(&owner, "get_graph_result", json!({"run_id": run_id}))
        .await?
        .map_err(|refusal| anyhow::anyhow!("get_graph_result refused the owner: {refusal}"))?;
    anyhow::ensure!(result["run_id"] == run_id.as_str(), "{result}");
    let _ = owner.cancel().await;
    Ok(())
}

/// A valid bearer for another DID reads with that DID as its subject: the
/// listing is that DID's own, empty listing, and the run views apply their
/// unchanged observer rule with that DID as the actor. This pins the read
/// subject, not authorization: the graph collections declare no document
/// policy, so DefraDB admits these reads, and the anonymous `query` tool on
/// the same `/mcp` reads the same documents.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_graph_tools_take_the_subject_did_from_the_bearer() -> Result<()> {
    let served =
        McpGraphHome::start("mcp-graph-subject", &["--enable-mcp", "--mcp-graph-tools"]).await?;
    let run_id = start_prepared_graph_run(&served).await?;
    let other = gents::KeyIdentity::load_or_create(served.tempdir.path().join("other.key"), None)?;
    let bearer = gents::identity::defradb_bearer_authorization(other.did(), &served.audience())?;
    let client = served.client(Some(&bearer)).await?;
    let listed = call_mcp_tool(&client, "list_graphs", json!({}))
        .await?
        .map_err(|refusal| {
            anyhow::anyhow!(
                "DefraDB refused another DID's listing, so a non-admin bearer cannot read the graph collections: {refusal}"
            )
        })?;
    anyhow::ensure!(
        listed["node_did"] == other.did() && listed["graphs"] == json!([]),
        "the listing is the bearer DID's own: {listed}"
    );
    for tool in ["get_graph_run", "get_graph_result"] {
        let refused = call_mcp_tool(&client, tool, json!({"run_id": run_id})).await?;
        anyhow::ensure!(
            matches!(&refused, Err(message) if message.contains("actor is not authorized to observe this graph run")),
            "{tool}: the run view takes the bearer's DID as its actor: {refused:?}"
        );
    }
    let _ = client.cancel().await;
    Ok(())
}
