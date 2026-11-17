use super::support::*;
use super::*;
use crate::support::live_inference::{rows_when, STALL_CEILING};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tracing::instrument::WithSubscriber;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

const LOCK_CHILD_ENV: &str = "GENTS_TEST_PLUGIN_PUBLICATION_LOCK_HOME";
const LOCK_CHILD_TEST: &str =
    "runtime::agent::runtime::tests::plugin_publication::plugin_publication_lock_child";
const LOCKED: &str = "plugin-publication-lock-held";

#[tokio::test]
async fn plugin_publication_lock_child() {
    let Some(home) = std::env::var_os(LOCK_CHILD_ENV) else {
        return;
    };
    let guard = gents_pack::plugin::store::try_lock_store(std::path::Path::new(&home))
        .expect("child owns the plugin store lock");
    let mut stdout = tokio::io::stdout();
    stdout
        .write_all(format!("{LOCKED}\n").as_bytes())
        .await
        .unwrap();
    stdout.flush().await.unwrap();
    let mut release = [0];
    tokio::io::stdin().read_exact(&mut release).await.unwrap();
    drop(guard);
}

struct PublicationWaitCapture(mpsc::UnboundedSender<()>);

impl<S> Layer<S> for PublicationWaitCapture
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _context: Context<'_, S>) {
        if event.metadata().target() == gents_tools::surface::PLUGIN_PUBLICATION_WAIT_EVENT_TARGET {
            let _ = self.0.send(());
        }
    }
}

#[tokio::test]
async fn request_submitted_while_another_process_holds_the_plugin_store_completes() {
    crate::test_support::root::enable_scoped_event_capture();
    let endpoint = crate::support::mock_endpoint::MockModelEndpoint::start("default").unwrap();
    let fixture = plugin_runtime_fixture(
        "plugin-publication-request",
        "backend-publication-request",
        endpoint.endpoint(),
    )
    .await
    .unwrap();
    let node = fixture.node.clone();
    let node_did = fixture.runtime.did().to_owned();
    let agent_did = fixture.node_binding.default_agent_did.clone().unwrap();
    let plugin_home = tempfile::tempdir().unwrap();
    super::plugin_readmission::install_fixture_plugin(plugin_home.path());
    let record =
        gents_pack::plugin::store::read_record(plugin_home.path(), "fixture", "list_files")
            .unwrap();
    let tools = serde_json::from_value(serde_json::json!({
        "tools_id": format!("{agent_did}:tools"),
        "node_did": node_did,
        "integrations": {"plugins": [{"plugin": "fixture/list_files", "digest": record.digest}]}
    }))
    .unwrap();
    gents_documents::write_tools_document(
        &fixture.user_access,
        &tools,
        &gents_inference::resolved::ProviderProfilePolicy,
    )
    .await
    .unwrap();
    let agent = crate::Gents::from_default_agent_documents(
        node.clone(),
        fixture.runtime.clone(),
        crate::runtime::agent::DocumentRuntimeOptions {
            backend_admission: Some(fixture.admission()),
            tool_ceiling: ToolCeiling::meta_only(),
            plugin_home: Some(plugin_home.path().to_path_buf()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let requester = agent
        .agents
        .iter()
        .find(|a| a.agent.agent_did == agent_did)
        .unwrap()
        .agent
        .agent_identity
        .clone();
    grant_held_agent_operations(&node, fixture.runtime.as_ref(), requester.as_ref()).await;

    create_user_agent_session(
        &fixture.user_access,
        &node_did,
        Some(&agent_did),
        "session-during-publication",
        &agent_did,
        Some(&agent_did),
    )
    .await;

    let (waiting_tx, mut waiting_rx) = mpsc::unbounded_channel();
    let subscriber = Registry::default().with(PublicationWaitCapture(waiting_tx));
    let (published, mut snapshots) = mpsc::unbounded_channel();
    let capture = Arc::new(PublicationReadinessCapture {
        node: node.clone(),
        snapshots: Default::default(),
        published,
    });
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let run = tokio::spawn(
        super::super::startup::run_agent_with_readiness_writer(
            agent,
            shutdown_rx,
            capture,
            Duration::from_millis(1),
        )
        .with_subscriber(subscriber),
    );
    tokio::time::timeout(STALL_CEILING, async {
        loop {
            let snapshot = snapshots.recv().await.expect("readiness writer stopped");
            if snapshot.process_state == NodeReadinessProcessState::Ready
                && snapshot.agents.iter().any(|entry| {
                    entry.agent_did == agent_did && entry.state == AgentReadinessState::Ready
                })
            {
                break;
            }
        }
    })
    .await
    .expect("node never reached READY before publication");
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            LOCK_CHILD_TEST,
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(LOCK_CHILD_ENV, plugin_home.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut output = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
    tokio::time::timeout(STALL_CEILING, async {
        loop {
            let line = output
                .next_line()
                .await
                .unwrap()
                .expect("lock child exited before acquiring lock");
            if line.contains(LOCKED) {
                break;
            }
        }
    })
    .await
    .expect("lock child stalled");

    merge_fixture_document(
        &fixture.user_access,
        "test.ready_publication.wake",
        gents_store::collection::Collection::AgentContext,
        &node_did,
        &format!("{agent_did}:context"),
        serde_json::json!({"system_prompt":"changed while ready and publishing"}),
    )
    .await;
    tokio::time::timeout(STALL_CEILING, waiting_rx.recv())
        .await
        .expect("surface constructor never observed contention")
        .expect("publication observation channel closed");
    let request_doc_id = create_held_agent_request(
        &node,
        &node_did,
        requester.as_ref(),
        "request-during-publication",
        "session-during-publication",
        "hello",
    )
    .await;
    let query = format!(
        r#"{{ AgentRequest(filter: {{_docID: {{_eq: "{}"}}}}) {{lifecycle_state failure_reason}} }}"#,
        escape_graphql_string(&request_doc_id)
    );
    let pending = fixture.agent_access.execute(&query).await.unwrap();
    assert!(
        !matches!(
            pending["data"]["AgentRequest"][0]["lifecycle_state"].as_str(),
            Some("failed" | "dead")
        ),
        "{pending}"
    );
    child.stdin.take().unwrap().write_all(&[1]).await.unwrap();
    assert!(tokio::time::timeout(STALL_CEILING, child.wait())
        .await
        .unwrap()
        .unwrap()
        .success());

    let terminal = rows_when(&fixture.agent_access, "AgentRequest", &query, |data| {
        data["AgentRequest"][0]["lifecycle_state"]
            .as_str()
            .is_some_and(|state| matches!(state, "completed" | "failed" | "dead" | "interrupted"))
    })
    .await;
    assert_eq!(
        terminal["AgentRequest"][0]["lifecycle_state"],
        "completed",
        "{terminal}; mock requests: {:?}",
        endpoint
            .recorded_requests()
            .iter()
            .map(|request| &request.path)
            .collect::<Vec<_>>()
    );
    let readiness = fetch_node_readiness(&node, &node_did).await;
    assert!(
        readiness
            .agents
            .iter()
            .any(|entry| entry.agent_did == agent_did && entry.state == AgentReadinessState::Ready),
        "{readiness:?}"
    );
    let _ = shutdown_tx.send(true);
    tokio::time::timeout(STALL_CEILING, run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

async fn configured_publication_agent(
    fixture: &PluginRuntimeFixture,
    home: &std::path::Path,
) -> crate::Gents {
    super::plugin_readmission::install_fixture_plugin(home);
    let record = gents_pack::plugin::store::read_record(home, "fixture", "list_files").unwrap();
    let agent_did = fixture.node_binding.default_agent_did.as_ref().unwrap();
    let tools: Tools = serde_json::from_value(serde_json::json!({
        "tools_id":format!("{agent_did}:tools"), "node_did":fixture.runtime.did(),
        "integrations":{"plugins":[{"plugin":"fixture/list_files", "digest":record.digest}]}
    }))
    .unwrap();
    gents_documents::write_tools_document(
        &fixture.user_access,
        &tools,
        &gents_inference::resolved::ProviderProfilePolicy,
    )
    .await
    .unwrap();
    crate::Gents::from_default_agent_documents(
        fixture.node.clone(),
        fixture.runtime.clone(),
        crate::runtime::agent::DocumentRuntimeOptions {
            backend_admission: Some(fixture.admission()),
            tool_ceiling: ToolCeiling::meta_only(),
            plugin_home: Some(home.to_owned()),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn publication_retry_recovers_after_the_settle_window_without_another_event() {
    bounded_publication_case(PublicationChange::None).await;
}

#[tokio::test]
async fn busy_publication_cannot_defer_a_committed_revocation_and_honours_shutdown() {
    bounded_publication_case(PublicationChange::DisableAgent).await;
}

#[tokio::test]
async fn busy_publication_applies_a_tools_revocation_before_the_writer_releases() {
    bounded_publication_case(PublicationChange::RevokePlugin).await;
}

enum PublicationChange {
    None,
    DisableAgent,
    RevokePlugin,
}

async fn bounded_publication_case(change: PublicationChange) {
    crate::test_support::root::enable_scoped_event_capture();
    let endpoint = MockModelEndpoint::start("default").unwrap();
    let fixture = plugin_runtime_fixture(
        "publication-rollback",
        "backend-publication-rollback",
        endpoint.endpoint(),
    )
    .await
    .unwrap();
    let home = tempfile::tempdir().unwrap();
    let agent = configured_publication_agent(&fixture, home.path()).await;
    if matches!(change, PublicationChange::DisableAgent) {
        choose_spare_default(&fixture, agent.default_agent_did()).await;
    }
    let context = agent.document_runtime_context().cloned().unwrap();
    let node_did = fixture.runtime.did().to_owned();
    let agent_did = agent.default_agent_did().to_owned();
    let runtime_status = RuntimeStatusHandle::new(fixture.node.clone(), &node_did);
    runtime_status.initialize_startup(&agent_did).await.unwrap();
    let tool_runtime = gents_tools::surface::ToolRuntimeContext::new_with_node_did(
        fixture.node.clone(),
        gents_tools::mcp::pool::McpPool::default(),
        gents_tools::mcp::health::ServiceHealthMap::default(),
        "localhost",
        None,
        &node_did,
        Some(context.identity.clone()),
    )
    .with_plugins(context.plugins.clone());
    let guard = gents_pack::plugin::store::try_lock_store(home.path()).unwrap();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (proposal_tx, mut proposal_rx) = mpsc::channel(4);
    let (health_tx, health_rx) = mpsc::channel(1);
    let watcher = tokio::spawn(run_control_watcher_with_timing(
        ControlWatcherInputs {
            node: fixture.node.clone(),
            backend_admission: fixture.admission(),
            subscription: subscribe_control_changes(&fixture.node).unwrap(),
            node_did: node_did.clone(),
            resolve_context: context,
            tool_runtime,
            proposals_tx: proposal_tx,
            runtime_status,
            health_events_rx: health_rx,
            shutdown: shutdown_rx,
        },
        ControlWatcherTiming {
            publication_wait: Duration::from_millis(20),
            debounce: Duration::from_millis(1),
            local_debounce: Duration::from_millis(1),
            settle_retry: Duration::from_millis(1),
            settle_window: Duration::from_millis(1),
            idle_sleep: Duration::from_secs(3600),
        },
    ));
    match change {
        PublicationChange::None => {}
        PublicationChange::DisableAgent => {
            merge_fixture_document(
                &fixture.user_access,
                "test.publication.revoke_agent",
                gents_store::collection::Collection::Agent,
                &node_did,
                &agent_did,
                serde_json::json!({"enabled":false}),
            )
            .await;
        }
        PublicationChange::RevokePlugin => {
            merge_fixture_document(
                &fixture.user_access,
                "test.publication.revoke_plugin",
                gents_store::collection::Collection::Tools,
                &node_did,
                &format!("{agent_did}:tools"),
                serde_json::json!({"integrations":{"plugins":null}}),
            )
            .await;
        }
    }
    health_tx.send(()).await.unwrap();
    let demotion = tokio::time::timeout(STALL_CEILING, proposal_rx.recv())
        .await
        .unwrap()
        .unwrap();
    if !matches!(change, PublicationChange::None) {
        match change {
            PublicationChange::DisableAgent => {
                assert_eq!(
                    demotion.unavailable_agents[&agent_did].public_reason,
                    AgentReadinessUnavailableReason::AgentDisabled
                );
                assert!(!demotion.agents.contains_key(&agent_did));
            }
            PublicationChange::RevokePlugin => {
                assert!(demotion.agents.contains_key(&agent_did));
                assert!(demotion.tool_surfaces[&agent_did]
                    .plugin_resolutions()
                    .is_empty());
            }
            PublicationChange::None => unreachable!(),
        }
        assert!(
            gents_pack::plugin::store::try_lock_store_shared(home.path()).is_err(),
            "revocation must precede lock release"
        );
        let _ = shutdown_tx.send(true);
        tokio::time::timeout(STALL_CEILING, watcher)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        drop(guard);
        return;
    }
    let cause = &demotion.unavailable_agents[&agent_did].diagnostic;
    assert!(cause.contains("plugin store is busy"), "{cause}");
    drop(guard); // Rollback/release emits no new document or health event.
    let proposal = tokio::time::timeout(STALL_CEILING, proposal_rx.recv())
        .await
        .expect("publication retry was stranded after settle expiry")
        .unwrap();
    assert!(proposal.agents.contains_key(&agent_did));
    assert!(!proposal.unavailable_agents.contains_key(&agent_did));
    let _ = shutdown_tx.send(true);
    tokio::time::timeout(STALL_CEILING, watcher)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

struct PublicationReadinessCapture {
    node: Arc<defra_node::Node>,
    snapshots: std::sync::Mutex<Vec<NodeReadinessSnapshot>>,
    published: mpsc::UnboundedSender<NodeReadinessSnapshot>,
}

#[async_trait::async_trait]
impl crate::runtime::node_readiness_publisher::NodeReadinessWriter for PublicationReadinessCapture {
    async fn upsert(
        &self,
        node_did: &str,
        snapshot: &NodeReadinessSnapshot,
        updated_at: &str,
    ) -> anyhow::Result<()> {
        crate::runtime::node_readiness_publisher::upsert_node_readiness(
            &self.node, node_did, snapshot, updated_at,
        )
        .await?;
        self.snapshots.lock().unwrap().push(snapshot.clone());
        let _ = self.published.send(snapshot.clone());
        Ok(())
    }
}

#[tokio::test]
async fn named_pin_update_never_publishes_an_unavailable_agent() {
    crate::test_support::root::enable_scoped_event_capture();
    let endpoint = crate::support::mock_endpoint::MockModelEndpoint::start("default").unwrap();
    let fixture = plugin_runtime_fixture(
        "publication-pin-update",
        "backend-publication-pin",
        endpoint.endpoint(),
    )
    .await
    .unwrap();
    let home = tempfile::tempdir().unwrap();
    let agent = configured_publication_agent(&fixture, home.path()).await;
    let node_did = fixture.runtime.did().to_owned();
    let agent_did = agent.default_agent_did().to_owned();
    let tools_id = format!("{agent_did}:tools");
    let stale_pair = agent
        .agents
        .iter()
        .find(|pair| pair.agent.agent_did == agent_did)
        .unwrap()
        .clone();
    let resolve_context = agent.document_runtime_context().cloned().unwrap();
    let stale_surface = Arc::new(
        stale_pair
            .tools
            .resolve_with_available_agent_targets(
                &fixture.node,
                &node_did,
                &std::iter::once(agent_did.clone()).collect(),
                &*resolve_context.plugins,
            )
            .await
            .unwrap(),
    );
    let requester = stale_pair.agent.agent_identity.clone();
    grant_held_agent_operations(&fixture.node, fixture.runtime.as_ref(), requester.as_ref()).await;
    create_user_agent_session(
        &fixture.user_access,
        &node_did,
        Some(&agent_did),
        "pin-commit-session",
        &agent_did,
        Some(&agent_did),
    )
    .await;

    let old_plugin =
        gents_pack::plugin::store::read_record(home.path(), "fixture", "list_files").unwrap();
    let omitted: Tools = serde_json::from_value(serde_json::json!({
        "tools_id":"not-named-by-update", "node_did":node_did,
        "integrations":{"plugins":[{"plugin":"fixture/list_files","digest":old_plugin.digest}]}
    }))
    .unwrap();
    gents_documents::write_tools_document(
        &fixture.user_access,
        &omitted,
        &gents_inference::resolved::ProviderProfilePolicy,
    )
    .await
    .unwrap();
    let (published, mut snapshots) = mpsc::unbounded_channel();
    let capture = Arc::new(PublicationReadinessCapture {
        node: fixture.node.clone(),
        snapshots: Default::default(),
        published,
    });
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (wait_tx, mut wait_rx) = mpsc::unbounded_channel();
    let subscriber = Registry::default().with(PublicationWaitCapture(wait_tx));
    let run = tokio::spawn(
        super::super::startup::run_agent_with_readiness_writer(
            agent,
            shutdown_rx,
            capture.clone(),
            Duration::from_millis(1),
        )
        .with_subscriber(subscriber),
    );
    let first_generation = tokio::time::timeout(STALL_CEILING, async {
        loop {
            let snapshot = snapshots.recv().await.unwrap();
            if snapshot
                .agents
                .iter()
                .any(|a| a.agent_did == agent_did && a.state == AgentReadinessState::Ready)
            {
                break snapshot.active_generation;
            }
        }
    })
    .await
    .unwrap();
    let (_directory, pack_root) = gents_pack::test_support::fixture_pack_copy(
        "bind_plugin_fixture",
        &serde_json::json!({"version":2}),
    );
    let (bytes, _) = gents_pack::pack::archive::pack_dir(&pack_root).unwrap();
    let archive = gents_pack::pack::archive::PackArchive::from_bytes(&bytes).unwrap();
    let replacements = gents_pack::pack::pin_moves::replacement_digests([&archive]).unwrap();
    let edits = gents_pack::pack::pin_moves::prepare(
        &fixture.user_access,
        &node_did,
        std::slice::from_ref(&tools_id),
        &replacements,
    )
    .await
    .unwrap();
    let access = fixture.user_access.clone();
    let owner = node_did.clone();
    let publication_home = home.path().to_owned();
    // The same callback config pack update uses: Tools effects and the pack
    // record are signed in one transaction while the plugin stage is fenced.
    let install_home = home.path().to_owned();
    let install_owner = node_did.clone();
    let (staged_tx, staged_rx) = tokio::sync::oneshot::channel();
    let (continue_tx, continue_rx) = tokio::sync::oneshot::channel();
    let (committed_tx, committed_rx) = tokio::sync::oneshot::channel();
    let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
    let install = tokio::spawn(async move {
        gents_pack::pack::plugin_lifecycle::install(
            &install_home,
            &archive,
            false,
            Some(&install_owner),
            &Default::default(),
            gents_pack::pack::InstalledVia::Operator,
            gents_pack::pack::PackSource::Local { file_name: None },
            move |identity, changes| async move {
                let _ = staged_tx.send(());
                continue_rx.await.expect("test released the signed install");
                gents_pack::pack::record_staged_plugin_store_change_with_pins(
                    &access,
                    &owner,
                    &publication_home,
                    &identity.coordinate,
                    Some(&identity),
                    &changes,
                    &edits,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await?;
                let _ = committed_tx.send(());
                finish_rx.await.expect("release committed publication");
                Ok(())
            },
        )
        .await
        .unwrap();
    });
    tokio::time::timeout(STALL_CEILING, staged_rx)
        .await
        .unwrap()
        .unwrap();
    // Wake the control reader while the install has staged new bytes but
    // has not committed its pin. This is the old-pin/new-record race.
    merge_fixture_document(
        &fixture.user_access,
        "test.pin_update.wake",
        gents_store::collection::Collection::AgentContext,
        &node_did,
        &format!("{agent_did}:context"),
        serde_json::json!({"system_prompt":"prompt edited during plugin publication"}),
    )
    .await;
    tokio::time::timeout(STALL_CEILING, wait_rx.recv())
        .await
        .unwrap()
        .unwrap();
    continue_tx.send(()).unwrap();
    tokio::time::timeout(STALL_CEILING, committed_rx)
        .await
        .unwrap()
        .unwrap();
    // Native pins have committed but the stage still holds its fence, so
    // the watcher cannot swap surfaces before this request is submitted.
    let request_id = create_held_agent_request(
        &fixture.node,
        &node_did,
        requester.as_ref(),
        "between-commit-and-swap",
        "pin-commit-session",
        "hello after commit",
    )
    .await;
    finish_tx.send(()).unwrap();
    tokio::time::timeout(STALL_CEILING, install)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(STALL_CEILING, async {
        loop {
            let snapshot = snapshots.recv().await.unwrap();
            if snapshot.active_generation > first_generation
                && snapshot
                    .agents
                    .iter()
                    .any(|a| a.agent_did == agent_did && a.state == AgentReadinessState::Ready)
            {
                break;
            }
        }
    })
    .await
    .expect("updated pin was not admitted");
    let tool_runtime = gents_tools::surface::ToolRuntimeContext::new_with_node_did(
        fixture.node.clone(),
        Default::default(),
        Default::default(),
        "localhost",
        None,
        &node_did,
        Some(resolve_context.identity.clone()),
    )
    .with_plugins(resolve_context.plugins.clone());
    // A slot proposal resolved before the install still has the old pin.
    // It must refresh once from current documents, never accept new bytes
    // against that old pin or consume its failure budget on the mismatch.
    let refreshed = super::super::context::build_slot_surface_fenced(
        &tool_runtime,
        Some(&resolve_context),
        stale_pair.clone(),
        stale_surface.clone(),
    )
    .await
    .unwrap();
    for (pin, installed) in refreshed.surface.plugin_resolutions() {
        assert_eq!(
            pin.digest.as_deref(),
            Some(replacements[&pin.plugin].as_str())
        );
        assert_eq!(
            installed.as_ref().unwrap().digest,
            replacements[&pin.plugin]
        );
    }
    let request_query = format!(
        r#"{{AgentRequest(filter: {{_docID: {{_eq: "{}"}}}}) {{lifecycle_state failure_reason}}}}"#,
        escape_graphql_string(&request_id)
    );
    let terminal = rows_when(
        &fixture.agent_access,
        "AgentRequest",
        &request_query,
        |data| {
            matches!(
                data["AgentRequest"][0]["lifecycle_state"].as_str(),
                Some("completed" | "failed" | "dead")
            )
        },
    )
    .await;
    assert_eq!(
        terminal["AgentRequest"][0]["lifecycle_state"], "completed",
        "{terminal}"
    );
    let all = capture.snapshots.lock().unwrap().clone();
    assert!(
        !all.iter()
            .filter(|snapshot| snapshot.active_generation >= first_generation)
            .any(|snapshot| snapshot.agents.iter().any(|entry| {
                entry.agent_did == agent_did && entry.state == AgentReadinessState::Unavailable
            })),
        "agent became unavailable during atomic pin update: {all:?}"
    );
    let query = format!(
        r#"{{ Tools(filter: {{tools_id: {{_eq: "{}"}}}}) {{integrations}} }}"#,
        escape_graphql_string(&tools_id)
    );
    let tools = fixture.user_access.execute(&query).await.unwrap();
    assert_eq!(
        tools["data"]["Tools"][0]["integrations"]["plugins"][0]["digest"],
        replacements["fixture/list_files"]
    );
    let untouched = fixture
        .user_access
        .execute(r#"{Tools(filter: {tools_id: {_eq: "not-named-by-update"}}) {integrations}}"#)
        .await
        .unwrap();
    assert_eq!(
        untouched["data"]["Tools"][0]["integrations"],
        serde_json::to_value(omitted.integrations).unwrap()
    );
    let _ = shutdown_tx.send(true);
    tokio::time::timeout(STALL_CEILING, run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    choose_spare_default(&fixture, &agent_did).await;
    merge_fixture_document(
        &fixture.user_access,
        "test.stale_pin.revoked_agent",
        gents_store::collection::Collection::Agent,
        &node_did,
        &agent_did,
        serde_json::json!({"enabled":false}),
    )
    .await;
    let error = super::super::context::build_slot_surface_fenced(
        &tool_runtime,
        Some(&resolve_context),
        stale_pair,
        stale_surface,
    )
    .await
    .err()
    .expect("refresh must reject a now-disabled agent");
    assert!(format!("{error:#}").contains("revoked or became unavailable"));
}

#[tokio::test]
async fn startup_shutdown_cancels_publication_wait_without_releasing_the_writer() {
    crate::test_support::root::enable_scoped_event_capture();
    let endpoint = MockModelEndpoint::start("default").unwrap();
    let fixture = plugin_runtime_fixture(
        "shutdown-publication",
        "backend-shutdown-publication",
        endpoint.endpoint(),
    )
    .await
    .unwrap();
    let home = tempfile::tempdir().unwrap();
    let agent = configured_publication_agent(&fixture, home.path()).await;
    let guard = gents_pack::plugin::store::try_lock_store(home.path()).unwrap();
    let (wait_tx, mut wait_rx) = mpsc::unbounded_channel();
    let subscriber = Registry::default().with(PublicationWaitCapture(wait_tx));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let run = tokio::spawn(agent.run(shutdown_rx).with_subscriber(subscriber));
    tokio::time::timeout(STALL_CEILING, wait_rx.recv())
        .await
        .unwrap()
        .unwrap();
    shutdown_tx.send(true).unwrap();
    tokio::time::timeout(STALL_CEILING, run)
        .await
        .expect("shutdown must interrupt the 60s publication wait")
        .unwrap()
        .unwrap();
    assert!(
        gents_pack::plugin::store::try_lock_store_shared(home.path()).is_err(),
        "test still owns writer exclusion"
    );
    drop(guard);
}

/// Revocation is valid only after a different enabled default is selected.
/// Exercise the signed configuration owner rather than bypass its invariant.
async fn choose_spare_default(fixture: &PluginRuntimeFixture, previous: &str) -> String {
    let actor = fixture.user_access.actor_did().unwrap().parse().unwrap();
    let binding = gents_store::identity::provision_agent_identity_authorized(
        &fixture.node,
        fixture.runtime.as_ref(),
        &actor,
    )
    .await
    .unwrap();
    let owner = fixture.runtime.did();
    let documents = vec![
        (gents_store::collection::Collection::Agent, serde_json::json!({
            "node_did":owner, "agent_did":binding.agent_did, "creation_nonce":binding.creation_nonce,
            "inference_profile_id":format!("{previous}:inference"),
        })),
        (gents_store::collection::Collection::DefaultAgentSelection, serde_json::json!({
            "node_did":owner, "default_agent_did":binding.agent_did,
        })),
    ].into_iter().map(|(collection, value)| gents_documents::DesiredStateApplyDocument {
        collection, add:value.clone(), update:value,
    }).collect();
    let plan = gents_documents::DesiredStateApplyPlan::new(documents).unwrap();
    fixture
        .user_access
        .transact_write("test.publication.spare_default", |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .unwrap();
    binding.agent_did
}

#[derive(Debug)]
enum SupervisorPublicationEvent {
    Started(String, u64, bool),
    Retired(String, u64),
}

struct RetiredPublicationSlot(
    mpsc::UnboundedSender<SupervisorPublicationEvent>,
    String,
    u64,
);
impl Drop for RetiredPublicationSlot {
    fn drop(&mut self) {
        let _ = self
            .0
            .send(SupervisorPublicationEvent::Retired(self.1.clone(), self.2));
    }
}

#[tokio::test]
async fn supervisor_retires_revoked_plugin_slots_keeps_unrelated_agents_and_recovers_after_release()
{
    use crate::runtime::agent::reconcile::GenerationSupervisor;
    let endpoint = MockModelEndpoint::start("default").unwrap();
    let fixture = plugin_runtime_fixture(
        "supervised-publication",
        "backend-supervised-publication",
        endpoint.endpoint(),
    )
    .await
    .unwrap();
    let home = tempfile::tempdir().unwrap();
    let agent = configured_publication_agent(&fixture, home.path()).await;
    let original = agent.default_agent_did().to_owned();
    let unrelated = choose_spare_default(&fixture, &original).await;
    let survivor = choose_spare_default(&fixture, &original).await;
    let owner = fixture.runtime.did().to_owned();
    let record =
        gents_pack::plugin::store::read_record(home.path(), "fixture", "list_files").unwrap();
    let documents = vec![
        (gents_store::collection::Collection::Tools, serde_json::json!({
            "node_did":owner, "tools_id":format!("{survivor}:tools"),
            "integrations":{"plugins":[{"plugin":"fixture/list_files","digest":record.digest}]},
        })),
        (gents_store::collection::Collection::AgentContext, serde_json::json!({
            "node_did":owner,"context_id":format!("{survivor}:context"),"tools_id":format!("{survivor}:tools"),
        })),
    ].into_iter().map(|(collection,value)|gents_documents::DesiredStateApplyDocument {collection,add:value.clone(),update:value}).collect();
    let plan = gents_documents::DesiredStateApplyPlan::new(documents).unwrap();
    fixture
        .user_access
        .transact_write("test.supervisor.survivor_config", |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .unwrap();
    merge_fixture_document(
        &fixture.user_access,
        "test.supervisor.plugin_survivor",
        gents_store::collection::Collection::Agent,
        &owner,
        &survivor,
        serde_json::json!({"context_id":format!("{survivor}:context")}),
    )
    .await;
    let context = agent.document_runtime_context().cloned().unwrap();
    let view = crate::runtime::agent::document_view::load_document_runtime_view_as(
        &fixture.node,
        &owner,
        context.identity.did(),
    )
    .await
    .unwrap();
    let initial =
        crate::runtime::agent::document_view::resolve_document_runtime_snapshot_from_view(
            &fixture.node,
            &context,
            &view,
        )
        .await
        .unwrap();
    assert_eq!(initial.agents.len(), 3);
    let tool_runtime = gents_tools::surface::ToolRuntimeContext::new_with_node_did(
        fixture.node.clone(),
        Default::default(),
        Default::default(),
        "localhost",
        None,
        &owner,
        Some(context.identity.clone()),
    )
    .with_plugins(context.plugins.clone());
    let status = RuntimeStatusHandle::new(fixture.node.clone(), &owner);
    status.initialize_startup(&survivor).await.unwrap();
    let (events_tx, mut events) = mpsc::unbounded_channel();
    let runner = {
        let runtime = tool_runtime.clone();
        let resolve = context.clone();
        move |pair: gents_tools::surface::AgentWithTools,
              surface: Arc<gents_tools::surface::ToolSurface>,
              requests: Arc<
            tokio::sync::Mutex<mpsc::Receiver<gents_protocol::row::AgentRequest>>,
        >,
              _capacity: Arc<crate::runtime::agent::worker_capacity::WorkerCapacity>,
              generation: u64,
              mut shutdown: watch::Receiver<bool>| {
            let runtime = runtime.clone();
            let resolve = resolve.clone();
            let events = events_tx.clone();
            async move {
                let did = pair.agent.agent_did.clone();
                let _retirement = RetiredPublicationSlot(events.clone(), did.clone(), generation);
                let prepared = super::super::context::build_slot_surface_fenced(
                    &runtime,
                    Some(&resolve),
                    pair,
                    surface,
                )
                .await?;
                let _ = events.send(SupervisorPublicationEvent::Started(
                    did,
                    generation,
                    prepared.surface.has_plugin_tools(),
                ));
                let mut requests = requests.lock().await;
                loop {
                    tokio::select! {
                        _ = shutdown.changed() => break,
                        request = requests.recv() => if request.is_none() { break; },
                    }
                }
                anyhow::Ok(())
            }
        }
    };
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let supervisor = GenerationSupervisor::bootstrap(
        initial,
        gents_inference::admission::AdmissionRegistry::new(fixture.node.clone()),
        crate::runtime::agent::retry::RetryPolicy {
            max_retries: 3,
            base_delay_ms: 1,
            max_delay_ms: 2,
        },
        runner,
        status.clone(),
        shutdown_rx.clone(),
        None,
    )
    .await
    .unwrap();
    let unrelated_dispatcher = supervisor.current_snapshot().dispatchers[&unrelated].clone();
    let (active_tx, mut active_rx) = watch::channel(supervisor.current_snapshot());
    let (proposals_tx, proposals_rx) = mpsc::channel(4);
    let supervising = tokio::spawn(supervisor.run(active_tx, proposals_rx, shutdown_rx.clone()));
    let mut observed = Vec::new();
    tokio::time::timeout(STALL_CEILING, async {
        while observed
            .iter()
            .filter(|e| matches!(e, SupervisorPublicationEvent::Started(_, 1, _)))
            .count()
            < 3
        {
            observed.push(events.recv().await.unwrap());
        }
    })
    .await
    .unwrap();
    let guard = gents_pack::plugin::store::try_lock_store(home.path()).unwrap();
    let (health_tx, health_rx) = mpsc::channel(1);
    let watcher = tokio::spawn(run_control_watcher_with_timing(
        ControlWatcherInputs {
            node: fixture.node.clone(),
            backend_admission: fixture.admission(),
            subscription: subscribe_control_changes(&fixture.node).unwrap(),
            node_did: owner.clone(),
            resolve_context: context,
            tool_runtime,
            proposals_tx,
            runtime_status: status,
            health_events_rx: health_rx,
            shutdown: shutdown_rx,
        },
        ControlWatcherTiming {
            publication_wait: Duration::from_millis(20),
            debounce: Duration::from_millis(1),
            local_debounce: Duration::from_millis(1),
            settle_retry: Duration::from_millis(1),
            settle_window: Duration::from_millis(1),
            idle_sleep: Duration::from_secs(3600),
        },
    ));
    merge_fixture_document(
        &fixture.user_access,
        "test.supervisor.revoke_plugin",
        gents_store::collection::Collection::Tools,
        &owner,
        &format!("{original}:tools"),
        serde_json::json!({"integrations":{"plugins":null}}),
    )
    .await;
    health_tx.send(()).await.unwrap();
    tokio::time::timeout(STALL_CEILING, async {
        loop {
            active_rx.changed().await.unwrap();
            let snapshot = active_rx.borrow_and_update();
            if snapshot.generation > 1
                && snapshot.dispatchers.contains_key(&original)
                && !snapshot.tool_surfaces[&original].has_plugin_tools()
                && snapshot.unavailable_agents.contains_key(&survivor)
            {
                assert!(snapshot.dispatchers[&unrelated].same_channel(&unrelated_dispatcher));
                break;
            }
        }
    })
    .await
    .expect("supervisor did not apply plugin revocation before release");
    tokio::time::timeout(STALL_CEILING, async {
        loop {
            observed.push(events.recv().await.unwrap());
            let retired = observed.iter().any(|e| matches!(e, SupervisorPublicationEvent::Retired(did, 1) if did == &original));
            let rebuilt = observed.iter().any(|e| matches!(e, SupervisorPublicationEvent::Started(did, generation, false) if did == &original && *generation > 1));
            if retired && rebuilt { break; }
        }
    }).await.expect("revoked slot was not retired and rebuilt without its plugin");
    assert!(gents_pack::plugin::store::try_lock_store_shared(home.path()).is_err());
    drop(guard); // No event after releasing the lease.
    tokio::time::timeout(STALL_CEILING, async {
        loop {
            active_rx.changed().await.unwrap();
            let snapshot = active_rx.borrow_and_update();
            if snapshot.dispatchers.contains_key(&survivor) {
                assert!(snapshot.dispatchers.contains_key(&original));
                assert!(snapshot.dispatchers[&unrelated].same_channel(&unrelated_dispatcher));
                assert!(!snapshot.tool_surfaces[&original].has_plugin_tools());
                break;
            }
        }
        loop {
            if matches!(events.recv().await.unwrap(), SupervisorPublicationEvent::Started(did, generation, true) if did == survivor && generation > 1) { break; }
        }
    }).await.expect("publication release stranded the demoted plugin agent");
    shutdown_tx.send(true).unwrap();
    tokio::time::timeout(STALL_CEILING, watcher)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    tokio::time::timeout(STALL_CEILING, supervising)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn plugin_free_startup_and_slots_remain_ready_while_store_is_exclusively_held() {
    let endpoint = MockModelEndpoint::start("default").unwrap();
    let fixture = plugin_runtime_fixture(
        "plugin-free-publication",
        "backend-plugin-free-publication",
        endpoint.endpoint(),
    )
    .await
    .unwrap();
    let home = tempfile::tempdir().unwrap();
    let agent = configured_publication_agent(&fixture, home.path()).await;
    let did = agent.default_agent_did().to_owned();
    merge_fixture_document(
        &fixture.user_access,
        "test.plugin_free.tools",
        gents_store::collection::Collection::Tools,
        fixture.runtime.did(),
        &format!("{did}:tools"),
        serde_json::json!({"integrations":{"plugins":null}}),
    )
    .await;
    let guard = gents_pack::plugin::store::try_lock_store(home.path()).unwrap();
    let (published, mut snapshots) = mpsc::unbounded_channel();
    let capture = Arc::new(PublicationReadinessCapture {
        node: fixture.node.clone(),
        snapshots: Default::default(),
        published,
    });
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let run = tokio::spawn(super::super::startup::run_agent_with_readiness_writer(
        agent,
        shutdown_rx,
        capture.clone(),
        Duration::from_millis(1),
    ));
    tokio::time::timeout(STALL_CEILING, async {
        loop {
            let snapshot = snapshots.recv().await.unwrap();
            if snapshot.process_state == NodeReadinessProcessState::Ready
                && snapshot.agents.iter().any(|entry| {
                    entry.agent_did == did && entry.state == AgentReadinessState::Ready
                })
            {
                break;
            }
        }
    })
    .await
    .expect("plugin-free startup waited on the plugin publication lock");
    assert!(gents_pack::plugin::store::try_lock_store_shared(home.path()).is_err());
    shutdown_tx.send(true).unwrap();
    tokio::time::timeout(STALL_CEILING, run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let all = capture.snapshots.lock().unwrap().clone();
    assert!(
        !all.iter()
            .filter(|snapshot| snapshot.active_generation > 0
                && !matches!(
                    snapshot.process_state,
                    NodeReadinessProcessState::ShuttingDown | NodeReadinessProcessState::Shutdown
                ))
            .any(|snapshot| snapshot
                .agents
                .iter()
                .any(|entry| entry.agent_did == did
                    && entry.state == AgentReadinessState::Unavailable)),
        "plugin-free snapshots: {all:#?}"
    );
    drop(guard);
}

#[tokio::test]
async fn real_busy_park_wakes_idle_watcher_and_forces_unchanged_recovery() {
    real_policy_publication_recovery(false, false).await;
}

#[tokio::test]
async fn real_busy_park_recovers_after_holding_past_the_watcher_budget() {
    real_policy_publication_recovery(false, true).await;
}

#[tokio::test]
async fn mixed_startup_exhaustion_starts_plugin_free_agents_and_recovers_plugins() {
    real_policy_publication_recovery(true, true).await;
}

async fn real_policy_publication_recovery(mixed_startup: bool, hold_past_budget: bool) {
    use super::super::startup::slot_failure_policy::StartupSlotFailurePolicy;
    use crate::runtime::agent::reconcile::GenerationSupervisor;
    let endpoint = MockModelEndpoint::start("default").unwrap();
    let fixture = plugin_runtime_fixture(
        "real-policy-publication",
        "backend-real-policy-publication",
        endpoint.endpoint(),
    )
    .await
    .unwrap();
    let home = tempfile::tempdir().unwrap();
    let agent = configured_publication_agent(&fixture, home.path()).await;
    let plugin = agent.default_agent_did().to_owned();
    let free = choose_spare_default(&fixture, &plugin).await;
    let owner = fixture.runtime.did().to_owned();
    let resolve = agent.document_runtime_context().cloned().unwrap();
    let runtime = gents_tools::surface::ToolRuntimeContext::new_with_node_did(
        fixture.node.clone(),
        Default::default(),
        Default::default(),
        "localhost",
        None,
        &owner,
        Some(resolve.identity.clone()),
    )
    .with_plugins(resolve.plugins.clone());
    let status = RuntimeStatusHandle::new(fixture.node.clone(), &owner);
    status.initialize_startup(&free).await.unwrap();
    let guard = if mixed_startup {
        Some(gents_pack::plugin::store::try_lock_store(home.path()).unwrap())
    } else {
        None
    };
    let (subscription, initial) = super::super::startup::resolve_startup_until(
        &agent,
        &runtime,
        &status,
        tokio::time::Instant::now() + Duration::from_millis(20),
    )
    .await
    .expect("publication contention must not abort a mixed startup");
    assert!(initial.agents.contains_key(&free));
    if mixed_startup {
        assert!(!initial.agents.contains_key(&plugin));
        assert_eq!(initial.unavailable_agents[&plugin].public_cause,
            Some(gents_automation::triggers::runtime_snapshot::PublicUnavailableCause::PluginPublicationBusy));
    } else {
        assert!(initial.agents.contains_key(&plugin));
    }
    let barrier = Arc::new(super::super::context::StartupBarrier::new(
        &initial.agents.values().cloned().collect::<Vec<_>>(),
    ));
    let (fatal, mut fatal_rx) = mpsc::unbounded_channel();
    let policy = Arc::new(StartupSlotFailurePolicy {
        barrier: barrier.clone(),
        runtime_status: status.clone(),
        admission_gate: super::super::router::RuntimeAdmissionGate::closed(),
        runtime_cancel: tokio_util::sync::CancellationToken::new(),
        fatal_error: fatal,
        observer: None,
        budget: 1,
    });
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (proposals_tx, mut proposals_rx) = mpsc::channel(8);
    let (health_tx, health_rx) = mpsc::channel(1);
    let watcher = tokio::spawn(run_control_watcher_with_timing(
        ControlWatcherInputs {
            node: fixture.node.clone(),
            backend_admission: fixture.admission(),
            subscription: subscription.unwrap(),
            node_did: owner.clone(),
            resolve_context: resolve.clone(),
            tool_runtime: runtime.clone(),
            proposals_tx,
            runtime_status: status.clone(),
            health_events_rx: health_rx,
            shutdown: shutdown_rx.clone(),
        },
        ControlWatcherTiming {
            publication_wait: Duration::from_millis(100),
            debounce: Duration::from_millis(1),
            local_debounce: Duration::from_millis(1),
            settle_retry: Duration::from_millis(1),
            settle_window: Duration::from_millis(1),
            idle_sleep: Duration::from_secs(3600),
        },
    ));
    if !mixed_startup {
        // Prime the actual watcher's dedup cache, then take the store AFTER
        // proposal resolution but BEFORE the slot can construct its surface.
        health_tx.send(()).await.unwrap();
        let primed = tokio::time::timeout(STALL_CEILING, proposals_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            primed.configuration_fingerprint(),
            initial.configuration_fingerprint()
        );
    }
    let guard =
        guard.unwrap_or_else(|| gents_pack::plugin::store::try_lock_store(home.path()).unwrap());
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let (start_tx, start_rx) = watch::channel(false);
    let runner = {
        let runtime = runtime.clone();
        let resolve = resolve.clone();
        let barrier = barrier.clone();
        move |pair: gents_tools::surface::AgentWithTools,
              surface: Arc<gents_tools::surface::ToolSurface>,
              requests: Arc<
            tokio::sync::Mutex<mpsc::Receiver<gents_protocol::row::AgentRequest>>,
        >,
              _capacity: Arc<crate::runtime::agent::worker_capacity::WorkerCapacity>,
              generation: u64,
              mut shutdown: watch::Receiver<bool>| {
            let runtime = runtime.clone();
            let resolve = resolve.clone();
            let barrier = barrier.clone();
            let started = started_tx.clone();
            let mut permission = start_rx.clone();
            async move {
                if !*permission.borrow() {
                    permission.changed().await.unwrap();
                }
                let did = pair.agent.agent_did.clone();
                if surface.has_plugin_tools() {
                    // Short injected duration, identical cancellable wait owner;
                    // the real production failure policy is deliberately used.
                    let _guard = gents_tools::surface::publication_guard_until(
                        runtime.plugin_home(),
                        tokio::time::Instant::now() + Duration::from_millis(5),
                    )
                    .await?;
                }
                let _prepared = super::super::context::build_slot_surface_fenced(
                    &runtime,
                    Some(&resolve),
                    pair,
                    surface,
                )
                .await?;
                barrier.mark_agent_ready(&did, generation).await;
                let _ = started.send((did, generation));
                let mut requests = requests.lock().await;
                loop {
                    tokio::select! {_=shutdown.changed()=>break, request=requests.recv()=>if request.is_none(){break;}}
                }
                anyhow::Ok(())
            }
        }
    };
    let mut recovery = status.publication_recovery();
    let supervisor = GenerationSupervisor::bootstrap(
        initial,
        gents_inference::admission::AdmissionRegistry::new(fixture.node.clone()),
        crate::runtime::agent::retry::RetryPolicy {
            max_retries: 1,
            base_delay_ms: 1,
            max_delay_ms: 2,
        },
        runner,
        status.clone(),
        shutdown_rx.clone(),
        Some(policy),
    )
    .await
    .unwrap();
    status
        .publish_startup_snapshot(supervisor.current_snapshot().as_ref())
        .await
        .unwrap();
    start_tx.send(true).unwrap();
    let unrelated = supervisor.current_snapshot().dispatchers[&free].clone();
    let (active_tx, mut active_rx) = watch::channel(supervisor.current_snapshot());
    let (forward_tx, forward_rx) = mpsc::channel(8);
    let supervising = tokio::spawn(supervisor.run(active_tx, forward_rx, shutdown_rx));
    let first = tokio::time::timeout(STALL_CEILING, started_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        first,
        (free.clone(), 1),
        "plugin-free startup must not wait for publication"
    );
    if !mixed_startup {
        tokio::time::timeout(STALL_CEILING, recovery.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status.readiness().observation().public_cause(&plugin),Some(gents_automation::triggers::runtime_snapshot::PublicUnavailableCause::PluginPublicationBusy));
        // The wake is emitted after BuildStanding::Demoted, not just a failed
        // build attempt or a synthetic proposal fed to the supervisor.
    }
    if hold_past_budget {
        let demotion = tokio::time::timeout(STALL_CEILING, proposals_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(demotion.unavailable_agents.contains_key(&plugin));
        forward_tx.send(demotion).await.unwrap();
        if mixed_startup {
            // It was already absent in the degraded startup snapshot. The
            // identical busy proposal is correctly a supervisor no-op, so
            // no watch generation change is required or expected here.
            assert!(!active_rx.borrow().dispatchers.contains_key(&plugin));
            assert!(active_rx.borrow().dispatchers[&free].same_channel(&unrelated));
        } else {
            tokio::time::timeout(STALL_CEILING, async {
                loop {
                    active_rx.changed().await.unwrap();
                    let active = active_rx.borrow_and_update();
                    if !active.dispatchers.contains_key(&plugin) {
                        assert!(active.dispatchers[&free].same_channel(&unrelated));
                        break;
                    }
                }
            })
            .await
            .unwrap();
        }
    }
    drop(guard); // No config/health event is sent at or after release.
    let recovered = tokio::time::timeout(STALL_CEILING, async {
        loop {
            let proposal = proposals_rx.recv().await.unwrap();
            if proposal.agents.contains_key(&plugin) {
                break proposal;
            }
        }
    })
    .await
    .expect("busy-park wake was fingerprint-deduplicated or startup retry was lost");
    forward_tx.send(recovered).await.unwrap();
    tokio::time::timeout(STALL_CEILING, async {
        loop {
            let (did, generation) = started_rx.recv().await.unwrap();
            if did == plugin {
                assert!(generation > 1);
                break;
            }
        }
    })
    .await
    .expect("real-policy parked agent never restarted");
    assert!(fatal_rx.try_recv().is_err());
    assert!(active_rx.borrow().dispatchers[&free].same_channel(&unrelated));
    shutdown_tx.send(true).unwrap();
    tokio::time::timeout(STALL_CEILING, watcher)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(forward_tx);
    tokio::time::timeout(STALL_CEILING, supervising)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
