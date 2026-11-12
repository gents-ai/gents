use std::path::{Path, PathBuf};
use std::time::Duration;

use gents_documents::{FileToolMode, FileTools, HostTools, IntegrationTools, LspTools, Tools};
use gents_tools::surface::ToolCeiling;

use crate::support::agent_requests::submit_user_request;
use crate::support::fixtures::configure_agent_tools_as;
use crate::support::live_inference::{
    bind_target, boot_live_agent_with_ceiling, live_target, terminal_assistant_answer,
    wait_for_request_terminal,
};
use crate::support::snapshots::fetch_tool_call_payloads_for_request_as;
use crate::support::test_db;

fn lsp_config() -> String {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let pack = std::env::var_os("GENTS_LSP_RUST_PACK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("../packs/packs/gents/lsp_rust"));
    let path = pack.join("pack_config.json");
    let bytes = std::fs::read(&path).unwrap_or_else(|error| {
        panic!(
            "read {}: {error}; set GENTS_LSP_RUST_PACK_DIR to packs/gents/lsp_rust",
            path.display()
        )
    });
    let pack: serde_json::Value = serde_json::from_slice(&bytes).expect("LSP pack configuration");
    let encoded = pack["tools"]
        .as_array()
        .expect("pack tools")
        .iter()
        .find(|tool| tool["tools_id"] == "lsp-readonly")
        .and_then(|tool| tool["integrations"]["lsp"]["config"].as_str())
        .expect("lsp-readonly server configuration");
    let mut config: serde_json::Value = serde_json::from_str(encoded).expect("LSP servers");
    let settings = &mut config["servers"]["rust-analyzer"]["settings"]["rust-analyzer"];
    settings["cargo"]["buildScripts"]["enable"] = false.into();
    settings["procMacro"]["enable"] = false.into();
    serde_json::to_string(&config).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lsp_live_model_uses_rust_analyzer() {
    let target = live_target().await;
    let config = tokio::task::spawn_blocking(|| {
        let version = std::process::Command::new("rust-analyzer")
            .arg("--version")
            .output()
            .expect("rust-analyzer must be installed and on PATH");
        assert!(
            version.status.success(),
            "rust-analyzer --version failed; install rust-analyzer"
        );
        lsp_config()
    })
    .await
    .unwrap();
    let workspace = tokio::task::spawn_blocking(tempfile::tempdir)
        .await
        .unwrap()
        .unwrap();
    let root = workspace.path().to_owned();
    let files_root = root.clone();
    tokio::task::spawn_blocking(move || {
        std::fs::create_dir(files_root.join("src")).unwrap();
        std::fs::write(files_root.join("Cargo.toml"),
            "[package]\nname = \"lsp-live-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[workspace]\n").unwrap();
        std::fs::write(files_root.join("src/lib.rs"),
            "pub struct Harbor { pub berths: u16 }\npub fn berth_count(harbor: &Harbor) -> u16 { harbor.berths }\n").unwrap();
    }).await.unwrap();
    let db = test_db("lsp-live").await;
    let actors = bind_target(&db, &target).await;
    configure_agent_tools_as(
        &actors.fixture.access, actors.node_did(), actors.agent_did(),
        Some("Use the requested lsp call. Once it succeeds, acknowledge briefly without repeating it.".into()),
        Tools {
            tools_id: "lsp-live-tools".into(), node_did: actors.node_did().into(),
            host: Some(HostTools { root: Some(root.display().to_string()),
                files: Some(FileTools { mode: FileToolMode::ReadOnly, ..Default::default() }),
                ..Default::default() }),
            integrations: Some(IntegrationTools { lsp: Some(LspTools { config: Some(config), ..Default::default() }), ..Default::default() }),
            ..Default::default()
        }, Vec::new(),
    ).await;
    let runtime = boot_live_agent_with_ceiling(
        &db,
        db.node_identity.clone(),
        ToolCeiling::readonly_at(&root),
    )
    .await
    .unwrap();
    const REQUEST: &str = "lsp-live-request";
    submit_user_request(actors.node_did(), &actors.fixture, "lsp-live-session", REQUEST,
        "Call lsp with action hover, file src/lib.rs, symbol berth_count, line 2. After the hover succeeds, acknowledge briefly.",
        Default::default()).await;
    let terminal =
        wait_for_request_terminal(&actors.fixture.access, REQUEST, Duration::from_secs(600)).await;
    let calls = fetch_tool_call_payloads_for_request_as(&actors.fixture.access, REQUEST).await;
    let answer = terminal_assistant_answer(&actors.fixture.access, REQUEST).await;
    runtime.shutdown().await;
    assert_eq!(terminal, "completed");
    assert!(
        calls.iter().any(|call| call.tool_name == "lsp"
            && call.lifecycle_state.as_deref() == Some("completed")
            && call
                .result
                .as_deref()
                .is_some_and(|result| result.contains("berth_count")
                    && result.contains("Harbor")
                    && result.contains("u16"))),
        "the real rust-analyzer hover must preserve the fixture's function signature: {calls:?}"
    );
    assert!(!answer.trim().is_empty());
}
