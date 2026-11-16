use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;

use anyhow::{anyhow, ensure, Context, Result};
use gents::support::live_inference::{live_target, wait_for_request_terminal, wait_for_rows};
use gents_protocol::row::{
    decode_node_readiness_snapshot, NodeReadinessProcessState, NodeReadinessRow,
};
use gents_store::config_client::ConfigAccess;
use serde_json::Value;
use tokio::io::{AsyncReadExt, BufReader};
use tokio::process::{Child, Command};

use crate::support::{
    allocate_port, cli_bin, graphql_url, inference_profile_id_from_init, node_did_from_init,
    run_init_json,
};

pub(crate) struct LiveCli {
    pub home: PathBuf,
    pub graphql: String,
    pub node: String,
    pub agent: String,
    pub access: ConfigAccess,
    child: Child,
    drain: tokio::task::JoinHandle<()>,
    directory: tempfile::TempDir,
}

impl LiveCli {
    pub(crate) async fn start(label: &str, unrestricted: bool) -> Result<Self> {
        let target = live_target().await;
        let endpoint = target.endpoint().to_owned();
        let model = target.model().to_owned();
        let label = label.to_owned();
        let (directory, home, init, port) = tokio::task::spawn_blocking(move || {
            let directory = tempfile::tempdir()?;
            let home = directory.path().join("home");
            std::fs::create_dir_all(&home)?;
            let mut args = vec![
                "--node-name",
                &label,
                "--model-name",
                &model,
                "--inference-url",
                &endpoint,
                "--max-concurrent",
                "4",
            ];
            if unrestricted {
                args.push("--yolo");
            }
            let init = run_init_json(&home, &args)?;
            let port = allocate_port()?;
            Ok::<_, anyhow::Error>((directory, home, init, port))
        })
        .await??;
        let node = node_did_from_init(&init)?;
        let agent = crate::support::agent_did_from_init(&init)?;
        let backend_id = init
            .pointer("/init/backend_id")
            .and_then(Value::as_str)
            .context("backend id")?;
        let mut backend = target.backend(&node);
        backend.backend_id = backend_id.into();
        backend.max_concurrent = Some(4);
        let mut profile = target.profile(&node);
        profile.profile_id = inference_profile_id_from_init(&init)?;
        profile.backend_id = backend_id.into();
        let sampling = target.sampling(&node);
        let target_home = home.clone();
        let root = directory.path().join("live-target");
        tokio::task::spawn_blocking(move || {
            let state = target_home.join(".gents");
            let state = state.to_str().context("runtime home is not UTF-8")?;
            let root_arg = root.to_str().context("target configuration is not UTF-8")?;
            crate::support::run_cli_text(
                &target_home,
                &["config", "export", "--home", state, "--root", root_arg],
            )?;
            let path = root.join("pack_config.json");
            let mut config = crate::support::read_json_file(&path)?;
            config["inference_backends"] = serde_json::to_value([backend])?;
            config["inference_profiles"] = serde_json::to_value([profile])?;
            if let Some(sampling) = sampling {
                config["inference_sampling"] = serde_json::to_value([sampling])?;
            }
            crate::support::write_json_file(&path, &config)?;
            crate::support::run_cli_json(
                &target_home,
                &["config", "apply", "--home", state, "--root", root_arg],
            )?;
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("configure the live target before starting its runtime")??;
        let mut child = Command::new(cli_bin())
            .env("HOME", &home)
            .env("RUST_LOG", "error")
            .env_remove("GENTS_SYSTEM_LOG")
            .current_dir(&home)
            .args(["server", "--http-port", &port.to_string()])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        let mut stdout = BufReader::new(child.stdout.take().context("server stdout")?);
        tokio::time::timeout(Duration::from_secs(30), async {
            let mut buffer = String::new();
            loop {
                let mut bytes = [0; 4096];
                let read = stdout.read(&mut bytes).await?;
                ensure!(read != 0, "server exited before readiness: {buffer}");
                buffer.push_str(&String::from_utf8_lossy(&bytes[..read]));
                if crate::support::process::server_readiness_json(&buffer).is_some() {
                    return Ok::<_, anyhow::Error>(());
                }
            }
        })
        .await
        .context("server readiness deadline")??;
        let drain = tokio::spawn(async move {
            let _ = tokio::io::copy(&mut stdout, &mut tokio::io::sink()).await;
        });
        let graphql = graphql_url(port);
        let user_home = home.clone();
        let served_home = home.join(".gents");
        tokio::task::spawn_blocking(move || {
            crate::support::graphql::register_served_home(&user_home, &served_home, &[], port)
        })
        .await??;
        let access = ConfigAccess::Graphql(crate::support::graphql::served_endpoint(&graphql));
        let query = format!("{{ NodeReadiness(filter: {{node_did: {{_eq: \"{}\"}}}}) {{node_did snapshot_json updated_at}} }}", crate::support::escape_graphql_string(&node));
        wait_for_rows(
            &access,
            "NodeReadiness",
            &query,
            Duration::from_secs(30),
            |data| {
                data["NodeReadiness"].as_array().is_some_and(|rows| {
                    rows.iter().any(|value| {
                        serde_json::from_value::<NodeReadinessRow>(value.clone())
                            .ok()
                            .and_then(|row| decode_node_readiness_snapshot(&row, &node).ok())
                            .is_some_and(|snapshot| {
                                snapshot.process_state == NodeReadinessProcessState::Ready
                                    && snapshot.active_generation > 0
                                    && snapshot.router_generation == snapshot.active_generation
                            })
                    })
                })
            },
        )
        .await;
        let live = Self {
            home,
            graphql,
            node,
            agent,
            access,
            child,
            drain,
            directory,
        };
        Ok(live)
    }

    pub(crate) async fn output(&self, args: &[&str]) -> Result<Output> {
        tokio::time::timeout(
            Duration::from_secs(120),
            Command::new(cli_bin())
                .env("HOME", &self.home)
                .env("RUST_LOG", "error")
                .current_dir(&self.home)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .context("CLI process deadline")?
        .with_context(|| format!("running gents {args:?}"))
    }

    pub(crate) async fn json(&self, args: &[&str]) -> Result<Value> {
        let output = self.output(args).await?;
        ensure!(
            output.status.success(),
            "gents {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).with_context(|| {
            format!(
                "gents {args:?}: {}",
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }

    pub(crate) async fn text(&self, args: &[&str]) -> Result<String> {
        let output = self.output(args).await?;
        ensure!(
            output.status.success(),
            "gents {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8(output.stdout)?)
    }

    pub(crate) async fn failure(&self, args: &[&str]) -> Result<String> {
        let output = self.output(args).await?;
        ensure!(
            !output.status.success(),
            "gents {args:?} unexpectedly succeeded"
        );
        Ok(String::from_utf8(output.stderr)?)
    }

    pub(crate) async fn request(&self, session: &str, content: &str) -> Result<Value> {
        self.json(&[
            "utils",
            "debug",
            "request",
            "submit",
            "--graphql",
            &self.graphql,
            "--node-did",
            &self.node,
            "--session-id",
            session,
            "--content",
            content,
            "--timeout-secs",
            "90",
            "--poll-secs",
            "1",
        ])
        .await
    }

    pub(crate) async fn terminal(&self, request: &str) -> Result<()> {
        let state =
            wait_for_request_terminal(&self.access, request, Duration::from_secs(120)).await;
        ensure!(state == "completed", "request {request} ended as {state}");
        Ok(())
    }

    pub(crate) async fn answer(&self, request: &str) -> Result<String> {
        let answer =
            gents::support::live_inference::terminal_assistant_answer(&self.access, request).await;
        ensure!(
            !answer.trim().is_empty(),
            "request {request} has no visible answer"
        );
        Ok(answer)
    }

    pub(crate) async fn first_row(&self, collection: &str, query: &str) -> Result<Value> {
        let value = self.access.execute(query).await?;
        value["data"][collection]
            .as_array()
            .and_then(|rows| rows.first())
            .cloned()
            .ok_or_else(|| anyhow!("no {collection} row: {value}"))
    }

    pub(crate) async fn finish(self, result: Result<()>) -> Result<()> {
        let Self {
            mut child,
            drain,
            directory,
            ..
        } = self;
        child.start_kill()?;
        let _ = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
        let _ = drain.await;
        tokio::task::spawn_blocking(move || drop(directory)).await?;
        result
    }
}

pub(crate) fn path_text(path: &Path) -> Result<&str> {
    path.to_str().context("fixture path must be UTF-8")
}
