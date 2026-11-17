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
    let endpoint = MockModelEndpoint::start("default").unwrap();
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

    let (waiting_tx, mut waiting_rx) = mpsc::unbounded_channel();
    let subscriber = Registry::default().with(PublicationWaitCapture(waiting_tx));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let run = tokio::spawn(agent.run(shutdown_rx).with_subscriber(subscriber));
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
    assert_eq!(
        pending["data"]["AgentRequest"][0]["lifecycle_state"], "pending",
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
        terminal["AgentRequest"][0]["lifecycle_state"], "completed",
        "{terminal}"
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
