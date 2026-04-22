use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Path, PathBuf};

use crate::client::{ClientStore, ClientStoreRows};
use crate::telemetry::DesktopLogEntry;
use pprof::protos::Message;
use pprof::ProfilerGuard;
use serde::Serialize;

use super::*;

const SOAK_ENDPOINT: &str = "http://100.73.235.38:8000/v1";
const SOAK_MODEL: &str = "MiniMax-M2.7-NVFP4";

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SoakLogRecord {
    pub(crate) id: u64,
    pub(crate) level: String,
    pub(crate) target: String,
    pub(crate) message: String,
    pub(crate) timestamp: String,
    pub(crate) fields: BTreeMap<String, String>,
}

impl SoakLogRecord {
    pub(crate) fn summary_line(&self) -> String {
        format!("{} {}: {}", self.level, self.target, self.message)
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct SoakP2pDiagnostics {
    pub(crate) peer_ids: Vec<String>,
    pub(crate) collection_ids: Vec<String>,
    pub(crate) document_ids: Vec<String>,
    pub(crate) cids: Vec<String>,
    pub(crate) topics: Vec<String>,
}

pub(crate) fn soak_log_record(entry: DesktopLogEntry) -> SoakLogRecord {
    let fields = entry
        .fields
        .into_iter()
        .map(|field| (field.name, field.value))
        .collect();
    SoakLogRecord {
        id: entry.id,
        level: entry.level.to_string(),
        target: entry.target,
        message: entry.message,
        timestamp: entry.timestamp.to_rfc3339(),
        fields,
    }
}

pub(crate) fn summarize_p2p_logs(logs: &[SoakLogRecord]) -> SoakP2pDiagnostics {
    use std::collections::BTreeSet;

    let mut peer_ids = BTreeSet::new();
    let mut collection_ids = BTreeSet::new();
    let mut document_ids = BTreeSet::new();
    let mut cids = BTreeSet::new();
    let mut topics = BTreeSet::new();

    for log in logs {
        for (name, value) in &log.fields {
            if value.is_empty() {
                continue;
            }
            match name.as_str() {
                "peer_id" | "remote_peer_id" | "sender_peer_id" | "target_peer_id" => {
                    peer_ids.insert(value.clone());
                }
                "collection_id" => {
                    collection_ids.insert(value.clone());
                }
                "doc_id" | "document_id" => {
                    document_ids.insert(value.clone());
                }
                "cid" | "root_cid" => {
                    cids.insert(value.clone());
                }
                "topic" | "topic_hash" => {
                    topics.insert(value.clone());
                }
                _ => {}
            }
        }
    }

    SoakP2pDiagnostics {
        peer_ids: peer_ids.into_iter().collect(),
        collection_ids: collection_ids.into_iter().collect(),
        document_ids: document_ids.into_iter().collect(),
        cids: cids.into_iter().collect(),
        topics: topics.into_iter().collect(),
    }
}

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
            request_id: submission.effective_request_id.clone(),
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
        self.write_store_snapshots(runtime, desktop_core, deployments)?;
        self.write_prometheus(runtime, desktop_core, deployments)?;
        let _ = self.write_profile_artifacts("latest");
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
        self.write_store_snapshots(runtime, desktop_core, deployments)?;
        self.write_prometheus(runtime, desktop_core, deployments)?;
        let _ = self.write_profile_artifacts("latest");
        Ok(())
    }

    pub(crate) fn record_problem(
        &self,
        phase: &str,
        error: &anyhow::Error,
        recent_logs: &[SoakLogRecord],
    ) -> Result<()> {
        #[derive(Serialize)]
        struct FailureRecord<'a> {
            timestamp: String,
            phase: &'a str,
            error: String,
            recent_logs: &'a [SoakLogRecord],
            p2p_diagnostics: SoakP2pDiagnostics,
        }

        let record = FailureRecord {
            timestamp: chrono::Utc::now().to_rfc3339(),
            phase,
            error: format!("{error:#}"),
            recent_logs,
            p2p_diagnostics: summarize_p2p_logs(recent_logs),
        };
        self.write_json("failure.json", &record)?;
        let _ = self.write_profile_artifacts("failure");
        Ok(())
    }

    pub(crate) fn write_log_snapshot(&self, log_store: &DesktopLogStore) -> Result<()> {
        let logs: Vec<_> = log_store
            .snapshot()
            .entries
            .into_iter()
            .map(soak_log_record)
            .collect();
        self.write_json("desktop-logs.json", &logs)?;

        let by_scope_dir = self.output_dir.join("logs");
        std::fs::create_dir_all(&by_scope_dir)
            .with_context(|| format!("creating {}", by_scope_dir.display()))?;

        for partition in log_scope_partitions(&logs) {
            let scoped_logs: Vec<_> = logs
                .iter()
                .filter(|record| log_matches_partition(record, &partition))
                .cloned()
                .collect();
            let path =
                by_scope_dir.join(format!("{}.json", sanitize_filename_component(&partition)));
            let bytes = serde_json::to_vec_pretty(&scoped_logs)?;
            std::fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
        }

        Ok(())
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
            let body = match client
                .get(runtime_api.metrics_url())
                .send()
                .with_context(|| format!("fetching {}", runtime_api.metrics_url()))
                .and_then(|response| {
                    response
                        .error_for_status()
                        .with_context(|| format!("reading {}", runtime_api.metrics_url()))
                })
                .and_then(|response| {
                    response.text().with_context(|| {
                        format!("reading metrics body {}", runtime_api.metrics_url())
                    })
                }) {
                Ok(body) => body,
                Err(error) => {
                    tracing::warn!(
                        label = %runtime_api.label(),
                        metrics_url = %runtime_api.metrics_url(),
                        error = %error,
                        "skipping runtime metrics scrape failure"
                    );
                    continue;
                }
            };
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
        let Some(_) = self.profiler.as_ref() else {
            return Ok(());
        };
        self.write_profile_artifacts("final")?;
        Ok(())
    }

    fn write_store_snapshots(
        &self,
        runtime: &Runtime,
        desktop_core: &ClientCore,
        deployments: &[LiveRemoteDeployment],
    ) -> Result<()> {
        #[derive(Serialize)]
        struct StoreSnapshotRecord {
            label: String,
            peer_id: String,
            connected_peers: Vec<String>,
            row_count: usize,
            approx_serialized_bytes: usize,
            rows: ClientStoreRows,
        }

        fn rows_from_store(store: &ClientStore) -> ClientStoreRows {
            ClientStoreRows {
                agent_principals: store.agent_principals.clone(),
                behaviors: store.behaviors.clone(),
                runtimes: store.runtimes.clone(),
                conversations: store.conversations.clone(),
                requests: store.requests.clone(),
                responses: store.responses.clone(),
                messages: store.messages.clone(),
                sessions: store.sessions.clone(),
                tool_calls: store.tool_calls.clone(),
                tool_results: store.tool_results.clone(),
                compaction_entries: store.compaction_entries.clone(),
                tasks: store.tasks.clone(),
                schedules: store.schedules.clone(),
                event_triggers: store.event_triggers.clone(),
                tool_selections: store.tool_selections.clone(),
                inference_backends: store.inference_backends.clone(),
                inference_profiles: store.inference_profiles.clone(),
                tool_service_registries: store.tool_service_registries.clone(),
            }
        }

        let store_dir = self.output_dir.join("store-snapshots");
        std::fs::create_dir_all(&store_dir)
            .with_context(|| format!("creating {}", store_dir.display()))?;

        runtime.block_on(desktop_core.refresh_store())?;
        let desktop_snapshot = desktop_core.store().snapshot();
        let desktop_record = StoreSnapshotRecord {
            label: "Desktop".to_string(),
            peer_id: desktop_core.local_peer_id().to_string(),
            connected_peers: runtime
                .block_on(desktop_core.p2p().connected_peers())
                .unwrap_or_default(),
            row_count: desktop_snapshot.row_count(),
            approx_serialized_bytes: desktop_snapshot.approx_serialized_bytes(),
            rows: rows_from_store(&desktop_snapshot),
        };
        self.write_json(store_dir.join("Desktop.json"), &desktop_record)?;

        for remote in deployments {
            runtime.block_on(remote.core.refresh_store())?;
            let snapshot = remote.core.store().snapshot();
            let record = StoreSnapshotRecord {
                label: remote.label.clone(),
                peer_id: remote.peer_id.clone(),
                connected_peers: runtime
                    .block_on(remote.core.p2p().connected_peers())
                    .unwrap_or_default(),
                row_count: snapshot.row_count(),
                approx_serialized_bytes: snapshot.approx_serialized_bytes(),
                rows: rows_from_store(&snapshot),
            };
            self.write_json(
                store_dir.join(format!(
                    "{}.json",
                    sanitize_filename_component(&remote.label)
                )),
                &record,
            )?;
        }

        Ok(())
    }

    fn write_profile_artifacts(&self, stem: &str) -> Result<()> {
        let Some(profiler) = self.profiler.as_ref() else {
            return Ok(());
        };
        let report = profiler.report().build()?;

        let flamegraph_path = if stem == "final" {
            self.output_dir.join("soak-flamegraph.svg")
        } else {
            self.output_dir.join(format!("soak-{stem}-flamegraph.svg"))
        };
        let flamegraph_file = File::create(&flamegraph_path)
            .with_context(|| format!("creating {}", flamegraph_path.display()))?;
        report.flamegraph(flamegraph_file)?;

        let profile = report.pprof()?;
        let profile_path = if stem == "final" {
            self.output_dir.join("soak-profile.pb")
        } else {
            self.output_dir.join(format!("soak-{stem}-profile.pb"))
        };
        std::fs::write(&profile_path, profile.encode_to_vec())
            .with_context(|| format!("writing {}", profile_path.display()))?;

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

fn log_scope_partitions(logs: &[SoakLogRecord]) -> Vec<String> {
    let mut partitions = BTreeSet::new();
    for log in logs {
        for partition in partitions_for_log(log) {
            partitions.insert(partition);
        }
    }
    partitions.into_iter().collect()
}

fn log_matches_partition(log: &SoakLogRecord, partition: &str) -> bool {
    partitions_for_log(log)
        .iter()
        .any(|candidate| candidate == partition)
}

fn partitions_for_log(log: &SoakLogRecord) -> Vec<String> {
    let mut partitions = Vec::new();

    if let Some(deployment) = log_field(log, "span.deployment_label") {
        partitions.push(format!("deployment-{deployment}"));
    }

    if let Some(agent_did) =
        log_field(log, "span.agent_did").or_else(|| log_field(log, "agent_did"))
    {
        partitions.push(format!("agent-{agent_did}"));
    }

    if partitions.is_empty() {
        partitions.push("unscoped".to_string());
    }

    partitions
}

fn log_field<'a>(log: &'a SoakLogRecord, name: &str) -> Option<&'a str> {
    log.fields
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
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
