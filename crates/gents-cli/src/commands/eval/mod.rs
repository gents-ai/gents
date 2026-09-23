//! `gents eval`: thin commands over `gents::eval`. Each resolves the
//! launching home the way the other commands do, calls one library function,
//! and writes a table or, with `--json`, the library's own structure.
//!
//! Refusals the library returns are re-raised with exactly their text
//! ([`surface_refusal`]); `main` prints them and exits 1. Clap exits 2 on a
//! usage error.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;
use gents::eval::checks::CheckRegistry;
use gents::eval::runner::embedded::EmbeddedExecutor;
use gents::eval::runner::{RunOptions, TrialExecutor};
use gents::ConfigAccess;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::cli::{EvalCommand, EvalScopeArgs};

/// The launching home a command acts for.
pub(crate) struct EvalContext {
    pub(crate) access: ConfigAccess,
    pub(crate) home_dir: PathBuf,
    /// The home's identity: owner of the runs it launches, and `by` on what
    /// it invalidates.
    pub(crate) owner: String,
}

impl EvalContext {
    pub(crate) async fn resolve(scope: &EvalScopeArgs) -> Result<Self> {
        let (access, home_dir) =
            crate::resolve_config_access(scope.home.as_deref(), scope.graphql.as_deref()).await?;
        let owner = crate::resolve_agent_did(Some(&home_dir), None)?;
        Ok(Self {
            access,
            home_dir,
            owner,
        })
    }

    pub(crate) fn runs_dir(&self) -> PathBuf {
        runs_dir(&self.home_dir)
    }
}

/// `<home>/eval/runs`, where the runner freezes every run.
pub(crate) fn runs_dir(home_dir: &Path) -> PathBuf {
    home_dir.join("eval").join("runs")
}

/// What a command body runs trials with. `dispatch` supplies the embedded
/// executor; tests supply a scripted one.
pub(crate) struct Deps<'a> {
    pub(crate) executor: &'a dyn TrialExecutor,
    pub(crate) registry: &'a CheckRegistry,
    pub(crate) cancel: CancellationToken,
    pub(crate) options: RunOptions,
}

/// A usage error visible from argv alone that a value parser cannot see
/// (it spans several flags). `async_main` raises it as a clap error, exit 2,
/// before anything is read.
pub(crate) fn usage_error(command: &EvalCommand) -> Option<String> {
    match command {
        EvalCommand::Run(args) => run::profiles_by_cell(args).err(),
        _ => None,
    }
}

pub(crate) async fn dispatch(command: EvalCommand) -> Result<()> {
    if let EvalCommand::Cancel(args) = &command {
        return cancel_without_context(args).await.map_err(surface_refusal);
    }
    let ctx = EvalContext::resolve(command.scope()).await?;
    let executor = EmbeddedExecutor::new(gents::DocumentRuntimeOptions::default(), ctx.runs_dir());
    let registry = CheckRegistry::builtin();
    // Only a command that hosts a loop replaces the default interrupt: a
    // read-only command stays killable by Ctrl-C.
    let cancel = if matches!(command, EvalCommand::Run(_) | EvalCommand::Resume(_)) {
        cancel_on_ctrl_c()
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
        EvalCommand::Resume(args) => run::resume(ctx, &args, deps, out).await,
        EvalCommand::List(args) => inspect::list(ctx, &args, out).await,
        EvalCommand::Show(args) => inspect::show(ctx, &args, out).await,
        EvalCommand::Trial(args) => inspect::trial(ctx, &args, out).await,
        EvalCommand::Compare(args) => compare::compare(ctx, &args, out).await,
        EvalCommand::Cancel(args) => manage::cancel_run(ctx, &args, out).await,
        EvalCommand::Invalidate(args) => manage::invalidate(ctx, &args, out).await,
        EvalCommand::Rm(args) => manage::rm(ctx, &args, out).await,
    };
    result.map_err(surface_refusal)
}

/// `gents eval cancel` from argv alone. The marker needs no database, and the
/// process hosting the run may hold the home's embedded node (its store is
/// locked exclusively), so the home's access is opened only after the marker
/// is written, and only to find the job note.
async fn cancel_without_context(args: &crate::cli::EvalRunIdArgs) -> Result<()> {
    let home_dir = crate::home_state::resolve_home_dir(args.scope.home.as_deref());
    let purpose = async {
        let ctx = EvalContext::resolve(&args.scope).await?;
        manage::run_purpose(&ctx.access, &ctx.owner, &args.run_id).await
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    manage::cancel(&runs_dir(&home_dir), &args.run_id, purpose, &mut out).await
}

/// A token Ctrl-C cancels. The loop then stops launching, leaves in-flight
/// trials open for a resume, and the command prints how to resume. A second
/// Ctrl-C exits at once with 130, without waiting for in-flight trials.
pub(crate) fn cancel_on_ctrl_c() -> CancellationToken {
    let token = CancellationToken::new();
    let cancel = token.clone();
    tokio::spawn(async move {
        on_interrupts(tokio::signal::ctrl_c, &cancel, || std::process::exit(130)).await;
    });
    token
}

/// The first interrupt `next` yields cancels `cancel`; the second calls
/// `exit`. A failure to listen ends the handler: the token stays as it is.
async fn on_interrupts<S>(
    mut next: impl FnMut() -> S,
    cancel: &CancellationToken,
    exit: impl FnOnce(),
) where
    S: std::future::Future<Output = std::io::Result<()>>,
{
    if next().await.is_err() {
        return;
    }
    tracing::warn!(
        "interrupt: the run stops launching; resume it to continue, or interrupt again to exit now"
    );
    cancel.cancel();
    if next().await.is_err() {
        return;
    }
    tracing::warn!("second interrupt: exiting without waiting for in-flight trials");
    exit();
}

/// A refusal the library returned, re-raised with exactly its own text
/// whatever context was added above it; any other error unchanged.
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

pub(crate) fn write_json<T: Serialize>(out: &mut dyn Write, value: &T) -> Result<()> {
    serde_json::to_writer_pretty(&mut *out, value)?;
    writeln!(out)?;
    Ok(())
}

/// The commit this binary was built from, recorded on every run it freezes.
pub(crate) fn source_commit() -> String {
    option_env!("GENTS_BUILD_GIT_SHA")
        .unwrap_or("unknown")
        .to_owned()
}

pub(crate) fn source_dirty() -> bool {
    option_env!("GENTS_BUILD_GIT_DIRTY") == Some("true")
}

/// Ruling F14: `<definition>-<unix ms>-<4 random hex>`, unique per call. An
/// operator who wants a run or job reused (the idempotent freeze, a resume)
/// names it with `--run-id` or `--job-id`.
pub(crate) fn default_id(definition_id: &str) -> String {
    let random = uuid::Uuid::new_v4().as_u128() as u16;
    format_default_id(definition_id, chrono::Utc::now().timestamp_millis(), random)
}

fn format_default_id(definition_id: &str, unix_ms: i64, random: u16) -> String {
    format!("{definition_id}-{unix_ms}-{random:04x}")
}

/// Printed above a policy verdict computed with the placeholder defaults.
pub(crate) const UNCALIBRATED_BANNER: &str =
    "policy defaults are uncalibrated until the A/A calibration (M5)";

pub(crate) fn load_policy(arg: &crate::cli::PolicyArg) -> Result<gents::optimization::PolicyV2> {
    use anyhow::Context;
    match arg {
        crate::cli::PolicyArg::Defaults => Ok(gents::optimization::PolicyV2::uncalibrated()),
        crate::cli::PolicyArg::File(path) => {
            let bytes = std::fs::read(path)
                .with_context(|| format!("reading policy {}", path.display()))?;
            serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing policy {}", path.display()))
        }
    }
}

mod compare;
mod inspect;
pub(crate) mod manage;
pub(crate) mod render;
mod run;
#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests {
    use clap::Parser;
    use tokio_util::sync::CancellationToken;

    use super::testing::{executor, Fixture, VALIDATION_CASES};
    use super::*;
    use crate::cli::{parse_cell, CellArg};

    /// `EvalCommand` parsed on its own, without the `gents` parser above it.
    #[derive(Parser)]
    struct Probe {
        #[command(subcommand)]
        command: EvalCommand,
    }

    #[test]
    fn the_read_commands_parse_with_their_scope() {
        let probe = Probe::try_parse_from(["probe", "list", "--all", "--json", "--home", "/tmp/h"])
            .unwrap_or_else(|error| panic!("{error}"));
        let EvalCommand::List(args) = &probe.command else {
            panic!("not list");
        };
        assert!(args.all && args.json);
        assert_eq!(
            probe.command.scope().home.as_deref(),
            Some(std::path::Path::new("/tmp/h"))
        );
        let probe = Probe::try_parse_from(["probe", "trial", "r1", "baseline", "val-a"])
            .unwrap_or_else(|error| panic!("{error}"));
        let EvalCommand::Trial(args) = &probe.command else {
            panic!("not trial");
        };
        assert_eq!((args.cell.as_str(), args.trial_index), ("baseline", None));
    }

    #[test]
    fn a_stray_or_repeated_profile_is_a_usage_error_before_anything_is_read() {
        let usage = |argv: &[&str]| usage_error(&super::testing::eval_command(argv));
        assert_eq!(
            usage(&["run", "d", "--cell", "a=p", "--profile", "other=local"]).as_deref(),
            Some("--profile names cell \"other\", which no --cell declares")
        );
        assert_eq!(
            usage(&[
                "run",
                "d",
                "--cell",
                "a=p",
                "--profile",
                "a=x",
                "--profile",
                "a=y"
            ])
            .as_deref(),
            Some("--profile names cell \"a\" more than once")
        );
        assert_eq!(
            usage(&["run", "d", "--cell", "a=p", "--cell", "a=q"]).as_deref(),
            Some("--cell declares cell \"a\" more than once")
        );
        assert_eq!(
            usage(&["run", "d", "--cell", "a=p", "--profile", "a=x"]),
            None
        );
        assert_eq!(usage(&["list"]), None);
    }

    #[tokio::test]
    async fn the_first_interrupt_cancels_and_the_second_exits() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let (send, receive) = tokio::sync::mpsc::unbounded_channel::<()>();
        let receive = Arc::new(tokio::sync::Mutex::new(receive));
        let token = CancellationToken::new();
        let exited = Arc::new(AtomicBool::new(false));
        let handler = {
            let token = token.clone();
            let exited = exited.clone();
            tokio::spawn(async move {
                let next = || {
                    let receive = receive.clone();
                    async move {
                        receive
                            .lock()
                            .await
                            .recv()
                            .await
                            .ok_or_else(|| std::io::Error::other("closed"))
                    }
                };
                on_interrupts(next, &token, || exited.store(true, Ordering::SeqCst)).await;
            })
        };
        let within = std::time::Duration::from_secs(10);
        send.send(()).unwrap();
        tokio::time::timeout(within, token.cancelled())
            .await
            .expect("the first interrupt cancels the token");
        assert!(!exited.load(Ordering::SeqCst), "one interrupt only cancels");
        send.send(()).unwrap();
        tokio::time::timeout(within, handler)
            .await
            .expect("the second interrupt ends the handler")
            .unwrap();
        assert!(exited.load(Ordering::SeqCst), "a second interrupt exits");

        let listening_failed = CancellationToken::new();
        on_interrupts(
            || async { Err(std::io::Error::other("no handler")) },
            &listening_failed,
            || panic!("never exits"),
        )
        .await;
        assert!(!listening_failed.is_cancelled());
    }

    #[test]
    fn a_refusal_is_surfaced_with_exactly_its_own_text() {
        use anyhow::Context as _;
        let refusal = Err::<(), _>(anyhow::Error::from(gents::eval::report::ReportRefused(
            "no eval run \"x\"".into(),
        )))
        .context("loading the report")
        .unwrap_err();
        assert_eq!(surface_refusal(refusal).to_string(), "no eval run \"x\"");
        assert_eq!(
            surface_refusal(anyhow::anyhow!("plain failure")).to_string(),
            "plain failure"
        );
    }

    #[tokio::test]
    async fn the_fixture_home_runs_a_scripted_two_cell_run() {
        let fixture = Fixture::new().await;
        let outcome = fixture
            .scripted_run(
                "r1",
                &executor(&VALIDATION_CASES[..3]),
                CancellationToken::new(),
            )
            .await;
        assert_eq!(outcome.completed, 24);
        assert!(fixture.ctx.runs_dir().join("r1").is_dir());
    }

    #[test]
    fn a_cell_names_its_pack_and_optionally_a_behavior_with_colons() {
        assert_eq!(
            parse_cell("base=monitor:did:key:z6M:default").unwrap(),
            CellArg {
                cell_id: "base".into(),
                pack: "monitor".into(),
                behavior: Some("did:key:z6M:default".into()),
            }
        );
        assert_eq!(parse_cell("base=/packs/monitor").unwrap().behavior, None);
        assert!(parse_cell("no-equals").is_err());
        assert!(parse_cell("base=").is_err());
        assert!(parse_cell("base=monitor:").is_err());
    }

    #[test]
    fn a_default_id_names_its_definition_the_time_and_four_random_hex() {
        assert_eq!(
            format_default_id("cli-def", 1_700, 0x0a),
            "cli-def-1700-000a"
        );
        assert_ne!(
            format_default_id("cli-def", 1_700, 0x0a),
            format_default_id("cli-def", 1_700, 0x0b),
            "the random part tells two ids of one millisecond apart"
        );
        let first = default_id("cli-def");
        let parts: Vec<&str> = first.rsplitn(3, '-').collect();
        assert_eq!(parts.len(), 3, "{first}");
        assert_eq!(parts[2], "cli-def");
        assert!(parts[1].parse::<i64>().is_ok(), "{first}");
        assert!(
            parts[0].len() == 4 && parts[0].bytes().all(|byte| byte.is_ascii_hexdigit()),
            "{first}"
        );
    }

    #[tokio::test]
    async fn a_pack_name_resolves_like_pack_install_and_a_directory_names_its_behavior() {
        let fixture = Fixture::new().await;
        let directory = crate::commands::pack::resolve_subject_pack(
            &fixture.ctx.home_dir,
            &fixture.pack_arg(),
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(directory.directory(), Some(fixture.pack.as_path()));
        assert_eq!(directory.default_behavior().unwrap(), "monitor");

        let bundled = gents::pack::pack_catalog().unwrap()[0].name.clone();
        let named = crate::commands::pack::resolve_subject_pack(
            &fixture.ctx.home_dir,
            &bundled,
            None,
            false,
        )
        .await
        .unwrap();
        assert!(
            matches!(&named.source, gents::eval::runner::CellSource::InstalledPack { name } if *name == bundled),
            "a compiled-in pack is handed to the runner by name"
        );
        let materialized = crate::commands::pack::resolve_subject_pack(
            &fixture.ctx.home_dir,
            &bundled,
            None,
            true,
        )
        .await
        .unwrap();
        assert!(materialized
            .directory()
            .is_some_and(|dir| dir.join("manifest.json").is_file()));
    }
}
