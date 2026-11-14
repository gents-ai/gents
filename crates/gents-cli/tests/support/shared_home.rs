use std::future::Future;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use tokio::sync::OnceCell;
use uuid::Uuid;

use super::mocks::MockModelEndpoint;
use super::process::{spawn_server_with_ready_json_recovering, ServeProcess};
use super::{
    allocate_port, graphql_url, node_did_from_init, run_init_json, wait_for_runtime_ready,
};

/// One initialized home served by one `gents runtime server` for every test of
/// a binary that asks for it. It starts on first use and stops when the test
/// process exits. A test that shares it keeps its documents apart by unique
/// ids and never changes node-wide state (configuration, packs, schema,
/// service, restarts) or asserts on a listing another test can add to; such a
/// test starts its own home.
pub struct SharedServedHome {
    /// The `HOME` every CLI process run against this home uses: its User.
    pub home_dir: PathBuf,
    pub graphql: String,
    pub node_did: String,
    pub init: Value,
    held: Mutex<Option<Held>>,
}

/// Dropped in declaration order at process exit: the server stops before its
/// home is removed. The mock model is not dropped: its drop joins a thread that
/// may still be serving a connection, and the process is ending.
struct Held {
    serve: ServeProcess,
    _tempdir: tempfile::TempDir,
    _model: std::mem::ManuallyDrop<MockModelEndpoint>,
}

static SHARED: OnceCell<Result<SharedServedHome, String>> = OnceCell::const_new();

/// Runs one test against the binary's shared served home, on the runtime every
/// such test shares. The CLI's GraphQL clients are process-wide and pool
/// connections on the runtime that opened them, so a test on its own
/// `#[tokio::test]` runtime would hand the next test a pooled connection to
/// the shared port whose runtime is gone. A failed start, or a server that has
/// since exited, is returned as this test's error so every dependent test
/// names the fixture's failure instead of an unrelated connection error.
pub fn with_shared_served_home<F, Fut>(test: F) -> Result<()>
where
    F: FnOnce(&'static SharedServedHome) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    shared_runtime()?.block_on(async {
        let home = SHARED
            .get_or_init(|| async { start().await.map_err(|error| format!("{error:#}")) })
            .await
            .as_ref()
            .map_err(|error| anyhow!("the shared served home failed to start: {error}"))?;
        home.ensure_serving()?;
        test(home).await
    })
}

/// The test binary's one runtime for shared-home tests, built on first use by
/// the test thread that asks; libtest's threads are entry points, outside any
/// runtime.
fn shared_runtime() -> Result<&'static tokio::runtime::Runtime> {
    static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(|error| anyhow!("building the shared-home test runtime: {error}"))
}

impl SharedServedHome {
    fn ensure_serving(&self) -> Result<()> {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let held = held
            .as_mut()
            .context("the shared served home was already stopped")?;
        if let Some(status) = held
            .serve
            .child
            .try_wait()
            .context("checking the shared server")?
        {
            let (stdout, stderr) = held.serve.captured_output()?;
            anyhow::bail!(
                "the shared served home's server exited ({status})\nstdout:\n{stdout}\nstderr:\n{stderr}"
            );
        }
        Ok(())
    }
}

async fn start() -> Result<SharedServedHome> {
    let (mut home, held) = tokio::task::spawn_blocking(|| {
        let tempdir = tempfile::tempdir().context("creating the shared home's tempdir")?;
        let home_dir = tempdir.path().join("home");
        std::fs::create_dir_all(&home_dir)?;
        let model_name = format!("mock-shared-{}", Uuid::new_v4().simple());
        let model = MockModelEndpoint::start(&model_name)?;
        let init = run_init_json(
            &home_dir,
            &[
                "--node-name",
                &format!("cli-shared-{}", Uuid::new_v4().simple()),
                "--model-name",
                &model_name,
                "--inference-url",
                model.endpoint(),
            ],
        )?;
        let node_did = node_did_from_init(&init)?;
        let (serve, port, _readiness) =
            spawn_server_with_ready_json_recovering(&home_dir, allocate_port()?, &[], &[])?;
        Ok::<_, anyhow::Error>((
            SharedServedHome {
                home_dir,
                graphql: graphql_url(port),
                node_did,
                init,
                held: Mutex::new(None),
            },
            Held {
                serve,
                _tempdir: tempdir,
                _model: std::mem::ManuallyDrop::new(model),
            },
        ))
    })
    .await
    .context("starting the shared served home")??;
    held.serve
        .capturing(wait_for_runtime_ready(
            &home.graphql,
            &home.node_did,
            Duration::from_secs(30),
        ))
        .await?;
    *home.held.get_mut().unwrap_or_else(PoisonError::into_inner) = Some(held);
    stop_at_exit()?;
    Ok(home)
}

/// Statics are never dropped, so the server child is stopped and the home
/// removed by an exit hook; libtest leaves through `exit` whether tests passed
/// or failed. A test process killed by a signal leaves the child behind, as a
/// per-test `ServeProcess` does.
fn stop_at_exit() -> Result<()> {
    extern "C" fn stop() {
        if let Some(Ok(home)) = SHARED.get() {
            drop(
                home.held
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take(),
            );
        }
    }
    // SAFETY: `stop` is a non-unwinding `extern "C"` function with no
    // arguments, as `atexit` requires.
    let registered = unsafe { libc::atexit(stop) };
    anyhow::ensure!(registered == 0, "registering the shared home's exit hook");
    Ok(())
}
