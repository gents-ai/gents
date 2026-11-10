use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context as _, Result};
use rapidhash::RapidHashMap;
use serde_json::Value;

use super::native_runner::self_runner_binary;
use super::shared::{
    prepare_managed_command, CommandConstraints, CommandExecutionMode, CommandNetworkMode,
};
use crate::tools::call::lifecycle::runtime::tool_execution_bounds;
use crate::tools::exec::{run_managed_exec, ManagedExecOutcome, ManagedExecRequest};

/// Path to a binary that takes [`PLUGIN_BUILD_COMMAND`]; unset means this one.
const BUILDER_ENV: &str = "GENTS_PLUGIN_BUILDER";
/// The hidden entry point of the `gents` and desktop binaries.
pub const PLUGIN_BUILD_COMMAND: &str = "__plugin-build";
const BUILD_TIMEOUT: Duration = Duration::from_secs(900);
const MAX_BUILD_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
const STDERR_TAIL_BYTES: usize = 16 * 1024;

/// What a sandboxed plugin build reported.
#[derive(Debug)]
pub struct PluginBuildOutcome {
    /// The builder's one JSON object: `{"cases": [...] | null}`.
    pub output: Value,
    pub sandbox: &'static str,
}

/// Builds the plugins of the pack at `pack_dir`, and runs their cases when
/// asked, in one process under the command sandbox bash uses: writes confined
/// to `root`, no network. The whole build runs inside it, source mirroring
/// included, because a source tree can hold links this process must not follow.
pub async fn run_plugin_build(
    root: &Path,
    pack_dir: &Path,
    run_cases: bool,
) -> Result<PluginBuildOutcome> {
    let program = builder_program()?;
    let mut args = vec![
        PLUGIN_BUILD_COMMAND.to_owned(),
        pack_dir
            .to_str()
            .context("the pack path is not UTF-8")?
            .to_owned(),
    ];
    if run_cases {
        args.push("--test".to_owned());
    }
    let mut command = prepare_managed_command(
        root,
        &program,
        &args,
        &CommandConstraints {
            allowed_argv_prefixes: Vec::new(),
            forbidden_argv_prefixes: Vec::new(),
            network_mode: CommandNetworkMode::Inherit,
            execution_mode: CommandExecutionMode::WorkspaceWrite,
            sandbox: CommandExecutionMode::WorkspaceWrite,
            deny_all_argv: false,
            deny_git_metadata_writes: true,
        },
    )
    .await
    .map_err(|error| anyhow!("{error}"))?;
    keep_toolchain_state_in_root(&mut command.environment);
    let bounds = tool_execution_bounds(BUILD_TIMEOUT);
    let argv = std::iter::once(command.program.to_string_lossy().into_owned())
        .chain(command.args)
        .collect();
    let outcome = run_managed_exec(ManagedExecRequest {
        argv,
        cwd: pack_dir.to_path_buf(),
        deadline_at: bounds.deadline_at,
        cancellation_token: bounds.cancellation_token,
        max_output_bytes: MAX_BUILD_OUTPUT_BYTES,
        stdin: Vec::new(),
        environment: Some(command.environment),
        tool_name: Some("config".to_owned()),
        live_output: None,
    })
    .await;
    match outcome {
        ManagedExecOutcome::Exited {
            code: Some(0),
            stdout,
            ..
        } => Ok(PluginBuildOutcome {
            output: serde_json::from_slice(&stdout)
                .context("the plugin builder printed no JSON report")?,
            sandbox: command.sandbox,
        }),
        ManagedExecOutcome::Exited { code, stderr, .. } => Err(anyhow!(
            "the plugin build failed (exit {}):\n{}",
            code.map_or_else(|| "by signal".to_owned(), |code| code.to_string()),
            stderr_tail(&stderr)
        )),
        ManagedExecOutcome::TimedOut { stderr, .. } => Err(anyhow!(
            "the plugin build was stopped at its deadline:\n{}",
            stderr_tail(&stderr)
        )),
        ManagedExecOutcome::Cancelled { .. } => Err(anyhow!("the plugin build was cancelled")),
        ManagedExecOutcome::SpawnFailed { error } => {
            Err(anyhow!("the plugin builder failed to spawn: {error}"))
        }
    }
}

fn builder_program() -> Result<String> {
    if let Some(path) = std::env::var(BUILDER_ENV)
        .ok()
        .filter(|path| !path.trim().is_empty())
    {
        return Ok(path);
    }
    self_runner_binary()
        .map(|path: PathBuf| path.to_string_lossy().into_owned())
        .context(
            "this runtime has no plugin builder; have the operator run `gents config pack build`",
        )
}

/// The sandbox refuses writes outside its root, so caches a toolchain keeps in
/// the home directory move into the root's temporary directory, and a
/// configured compiler wrapper daemon such as sccache is switched off. Cargo
/// runs offline because the sandbox has no network.
fn keep_toolchain_state_in_root(environment: &mut RapidHashMap<String, String>) {
    for name in ["RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"] {
        environment.insert(name.to_owned(), String::new());
    }
    environment.insert("CARGO_NET_OFFLINE".to_owned(), "true".to_owned());
    if let Some(scratch) = environment.get("TMPDIR").map(PathBuf::from) {
        for (name, directory) in [
            ("GOCACHE", "go-cache"),
            ("GOMODCACHE", "go-mod"),
            ("GOPATH", "go-path"),
        ] {
            environment.insert(
                name.to_owned(),
                scratch.join(directory).to_string_lossy().into_owned(),
            );
        }
    }
}

fn stderr_tail(stderr: &[u8]) -> String {
    let start = stderr.len().saturating_sub(STDERR_TAIL_BYTES);
    String::from_utf8_lossy(&stderr[start..]).into_owned()
}
