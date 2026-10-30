use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;
use gents::eval::checks::CheckRegistry;
use gents::eval::runner::embedded::EmbeddedExecutor;
use gents::eval::runner::{RunOptions, TrialExecutor};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::cli::{EvalCommand, EvalScopeArgs};

pub(crate) struct EvalContext {
    pub(crate) access: gents::config_client::ConfigAccess,
    pub(crate) home_dir: PathBuf,
    pub(crate) owner: String,
    pub(crate) runtime: tokio::runtime::Handle,
}

impl EvalContext {
    pub(crate) async fn resolve(
        scope: &EvalScopeArgs,
        runtime: tokio::runtime::Handle,
    ) -> Result<Self> {
        let (access, home_dir) =
            crate::resolve_config_access(scope.home.as_deref(), scope.graphql.as_deref()).await?;
        let owner = crate::resolve_node_did(Some(&home_dir), None)?;
        Ok(Self {
            access,
            home_dir,
            owner,
            runtime,
        })
    }

    pub(crate) fn runs_dir(&self) -> PathBuf {
        runs_dir(&self.home_dir)
    }

    pub(crate) fn jobs_dir(&self) -> PathBuf {
        self.home_dir
            .join(gents::store::local::home::EVAL_DIR_NAME)
            .join("jobs")
    }
}

pub(crate) fn runs_dir(home_dir: &Path) -> PathBuf {
    home_dir
        .join(gents::store::local::home::EVAL_DIR_NAME)
        .join("runs")
}

pub(crate) struct Deps<'a> {
    pub(crate) executor: &'a dyn TrialExecutor,
    pub(crate) registry: &'a CheckRegistry,
    pub(crate) cancel: CancellationToken,
    pub(crate) options: RunOptions,
}

pub(crate) fn usage_error(command: &EvalCommand) -> Option<String> {
    match command {
        EvalCommand::Run(args) => run::profiles_by_cell(args).err(),
        _ => None,
    }
}

pub(crate) async fn dispatch(command: EvalCommand, runtime: tokio::runtime::Handle) -> Result<()> {
    if matches!(
        command,
        EvalCommand::Run(_) | EvalCommand::Batch(_) | EvalCommand::Resume(_) | EvalCommand::Init(_)
    ) {
        crate::process_resources::prepare()?;
    }
    if let EvalCommand::Cancel(args) = &command {
        return cancel_without_context(args, runtime)
            .await
            .map_err(surface_refusal);
    }
    if let EvalCommand::Watch(args) = &command {
        return watch_while_held(args, runtime)
            .await
            .map_err(surface_refusal);
    }
    if let EvalCommand::Checks(args) = &command {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        return checks::checks(&CheckRegistry::builtin(), args, &mut out);
    }
    if let EvalCommand::Init(args) = &command {
        init::preflight(args).await?;
    }
    let ctx = EvalContext::resolve(command.scope(), runtime).await?;
    let executor = EmbeddedExecutor::new(
        gents::DocumentRuntimeOptions::default(),
        ctx.runs_dir(),
        ctx.runtime.clone(),
    );
    let registry = CheckRegistry::builtin();
    let cancel = if matches!(
        command,
        EvalCommand::Run(_) | EvalCommand::Batch(_) | EvalCommand::Resume(_)
    ) {
        cancel_on_ctrl_c(if matches!(command, EvalCommand::Batch(_)) {
            "interrupt: batch runs stop launching; rerun the saved plan to resume, or interrupt again to exit now"
        } else {
            "interrupt: the run stops launching; resume it to continue, or interrupt again to exit now"
        })
    } else {
        CancellationToken::new()
    };
    let deps = Deps {
        executor: &executor,
        registry: &registry,
        cancel,
        options: RunOptions::default(),
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    execute(&ctx, command, &deps, &mut out).await
}

pub(crate) async fn execute(
    ctx: &EvalContext,
    command: EvalCommand,
    deps: &Deps<'_>,
    out: &mut dyn Write,
) -> Result<()> {
    let result = match command {
        EvalCommand::Run(args) => run::run(ctx, &args, deps, out).await,
        EvalCommand::Batch(args) => batch::run(ctx, &args, deps, out).await,
        EvalCommand::Resume(args) => run::resume(ctx, &args, deps, out).await,
        EvalCommand::List(args) => inspect::list(ctx, &args, out).await,
        EvalCommand::Show(args) => inspect::show(ctx, &args, out).await,
        EvalCommand::Trial(args) => inspect::trial(ctx, &args, out).await,
        EvalCommand::Compare(args) => compare::compare(ctx, &args, out).await,
        EvalCommand::Cancel(args) => manage::cancel_run(ctx, &args, out).await,
        EvalCommand::Invalidate(args) => manage::invalidate(ctx, &args, out).await,
        EvalCommand::Rm(args) => manage::rm(ctx, &args, out).await,
        EvalCommand::Watch(args) => {
            watch::watch(
                &ctx.runs_dir(),
                &args,
                None,
                || async { Ok(watch::Documents::Open(ctx)) },
                out,
            )
            .await
        }
        EvalCommand::Gc(args) => manage::gc(ctx, &args, out).await,
        EvalCommand::Checks(args) => checks::checks(deps.registry, &args, out),
        EvalCommand::Init(args) => init::run(ctx, &args, deps, out).await,
    };
    result.map_err(surface_refusal)
}

/// `gents eval cancel` from argv alone: the marker is written before the
/// home's access is opened, since the process hosting the run may hold the
/// home's embedded node.
async fn cancel_without_context(
    args: &crate::cli::EvalRunIdArgs,
    runtime: tokio::runtime::Handle,
) -> Result<()> {
    let home_dir = crate::home_state::resolve_home_dir(args.scope.home.as_deref());
    let purpose = async {
        let ctx = EvalContext::resolve(&args.scope, runtime).await?;
        manage::run_purpose(&ctx.access, &ctx.owner, &args.run_id).await
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    manage::cancel(&runs_dir(&home_dir), &args.run_id, purpose, &mut out).await
}

/// `gents eval watch` from argv. The process hosting the run may hold the
/// home's embedded node; then the watch reads the files the runner leaves
/// and tries the node again only once no live process holds the run.
async fn watch_while_held(
    args: &crate::cli::EvalWatchArgs,
    runtime: tokio::runtime::Handle,
) -> Result<()> {
    use std::io::IsTerminal;

    let stdout = std::io::stdout();
    let screen = (stdout.is_terminal() && !args.once && !args.json).then(watch::frame_screen);
    let mut out = stdout.lock();
    if screen.is_some() {
        write!(out, "{}\x1b[2J", frame::HIDE_CURSOR)?;
    }
    let watched = async {
        match EvalContext::resolve(&args.scope, runtime.clone()).await {
            Ok(ctx) => {
                watch::watch(
                    &ctx.runs_dir(),
                    args,
                    screen,
                    || async { Ok(watch::Documents::Open(&ctx)) },
                    &mut out,
                )
                .await
            }
            Err(error) if watch::store_locked(&error) => {
                tracing::debug!(
                    error = %format!("{error:#}"),
                    "node held, reading runner files"
                );
                let home_dir = crate::home_state::resolve_home_dir(args.scope.home.as_deref());
                watch::watch(
                    &runs_dir(&home_dir),
                    args,
                    screen,
                    || watch::reopen(|| EvalContext::resolve(&args.scope, runtime.clone())),
                    &mut out,
                )
                .await
            }
            Err(error) => Err(error),
        }
    };
    let result = if screen.is_some() {
        tokio::select! {
            result = watched => result,
            _ = tokio::signal::ctrl_c() => Ok(()),
        }
    } else {
        watched.await
    };
    if screen.is_some() {
        write!(out, "{}", frame::SHOW_CURSOR)?;
        out.flush()?;
    }
    result
}

/// A token Ctrl-C cancels: the loop stops launching, in-flight trials stay
/// open for a resume, and a second Ctrl-C exits at once with 130.
pub(crate) fn cancel_on_ctrl_c(first_warning: &'static str) -> CancellationToken {
    let token = CancellationToken::new();
    let cancel = token.clone();
    tokio::spawn(async move {
        on_interrupts(tokio::signal::ctrl_c, &cancel, first_warning, || {
            std::process::exit(130)
        })
        .await;
    });
    token
}

async fn on_interrupts<S>(
    mut next: impl FnMut() -> S,
    cancel: &CancellationToken,
    first_warning: &str,
    exit: impl FnOnce(),
) where
    S: std::future::Future<Output = std::io::Result<()>>,
{
    if next().await.is_err() {
        return;
    }
    tracing::warn!("{first_warning}");
    cancel.cancel();
    if next().await.is_err() {
        return;
    }
    tracing::warn!("second interrupt: exiting without waiting for in-flight trials");
    exit();
}

const PROGRESS_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

pub(crate) trait Progress {
    /// A failed read only delays the lines; it never stops the loop.
    async fn report(&mut self);
}

/// Drive `running` to its end, with `progress` reported on a timer and once
/// more at the end when `print` is set. The read is polled beside the loop,
/// never instead of it: the loop may hold a transaction the read waits on.
pub(crate) async fn follow_progress<T>(
    print: bool,
    progress: &mut impl Progress,
    running: impl std::future::Future<Output = T>,
) -> T {
    tokio::pin!(running);
    let mut ticker = tokio::time::interval(PROGRESS_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let result = loop {
        tokio::select! {
            result = &mut running => break result,
            () = async {
                ticker.tick().await;
                progress.report().await;
            }, if print => {}
        }
    };
    if print {
        progress.report().await;
    }
    result
}

pub(crate) fn surface_refusal(error: anyhow::Error) -> anyhow::Error {
    let verbatim = gents::eval::report::report_refused(&error)
        .map(ToString::to_string)
        .or_else(|| gents::eval::runner::freeze_refused(&error).map(ToString::to_string))
        .or_else(|| gents::eval::already_invalidated(&error).map(ToString::to_string))
        .or_else(|| gents::eval::runner::provider_down(&error).map(ToString::to_string));
    match verbatim {
        Some(text) => anyhow::anyhow!(text),
        None => error,
    }
}

pub(crate) fn write_json<T>(out: &mut dyn Write, value: &T) -> Result<()>
where
    T: Serialize,
{
    serde_json::to_writer_pretty(&mut *out, value)?;
    writeln!(out)?;
    Ok(())
}

pub(crate) fn source_commit() -> String {
    option_env!("GENTS_BUILD_GIT_SHA")
        .unwrap_or("unknown")
        .to_owned()
}

pub(crate) fn source_dirty() -> bool {
    option_env!("GENTS_BUILD_GIT_DIRTY") == Some("true")
}

pub(crate) fn default_id(definition_id: &str) -> String {
    let random = uuid::Uuid::new_v4().as_u128() as u16;
    format_default_id(definition_id, chrono::Utc::now().timestamp_millis(), random)
}

fn format_default_id(definition_id: &str, unix_ms: i64, random: u16) -> String {
    format!("{definition_id}-{unix_ms}-{random:04x}")
}

pub(crate) const UNCALIBRATED_BANNER: &str =
    "policy defaults are uncalibrated until an A/A calibration run has set them";

pub(crate) fn load_policy(
    arg: &crate::cli::PolicyArg,
) -> Result<gents::eval::optimization::PolicyV2> {
    use anyhow::Context;
    match arg {
        crate::cli::PolicyArg::Defaults => Ok(gents::eval::optimization::PolicyV2::uncalibrated()),
        crate::cli::PolicyArg::File(path) => {
            let bytes = std::fs::read(path)
                .with_context(|| format!("reading policy {}", path.display()))?;
            serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing policy {}", path.display()))
        }
    }
}

mod batch;
mod checks;
mod compare;
mod frame;
pub(crate) mod init;
mod inspect;
pub(crate) mod manage;
pub(crate) mod render;
mod run;
#[cfg(test)]
pub(crate) mod testing;
mod watch;
