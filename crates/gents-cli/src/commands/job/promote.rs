//! `gents job promote`: a finished dynamic run becomes an installed, wired tool.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use gents::config_client::ConfigAccess;
use gents::document_config::Tools;
use gents::pack::authoring::check_dir;
use gents::pack::authoring::test::run_plugin_cases;
use gents::plugin_job::promote::{
    generate_pack, load_dynamic_run, promoted_dir, validate_name, write_promotion, PromoteRefusal,
    PromotionRecord,
};
use gents::plugin_job::view::JobKey;
use serde::Serialize;
use serde_json::Value;

use super::resolve_job;
use crate::cli::{PackDriftArgs, PackInstallArgs, ScopeArgs};
use crate::commands::pack::wire::{wire_in, WireRequest};
use crate::output_format::OutputFormat;

/// A stage of a promotion; `promote` reports each one before it starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PromoteStep {
    Javy,
    Load,
    Generate,
    Build,
    Test,
    Check,
    Install,
    Wire,
}

impl PromoteStep {
    pub fn name(self) -> String {
        format!("{self:?}").to_lowercase()
    }
}

#[derive(Debug, Serialize)]
pub struct PromoteReport {
    pub tool: String,
    pub plugin: String,
    pub dir: PathBuf,
    /// The installed pack's content digest.
    pub digest: String,
    /// `javy --version`.
    pub javy: String,
    pub wire: Value,
    pub record: PromotionRecord,
}

/// The build compiles the workflow with javy, so promotion refuses up front without it.
#[derive(Debug)]
pub struct JavyMissing(pub String);

impl JavyMissing {
    pub fn code(&self) -> &'static str {
        "promote_javy_missing"
    }
}

impl std::fmt::Display for JavyMissing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "javy is not runnable ({}); promotion builds the workflow with javy 8.1.1, which \
             .github/scripts/setup-afterburner-toolchains.sh installs (or set JAVY to its path)",
            self.0
        )
    }
}

impl std::error::Error for JavyMissing {}

/// The refusal code of a promotion that was refused rather than failed.
pub fn refusal_code(error: &anyhow::Error) -> Option<&'static str> {
    error
        .downcast_ref::<PromoteRefusal>()
        .map(PromoteRefusal::code)
        .or_else(|| error.downcast_ref::<JavyMissing>().map(JavyMissing::code))
}

/// The run's handle as the tool name, when it is a pack name.
pub fn default_name(handle: &str) -> Option<String> {
    gents::pack::is_valid_pack_name(handle).then(|| handle.to_owned())
}

/// Refuses an agent with no Tools document: wiring into it would fail after
/// the install.
fn require_agent_tools(agent: &str, tools: Option<Tools>) -> Result<Tools, PromoteRefusal> {
    tools.ok_or_else(|| PromoteRefusal::NoAgentTools(agent.to_owned()))
}

fn javy_version() -> Result<String, JavyMissing> {
    let javy = std::env::var("JAVY").unwrap_or_else(|_| "javy".into());
    match Command::new(&javy).arg("--version").output() {
        Ok(output) if output.status.success() => {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        }
        Ok(output) => Err(JavyMissing(format!(
            "`{javy} --version` exited with {}",
            output.status
        ))),
        Err(error) => Err(JavyMissing(format!("`{javy}`: {error}"))),
    }
}

/// Promotes the dynamic run `selector` names (its address
/// `SESSION_ID:TOOL_CALL_ID` or tool call id) into the installed plugin
/// `gents/<name>`, wired into `agent` (default: the agent that ran it). Every
/// refusal comes before a file is written; a failure after Generate leaves
/// `<home>/packs/promoted/<name>` in place and names its stage.
pub async fn promote(
    access: &ConfigAccess,
    home: &Path,
    actor: &str,
    selector: &str,
    name: Option<&str>,
    agent: Option<&str>,
    report: &mut dyn FnMut(PromoteStep),
) -> Result<PromoteReport> {
    report(PromoteStep::Javy);
    let javy = javy_version()?;

    report(PromoteStep::Load);
    let view = resolve_job(access, actor, selector, None).await?;
    let run = load_dynamic_run(access, JobKey::RowDocId(view.row_doc_id)).await?;
    let name = match name {
        Some(name) => name.to_owned(),
        None => default_name(&run.view.handle)
            .ok_or_else(|| PromoteRefusal::BadName(run.view.handle.clone()))
            .context("the run's handle is not a pack name; pass --name")?,
    };
    validate_name(home, &name)?;
    let agent = agent.map_or_else(|| run.caller_agent_id.clone(), str::to_owned);
    let tools = {
        let (owner, agent_id) = (run.view.scope.node_did.clone(), agent.clone());
        access
            .transact_readonly("job.promote.agent_tools", move |txn| {
                let (owner, agent_id) = (owner.clone(), agent_id.clone());
                Box::pin(async move {
                    gents::document_config::load_agent_tools_in_txn(txn, &owner, &agent_id).await
                })
            })
            .await
            .context("loading the agent's Tools")?
    };
    require_agent_tools(&agent, tools)?;

    report(PromoteStep::Generate);
    let dir = std::path::absolute(promoted_dir(home).join(&name))?;
    // `validate_name` passed, so a directory here is a failed attempt's leftover.
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("removing the leftover {}", dir.display()))?;
    }
    // The step-0 fixture runs the script under wasmtime, which blocks.
    let pack = {
        let (dir, name, run) = (dir.clone(), name.clone(), run.clone());
        tokio::task::spawn_blocking(move || generate_pack(&dir, &name, &run)).await?
    }
    .context("promote generate failed")?;

    report(PromoteStep::Build);
    crate::commands::pack::build::build_pack(&dir, None).context("promote build failed")?;

    report(PromoteStep::Test);
    let cases = run_plugin_cases(&dir)
        .await
        .context("promote test failed")?;
    if !matches!(cases.as_slice(), [only] if only.passed > 0 && only.failures.is_empty()) {
        anyhow::bail!("promote test failed: {}", serde_json::to_string(&cases)?);
    }

    report(PromoteStep::Check);
    let checked = check_dir(&dir).await;
    if !checked.problems.is_empty() {
        anyhow::bail!("promote check failed: {}", checked.problems.join("; "));
    }

    report(PromoteStep::Install);
    // The held access, not `pack install`'s own: a second local access would
    // contend for the home's store claim.
    let install = PackInstallArgs {
        package: dir.display().to_string(),
        inference_slots: Vec::new(),
        preview: false,
        scope: ScopeArgs {
            home: Some(home.to_path_buf()),
            graphql: None,
            node_did: None,
        },
        output: OutputFormat::Json,
        force_rebind_concrete_did: false,
        registry: None,
        drift: PackDriftArgs::default(),
        grant_authority: true,
        wire: Vec::new(),
        expect_digest: None,
        explicit: true,
    };
    let installed = async {
        let source =
            crate::commands::pack::resolve_pack_source(&install.package, None, home).await?;
        crate::commands::pack::install_documents_pack(access, actor, &source, &install, true).await
    }
    .await
    .context("promote install failed")?;

    let record = PromotionRecord {
        name: name.clone(),
        plugin: pack.plugin.clone(),
        script_id: run.view.pin.script_id.clone(),
        address: run.view.address.clone(),
        session_id: run.view.session_id.clone(),
        promoted_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    };
    let recovery = format!(
        "; the tool {plugin} is installed: finish with gents pack wire {plugin} --agent {agent}{owners}, \
         or remove it with gents pack remove {plugin}",
        plugin = pack.plugin,
        owners = run
            .owners
            .iter()
            .map(|owner| format!(" --targets-from {owner}"))
            .collect::<String>(),
    );
    write_promotion(home, &record).with_context(|| format!("promote record failed{recovery}"))?;

    report(PromoteStep::Wire);
    let wire = wire_in(
        access,
        actor,
        home,
        &WireRequest {
            plugin: pack.plugin.clone(),
            agent: agent.clone(),
            targets_from: run.owners.clone(),
            remove: false,
            preview: false,
        },
    )
    .await
    .with_context(|| format!("promote wire failed{recovery}"))?;
    Ok(PromoteReport {
        tool: name,
        plugin: pack.plugin,
        dir,
        digest: installed["digest"].as_str().unwrap_or_default().to_owned(),
        javy,
        wire,
        record,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents::config_client::GraphqlEndpoint;
    use gents::plugin_job::promote::gate;
    use gents_protocol::job::{JobOrigin, JobPinSummary, JobScope, JobState, JobView};

    fn view(state: JobState, origin: JobOrigin) -> JobView {
        JobView {
            address: "session-1:call_1".into(),
            row_doc_id: "row-1".into(),
            session_id: "session-1".into(),
            tool_call_id: "call_1".into(),
            scope: JobScope {
                session_id: "session-1".into(),
                request_id: "request-1".into(),
                node_did: "did:key:z6Mknode".into(),
                requester_did: None,
            },
            handle: "rome".into(),
            state,
            outcome: None,
            pin: JobPinSummary {
                script_id: "script".into(),
                origin,
                engine_digest: "engine".into(),
                plugin_ref: "gents/dynamic_workflows".into(),
                agent_budget: 8,
                cwd: "/work".into(),
                chain_root_doc_id: "row-1".into(),
                resume_of: None,
            },
            progress: None,
            children: vec![],
            created_at: 0,
            adopted_at: 0,
            completed_at: None,
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn javy_missing_is_the_first_refusal() {
        // JAVY is process-wide; the compile lock keeps the build tests off it.
        let _guard = crate::commands::afterburner_build::compile_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let saved = std::env::var_os("JAVY");
        std::env::set_var("JAVY", "/nonexistent/javy");
        let home = tempfile::tempdir().unwrap();
        let access =
            ConfigAccess::Graphql(GraphqlEndpoint::anonymous("http://127.0.0.1:9/graphql"));
        let mut steps = Vec::new();
        let result = promote(
            &access,
            home.path(),
            "did:key:z6Mknode",
            "no-such-job",
            None,
            None,
            &mut |step| steps.push(step),
        )
        .await;
        match saved {
            Some(value) => std::env::set_var("JAVY", value),
            None => std::env::remove_var("JAVY"),
        }
        let error = result.err().expect("a missing javy refuses");
        assert_eq!(refusal_code(&error), Some("promote_javy_missing"));
        let missing = error.downcast_ref::<JavyMissing>().expect("a JavyMissing");
        let message = missing.to_string();
        assert!(message.contains("8.1.1"), "{message}");
        assert!(
            message.contains(".github/scripts/setup-afterburner-toolchains.sh"),
            "{message}"
        );
        assert_eq!(steps, vec![PromoteStep::Javy]);
    }

    #[test]
    fn the_name_defaults_to_the_handle_when_valid() {
        assert_eq!(default_name("rome"), Some("rome".to_owned()));
        assert_eq!(default_name("rome_2"), Some("rome_2".to_owned()));
    }

    #[test]
    fn a_bad_default_handle_requires_a_name() {
        for handle in ["", "Rome", "2rome", "rome-flow", "rome flow"] {
            assert_eq!(default_name(handle), None, "{handle:?}");
        }
    }

    #[test]
    fn an_installed_run_is_refused() {
        let refusal = gate(&view(JobState::Completed, JobOrigin::Installed)).unwrap_err();
        assert_eq!(refusal, PromoteRefusal::NotDynamic);
        assert_eq!(refusal.code(), "promote_not_dynamic");
        assert_eq!(gate(&view(JobState::Completed, JobOrigin::Dynamic)), Ok(()));
    }

    #[test]
    fn an_agent_without_tools_is_refused() {
        let refusal = require_agent_tools("typo", None).unwrap_err();
        assert_eq!(refusal, PromoteRefusal::NoAgentTools("typo".into()));
        assert_eq!(refusal.code(), "promote_no_agent_tools");
        assert!(refusal.to_string().contains("\"typo\""));
        assert_eq!(
            require_agent_tools("caller", Some(Tools::default())),
            Ok(Tools::default())
        );
    }

    #[test]
    fn a_run_that_did_not_complete_is_refused() {
        for state in [
            JobState::Running,
            JobState::Paused,
            JobState::AwaitingUser,
            JobState::Failed,
            JobState::Cancelled,
            JobState::TimedOut,
        ] {
            let refusal = gate(&view(state, JobOrigin::Dynamic)).unwrap_err();
            assert_eq!(refusal, PromoteRefusal::NotCompleted(state));
            assert_eq!(refusal.code(), "promote_not_completed");
            assert!(refusal.to_string().contains("completed"), "{refusal}");
        }
    }
}
