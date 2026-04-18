use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use pprof::ProfilerGuard;
use serde::Serialize;

use super::*;

const SOAK_ENDPOINT: &str = "http://100.73.235.38:8000/v1";
const SOAK_MODEL: &str = "MiniMax-M2.7-NVFP4";

#[derive(Debug, Clone)]
pub(crate) struct LiveSoakConfig {
    pub(crate) output_dir: PathBuf,
    pub(crate) keep_workspace: bool,
}

impl LiveSoakConfig {
    pub(crate) fn from_env(name: &str) -> Result<Self> {
        let output_dir = LiveSoakDiagnostics::persistent_output_dir(name)?;
        let keep_workspace = std::env::var("DEFRA_AGENT_DESKTOP_SOAK_KEEP_WORKDIR")
            .ok()
            .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes"));
        Ok(Self {
            output_dir,
            keep_workspace,
        })
    }
}

pub(crate) fn explicit_soak_backend() -> AgentBackendConfig {
    AgentBackendConfig::openai_compatible(SOAK_ENDPOINT, SOAK_MODEL)
}

pub(crate) struct LiveSoakDiagnostics {
    output_dir: PathBuf,
    profiler: Option<ProfilerGuard<'static>>,
    completed_turns: usize,
    last_turn_by_deployment: BTreeMap<String, usize>,
}

impl LiveSoakDiagnostics {
    pub(crate) fn new(output_dir: impl Into<PathBuf>) -> Result<Self> {
        let output_dir = output_dir.into();
        std::fs::create_dir_all(&output_dir)
            .with_context(|| format!("creating soak diagnostics dir {}", output_dir.display()))?;
        let profiler = ProfilerGuard::new(99).ok();
        Ok(Self {
            output_dir,
            profiler,
            completed_turns: 0,
            last_turn_by_deployment: BTreeMap::new(),
        })
    }

    pub(crate) fn persistent_output_dir(name: &str) -> Result<PathBuf> {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let root = manifest_dir
            .ancestors()
            .nth(2)
            .ok_or_else(|| anyhow!("failed to locate repo root from {}", manifest_dir.display()))?;
        let artifact_root = std::env::var("DEFRA_AGENT_DESKTOP_SOAK_ARTIFACT_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| root.join("target").join("desktop-live-soak"));
        let output_dir = artifact_root.join(format!(
            "{}-{}-{}",
            sanitize_filename_component(name),
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ"),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&output_dir)
            .with_context(|| format!("creating soak diagnostics dir {}", output_dir.display()))?;
        Ok(output_dir)
    }

    pub(crate) fn output_dir(&self) -> &Path {
        &self.output_dir
    }

    pub(crate) fn write_metadata(
        &self,
        deployments: &[LiveRemoteDeployment],
        backend: &AgentBackendConfig,
    ) -> Result<()> {
        #[derive(Serialize)]
        struct Metadata<'a> {
            endpoint: &'a str,
            model_name: &'a str,
            deployment_labels: Vec<&'a str>,
            agent_dids: Vec<&'a str>,
            tool_roots: Vec<String>,
        }

        let metadata = Metadata {
            endpoint: &backend.endpoint,
            model_name: &backend.model_name,
            deployment_labels: deployments
                .iter()
                .map(|deployment| deployment.label.as_str())
                .collect(),
            agent_dids: deployments
                .iter()
                .map(|deployment| deployment.agent_did.as_str())
                .collect(),
            tool_roots: deployments
                .iter()
                .map(|deployment| deployment.running_agent.tool_root.display().to_string())
                .collect(),
        };
        self.write_json("metadata.json", &metadata)
    }

    pub(crate) fn record_turn(
        &mut self,
        runtime: &Runtime,
        driver: &AuditDriver,
        deployments: &[LiveRemoteDeployment],
        deployment: &LiveDeploymentCase<'_>,
        turn: usize,
        submission: &LiveSubmissionCase,
    ) -> Result<()> {
        #[derive(Serialize)]
        struct PeerTopology {
            local_peer_id: String,
            connected_peers: Vec<String>,
        }

        #[derive(Serialize)]
        struct DeploymentState {
            label: String,
            peer_id: String,
            agent_did: String,
            runtime_generation: Option<i64>,
            connected_peers: Vec<String>,
            conversation_count: usize,
            request_count: usize,
            response_count: usize,
        }

        #[derive(Serialize)]
        struct TurnRecord {
            timestamp: String,
            deployment: String,
            peer_id: String,
            agent_did: String,
            turn: usize,
            request_id: String,
            session_id: String,
            prompt: String,
            response: String,
            desktop_selected_peer_id: Option<String>,
            desktop_selected_agent_did: Option<String>,
            desktop_selected_session_id: Option<String>,
            desktop_topology: PeerTopology,
            desktop_requests_total: usize,
            desktop_responses_total: usize,
            desktop_conversations_total: usize,
            deployment_states: Vec<DeploymentState>,
        }

        let desktop_core =
            driver.app.client.as_ref().ok_or_else(|| {
                anyhow!("desktop client missing while recording soak diagnostics")
            })?;
        runtime.block_on(desktop_core.refresh_store())?;
        let desktop_snapshot = desktop_core.store().snapshot();
        let desktop_topology = PeerTopology {
            local_peer_id: desktop_core.local_peer_id().to_string(),
            connected_peers: runtime
                .block_on(desktop_core.p2p().connected_peers())
                .unwrap_or_default(),
        };

        let mut deployment_states = Vec::new();
        for remote in deployments {
            runtime.block_on(remote.core.refresh_store())?;
            let snapshot = remote.core.store().snapshot();
            deployment_states.push(DeploymentState {
                label: remote.label.clone(),
                peer_id: remote.peer_id.clone(),
                agent_did: remote.agent_did.clone(),
                runtime_generation: snapshot
                    .latest_runtime(&remote.agent_did)
                    .and_then(|row| row.router_generation.or(row.active_generation)),
                connected_peers: runtime
                    .block_on(remote.core.p2p().connected_peers())
                    .unwrap_or_default(),
                conversation_count: snapshot.conversation_rows(&remote.agent_did).len(),
                request_count: snapshot
                    .requests
                    .iter()
                    .filter(|row| row.agent_did.as_deref() == Some(remote.agent_did.as_str()))
                    .count(),
                response_count: snapshot
                    .responses
                    .iter()
                    .filter(|row| row.agent_did.as_deref() == Some(remote.agent_did.as_str()))
                    .count(),
            });
        }

        let record = TurnRecord {
            timestamp: chrono::Utc::now().to_rfc3339(),
            deployment: deployment.label.clone(),
            peer_id: deployment.peer_id.clone(),
            agent_did: deployment.agent_did.clone(),
            turn,
            request_id: submission.request_id.clone(),
            session_id: submission.session_id.clone(),
            prompt: submission.prompt.clone(),
            response: submission.response.clone(),
            desktop_selected_peer_id: driver.app.state.chat.shell.selected_peer_id.clone(),
            desktop_selected_agent_did: driver.app.state.chat.shell.selected_agent_did.clone(),
            desktop_selected_session_id: driver.app.state.chat.shell.selected_session_id.clone(),
            desktop_topology,
            desktop_requests_total: desktop_snapshot.requests.len(),
            desktop_responses_total: desktop_snapshot.responses.len(),
            desktop_conversations_total: desktop_snapshot.conversations.len(),
            deployment_states,
        };

        self.completed_turns += 1;
        self.last_turn_by_deployment
            .insert(deployment.label.clone(), turn);

        let filename = format!(
            "turn-{turn:02}-{}.json",
            sanitize_filename_component(&deployment.label)
        );
        self.write_json(filename, &record)?;
        self.write_prometheus(runtime, desktop_core, deployments)?;
        Ok(())
    }

    pub(crate) fn record_snapshot(
        &self,
        runtime: &Runtime,
        driver: &AuditDriver,
        deployments: &[LiveRemoteDeployment],
    ) -> Result<()> {
        #[derive(Serialize)]
        struct DeploymentSnapshot {
            label: String,
            peer_id: String,
            agent_did: String,
            connected_peers: Vec<String>,
            request_count: usize,
            response_count: usize,
            conversation_count: usize,
        }

        #[derive(Serialize)]
        struct SnapshotRecord {
            timestamp: String,
            desktop_connected_peers: Vec<String>,
            desktop_requests_total: usize,
            desktop_responses_total: usize,
            desktop_conversations_total: usize,
            deployments: Vec<DeploymentSnapshot>,
        }

        let desktop_core = driver
            .app
            .client
            .as_ref()
            .ok_or_else(|| anyhow!("desktop client missing while recording soak snapshot"))?;
        runtime.block_on(desktop_core.refresh_store())?;
        let desktop_snapshot = desktop_core.store().snapshot();
        let mut deployment_snapshots = Vec::new();
        for remote in deployments {
            runtime.block_on(remote.core.refresh_store())?;
            let snapshot = remote.core.store().snapshot();
            deployment_snapshots.push(DeploymentSnapshot {
                label: remote.label.clone(),
                peer_id: remote.peer_id.clone(),
                agent_did: remote.agent_did.clone(),
                connected_peers: runtime
                    .block_on(remote.core.p2p().connected_peers())
                    .unwrap_or_default(),
                request_count: snapshot.requests.len(),
                response_count: snapshot.responses.len(),
                conversation_count: snapshot.conversations.len(),
            });
        }

        let record = SnapshotRecord {
            timestamp: chrono::Utc::now().to_rfc3339(),
            desktop_connected_peers: runtime
                .block_on(desktop_core.p2p().connected_peers())
                .unwrap_or_default(),
            desktop_requests_total: desktop_snapshot.requests.len(),
            desktop_responses_total: desktop_snapshot.responses.len(),
            desktop_conversations_total: desktop_snapshot.conversations.len(),
            deployments: deployment_snapshots,
        };
        self.write_json("snapshot.json", &record)?;
        self.write_prometheus(runtime, desktop_core, deployments)
    }

    pub(crate) fn record_problem(
        &self,
        phase: &str,
        error: &anyhow::Error,
        recent_logs: &[String],
    ) -> Result<()> {
        #[derive(Serialize)]
        struct FailureRecord<'a> {
            timestamp: String,
            phase: &'a str,
            error: String,
            recent_logs: &'a [String],
        }

        let record = FailureRecord {
            timestamp: chrono::Utc::now().to_rfc3339(),
            phase,
            error: format!("{error:#}"),
            recent_logs,
        };
        self.write_json("failure.json", &record)
    }

    pub(crate) fn write_log_snapshot(&self, log_store: &DesktopLogStore) -> Result<()> {
        #[derive(Serialize)]
        struct LogRecord {
            id: u64,
            level: String,
            target: String,
            message: String,
            timestamp: String,
        }

        let logs: Vec<_> = log_store
            .snapshot()
            .entries
            .into_iter()
            .map(|entry| LogRecord {
                id: entry.id,
                level: entry.level.to_string(),
                target: entry.target,
                message: entry.message,
                timestamp: entry.timestamp.to_rfc3339(),
            })
            .collect();
        self.write_json("desktop-logs.json", &logs)
    }

    pub(crate) fn scrape_runtime_metrics(
        &self,
        runtime_apis: &[BootstrapRuntimeApi],
    ) -> Result<()> {
        let metrics_dir = self.output_dir.join("runtime-metrics");
        std::fs::create_dir_all(&metrics_dir)
            .with_context(|| format!("creating {}", metrics_dir.display()))?;
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()?;
        for runtime_api in runtime_apis {
            let body = client
                .get(runtime_api.metrics_url())
                .send()
                .with_context(|| format!("fetching {}", runtime_api.metrics_url()))?
                .error_for_status()
                .with_context(|| format!("reading {}", runtime_api.metrics_url()))?
                .text()
                .with_context(|| format!("reading metrics body {}", runtime_api.metrics_url()))?;
            let path = metrics_dir.join(format!(
                "{}.prom",
                sanitize_filename_component(runtime_api.label())
            ));
            std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
        }
        Ok(())
    }

    pub(crate) fn capture_workspace(&self, workspace_root: &Path) -> Result<()> {
        let destination = self.output_dir.join("workspace");
        copy_dir_all(workspace_root, &destination)
            .with_context(|| format!("copying workspace into {}", destination.display()))
    }

    fn write_prometheus(
        &self,
        runtime: &Runtime,
        desktop_core: &ClientCore,
        deployments: &[LiveRemoteDeployment],
    ) -> Result<()> {
        runtime.block_on(desktop_core.refresh_store())?;
        let desktop_snapshot = desktop_core.store().snapshot();
        let mut body = String::new();
        body.push_str("# HELP desktop_live_soak_completed_turns_total completed soak turns.\n");
        body.push_str("# TYPE desktop_live_soak_completed_turns_total counter\n");
        body.push_str(&format!(
            "desktop_live_soak_completed_turns_total {}\n",
            self.completed_turns
        ));
        body.push_str(
            "# HELP desktop_live_soak_desktop_requests_total observed desktop requests.\n",
        );
        body.push_str("# TYPE desktop_live_soak_desktop_requests_total gauge\n");
        body.push_str(&format!(
            "desktop_live_soak_desktop_requests_total {}\n",
            desktop_snapshot.requests.len()
        ));
        body.push_str(
            "# HELP desktop_live_soak_desktop_responses_total observed desktop responses.\n",
        );
        body.push_str("# TYPE desktop_live_soak_desktop_responses_total gauge\n");
        body.push_str(&format!(
            "desktop_live_soak_desktop_responses_total {}\n",
            desktop_snapshot.responses.len()
        ));
        body.push_str("# HELP desktop_live_soak_connected_peers currently connected peers.\n");
        body.push_str("# TYPE desktop_live_soak_connected_peers gauge\n");
        let desktop_connected = runtime
            .block_on(desktop_core.p2p().connected_peers())
            .unwrap_or_default();
        body.push_str(&format!(
            "desktop_live_soak_connected_peers{{scope=\"desktop\"}} {}\n",
            desktop_connected.len()
        ));

        for remote in deployments {
            runtime.block_on(remote.core.refresh_store())?;
            let snapshot = remote.core.store().snapshot();
            let connected = runtime
                .block_on(remote.core.p2p().connected_peers())
                .unwrap_or_default();
            let generation = snapshot
                .latest_runtime(&remote.agent_did)
                .and_then(|row| row.router_generation.or(row.active_generation))
                .unwrap_or_default();
            let last_turn = self
                .last_turn_by_deployment
                .get(&remote.label)
                .copied()
                .unwrap_or_default();
            body.push_str(&format!(
                "desktop_live_soak_connected_peers{{scope=\"remote\",deployment=\"{}\"}} {}\n",
                prometheus_escape(&remote.label),
                connected.len()
            ));
            body.push_str(&format!(
                "desktop_live_soak_runtime_generation{{deployment=\"{}\",agent_did=\"{}\"}} {}\n",
                prometheus_escape(&remote.label),
                prometheus_escape(&remote.agent_did),
                generation
            ));
            body.push_str(&format!(
                "desktop_live_soak_last_completed_turn{{deployment=\"{}\",agent_did=\"{}\"}} {}\n",
                prometheus_escape(&remote.label),
                prometheus_escape(&remote.agent_did),
                last_turn
            ));
        }

        std::fs::write(self.output_dir.join("soak.prom"), body)
            .with_context(|| format!("writing {}", self.output_dir.join("soak.prom").display()))
    }

    fn write_json(&self, name: impl AsRef<Path>, value: &impl Serialize) -> Result<()> {
        let path = self.output_dir.join(name.as_ref());
        let bytes = serde_json::to_vec_pretty(value)?;
        std::fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))
    }

    fn flush_profile(&mut self) -> Result<()> {
        let Some(profiler) = self.profiler.take() else {
            return Ok(());
        };
        let report = profiler.report().build()?;
        let flamegraph_path = self.output_dir.join("soak-flamegraph.svg");
        let file = File::create(&flamegraph_path)
            .with_context(|| format!("creating {}", flamegraph_path.display()))?;
        report.flamegraph(file)?;
        Ok(())
    }
}

impl Drop for LiveSoakDiagnostics {
    fn drop(&mut self) {
        let _ = self.flush_profile();
    }
}

fn sanitize_filename_component(input: &str) -> String {
    input
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' => ch,
            _ => '-',
        })
        .collect()
}

fn prometheus_escape(input: &str) -> String {
    input.replace('\\', r#"\\"#).replace('"', "\\\"")
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst).with_context(|| format!("creating {}", dst.display()))?;
    for entry in std::fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let destination = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_all(&entry.path(), &destination)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), &destination).with_context(|| {
                format!(
                    "copying {} -> {}",
                    entry.path().display(),
                    destination.display()
                )
            })?;
        }
    }
    Ok(())
}
