use crate::support::*;

use anyhow::{Context, Result};
use futures::StreamExt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// `gents` with `user_home` as `HOME`, so the remembered file and the default
/// home both resolve under it.
fn run_gents(user_home: &Path, args: &[&str]) -> Result<Output> {
    Command::new(cli_bin())
        .env("HOME", user_home)
        .env_remove("GENTS_HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env("RUST_LOG", "error")
        .args(args)
        .output()
        .with_context(|| format!("running gents {}", args.join(" ")))
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn remembered_home_path(user_home: &Path) -> PathBuf {
    let config_dir = if cfg!(target_os = "macos") {
        user_home.join("Library/Application Support")
    } else {
        user_home.join(".config")
    };
    config_dir.join("gents/home")
}

#[test]
fn no_flag_with_an_unreadable_remembered_file_names_it() -> Result<()> {
    let user_home = tempfile::tempdir()?;
    let remembered = remembered_home_path(user_home.path());
    fs::create_dir_all(&remembered)?;

    let output = run_gents(user_home.path(), &["status"])?;

    let stderr = stderr_of(&output);
    assert!(!output.status.success(), "stderr:\n{stderr}");
    assert!(
        stderr.contains(&*remembered.to_string_lossy()),
        "expected {} in stderr, got:\n{stderr}",
        remembered.display()
    );
    Ok(())
}

#[test]
fn explicit_home_never_reads_the_remembered_file() -> Result<()> {
    let user_home = tempfile::tempdir()?;
    let home = tempfile::tempdir()?;
    let remembered = remembered_home_path(user_home.path());
    fs::create_dir_all(&remembered)?;

    let output = run_gents(
        user_home.path(),
        &["status", "--home", &home.path().to_string_lossy()],
    )?;

    let stderr = stderr_of(&output);
    assert!(
        !stderr.contains(&*remembered.to_string_lossy()),
        "stderr mentions the remembered file:\n{stderr}"
    );
    assert!(
        stderr.contains(&*home.path().to_string_lossy()),
        "expected the status failure for the explicit home, got:\n{stderr}"
    );
    Ok(())
}

#[test]
fn explicit_home_never_changes_the_remembered_file() -> Result<()> {
    let user_home = tempfile::tempdir()?;
    let home = tempfile::tempdir()?;
    let remembered = remembered_home_path(user_home.path());
    fs::create_dir_all(
        remembered
            .parent()
            .context("remembered file has a parent")?,
    )?;
    let planted = b"/srv/remembered home\n";
    fs::write(&remembered, planted)?;

    run_gents(
        user_home.path(),
        &["status", "--home", &home.path().to_string_lossy()],
    )?;

    assert_eq!(fs::read(&remembered)?, planted);
    Ok(())
}

#[test]
fn bare_gents_without_a_terminal_prints_help_and_exits_2() -> Result<()> {
    let user_home = tempfile::tempdir()?;
    let home = tempfile::tempdir()?;

    for args in [&[][..], &["--home", &*home.path().to_string_lossy()][..]] {
        let output = run_gents(user_home.path(), args)?;

        let stderr = stderr_of(&output);
        assert_eq!(output.status.code(), Some(2), "{args:?} stderr:\n{stderr}");
        assert!(stderr.contains("\n  utils "), "{args:?} stderr:\n{stderr}");
        assert!(output.stdout.is_empty(), "{args:?} wrote to stdout");
        assert_eq!(
            fs::read_dir(user_home.path())?.count(),
            0,
            "{args:?} wrote to HOME"
        );
        assert_eq!(
            fs::read_dir(home.path())?.count(),
            0,
            "{args:?} wrote to the home"
        );
    }
    Ok(())
}

#[test]
fn print_without_a_terminal_on_a_home_not_set_up_says_how_to_set_it_up() -> Result<()> {
    let user_home = tempfile::tempdir()?;
    let home = tempfile::tempdir()?;

    let output = run_gents(
        user_home.path(),
        &["-p", "hello", "--home", &home.path().to_string_lossy()],
    )?;

    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(1), "stderr:\n{stderr}");
    assert!(
        stderr.contains("run `gents` in a terminal"),
        "stderr:\n{stderr}"
    );
    assert!(output.stdout.is_empty(), "wrote to stdout");
    assert_eq!(fs::read_dir(home.path())?.count(), 0, "wrote to the home");
    Ok(())
}

#[test]
fn root_flags_conflict_with_subcommands() -> Result<()> {
    let user_home = tempfile::tempdir()?;

    let output = run_gents(user_home.path(), &["--home", "X", "status"])?;

    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(2), "stderr:\n{stderr}");
    assert!(
        stderr.contains("`status` is a command; put options after it"),
        "stderr:\n{stderr}"
    );
    Ok(())
}

#[test]
fn bare_gents_with_agent_and_no_terminal_exits_2() -> Result<()> {
    let user_home = tempfile::tempdir()?;

    let output = run_gents(user_home.path(), &["--agent", "X"])?;

    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(2), "stderr:\n{stderr}");
    assert!(stderr.contains("\n  utils "), "stderr:\n{stderr}");
    assert_eq!(fs::read_dir(user_home.path())?.count(), 0, "wrote to HOME");
    Ok(())
}

#[test]
fn the_root_agent_flag_conflicts_with_subcommands() -> Result<()> {
    let user_home = tempfile::tempdir()?;

    let output = run_gents(user_home.path(), &["--agent", "X", "status"])?;

    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(2), "stderr:\n{stderr}");
    assert!(
        stderr.contains("`status` is a command; put options after it"),
        "stderr:\n{stderr}"
    );
    Ok(())
}

#[test]
fn print_on_a_broken_explicit_folder_names_it_and_how_to_repair() -> Result<()> {
    let user_home = tempfile::tempdir()?;
    let folder = tempfile::tempdir()?;
    fs::write(folder.path().join("init.json"), "{")?;

    let output = run_gents(
        user_home.path(),
        &["--home", &folder.path().to_string_lossy(), "-p", "hi"],
    )?;

    let stderr = stderr_of(&output);
    assert!(!output.status.success(), "stderr:\n{stderr}");
    assert!(
        stderr.contains("can't be loaded: its settings file (init.json) can't be read"),
        "stderr:\n{stderr}"
    );
    assert!(stderr.contains("Nothing was changed."), "stderr:\n{stderr}");
    assert!(
        stderr.contains("in a terminal and choose to set it aside"),
        "stderr:\n{stderr}"
    );
    assert!(
        folder.path().join("init.json").exists(),
        "the folder was moved"
    );
    Ok(())
}

#[test]
fn print_on_a_broken_default_folder_leaves_it_and_names_how_to_repair() -> Result<()> {
    let user_home = tempfile::tempdir()?;
    let home = user_home.path().join(".gents");
    fs::create_dir(&home)?;
    fs::write(home.join("init.json"), "{")?;

    let output = Command::new(cli_bin())
        .env("HOME", user_home.path())
        .env("GENTS_HOME", &home)
        .env_remove("XDG_CONFIG_HOME")
        .env("RUST_LOG", "error")
        .args(["-p", "hi"])
        .output()?;

    let stderr = stderr_of(&output);
    assert!(!output.status.success(), "stderr:\n{stderr}");
    assert!(
        stderr.contains("can't be loaded: its settings file (init.json) can't be read"),
        "stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("in a terminal and choose to set it aside"),
        "stderr:\n{stderr}"
    );
    assert_eq!(
        fs::read_to_string(home.join("init.json"))?,
        "{",
        "the folder was changed"
    );
    Ok(())
}

const CLIENT_COMMANDS: &[&[&str]] = &[
    &["status"],
    &["sessions", "list"],
    &["automation", "task", "list"],
    &["config", "agent", "list"],
    &["runtime", "p2p", "status"],
    &["runtime", "diagnose"],
    &[
        "utils",
        "data",
        "query",
        "find",
        "--collection",
        "AgentSession",
        "--field",
        "_docID",
    ],
    &["utils", "debug", "background", "list"],
];

#[derive(Clone, Copy, Debug)]
enum HomeSelection {
    Flag,
    Env,
    Remembered,
    Fallback,
}

struct ClientIdentityFixture {
    user_home: tempfile::TempDir,
    identities: tempfile::TempDir,
    other: tempfile::TempDir,
}

impl ClientIdentityFixture {
    fn new() -> Result<Self> {
        Ok(Self {
            user_home: tempfile::tempdir()?,
            identities: tempfile::tempdir()?,
            other: tempfile::tempdir()?,
        })
    }

    fn key(&self) -> PathBuf {
        self.identities.path().join("cli/principal.ed25519.key")
    }

    fn command(
        &self,
        command: &[&str],
        selection: HomeSelection,
        selected: &Path,
    ) -> Result<Command> {
        let mut process = Command::new(cli_bin());
        process
            .env("HOME", self.user_home.path())
            .env("GENTS_DESKTOP_HOME", self.identities.path())
            .env_remove("GENTS_HOME")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_DATA_HOME")
            .env("RUST_LOG", "error")
            .args(command);
        match selection {
            HomeSelection::Flag => {
                process.arg("--home").arg(selected);
            }
            HomeSelection::Env => {
                process.env("GENTS_HOME", selected);
            }
            HomeSelection::Remembered => {
                let remembered = remembered_home_path(self.user_home.path());
                fs::create_dir_all(
                    remembered
                        .parent()
                        .context("remembered file has a parent")?,
                )?;
                fs::write(&remembered, format!("{}\n", selected.display()))?;
            }
            HomeSelection::Fallback => {}
        }
        Ok(process)
    }

    fn run(&self, command: &[&str], selection: HomeSelection, selected: &Path) -> Result<Output> {
        self.command(command, selection, selected)?
            .output()
            .with_context(|| format!("running gents {}", command.join(" ")))
    }

    fn selected_home(&self, selection: HomeSelection) -> PathBuf {
        match selection {
            HomeSelection::Fallback => self.user_home.path().join(".gents"),
            _ => self.other.path().to_path_buf(),
        }
    }
}

fn set_up(home: &Path) -> Result<()> {
    fs::create_dir_all(home)?;
    fs::write(home.join("init.json"), "{}")?;
    Ok(())
}

#[test]
fn a_set_up_home_never_gets_a_new_client_identity_however_it_is_selected() -> Result<()> {
    let mut wrong = Vec::new();
    for selection in [
        HomeSelection::Flag,
        HomeSelection::Env,
        HomeSelection::Remembered,
        HomeSelection::Fallback,
    ] {
        for command in CLIENT_COMMANDS {
            let fixture = ClientIdentityFixture::new()?;
            let home = fixture.selected_home(selection);
            set_up(&home)?;

            let output = fixture.run(command, selection, &home)?;

            let stderr = stderr_of(&output);
            if output.status.code() == Some(2) {
                wrong.push(format!(
                    "{command:?} {selection:?}: did not parse:\n{stderr}"
                ));
            } else if fixture.key().exists() {
                wrong.push(format!(
                    "{command:?} {selection:?}: minted a client identity for set-up {}:\n{stderr}",
                    home.display()
                ));
            } else if !stderr.contains(&*home.to_string_lossy()) {
                wrong.push(format!(
                    "{command:?} {selection:?}: does not name {}:\n{stderr}",
                    home.display()
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n\n"));
    Ok(())
}

#[test]
fn an_explicit_home_never_reads_the_remembered_file_for_any_client_command() -> Result<()> {
    let mut wrong = Vec::new();
    for command in CLIENT_COMMANDS {
        let fixture = ClientIdentityFixture::new()?;
        let remembered = remembered_home_path(fixture.user_home.path());
        fs::create_dir_all(&remembered)?;

        let output = fixture.run(command, HomeSelection::Flag, fixture.other.path())?;

        let stderr = stderr_of(&output);
        if output.status.code() == Some(2) {
            wrong.push(format!("{command:?}: did not parse:\n{stderr}"));
        } else if stderr.contains(&*remembered.to_string_lossy()) {
            wrong.push(format!("{command:?}: read the remembered file:\n{stderr}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n\n"));
    Ok(())
}

const RESOLVING_COMMANDS: &[&[&str]] = &[
    &["status"],
    &["sessions", "list"],
    &["automation", "task", "list"],
    &["config", "agent", "list"],
    &["runtime", "p2p", "status"],
    &[
        "utils",
        "data",
        "query",
        "find",
        "--collection",
        "AgentSession",
        "--field",
        "_docID",
    ],
    &["utils", "debug", "background", "list"],
];

fn place(home: &Path, settings_present: bool, pending: bool) -> Result<()> {
    fs::create_dir_all(home)?;
    if settings_present {
        fs::write(home.join("init.json"), "{}")?;
    }
    if pending {
        fs::write(home.join("init-pending"), "")?;
    }
    Ok(())
}

struct IdentityCase {
    selection: HomeSelection,
    key_present: bool,
    selected: (bool, bool),
    other: (bool, bool),
}

async fn observe_identity(case: &IdentityCase, command: &[&str]) -> Result<&'static str> {
    let fixture = ClientIdentityFixture::new()?;
    let home = fixture.selected_home(case.selection);
    place(&home, case.selected.0, case.selected.1)?;
    let other = match case.selection {
        HomeSelection::Flag | HomeSelection::Env => {
            let remembered = remembered_home_path(fixture.user_home.path());
            fs::create_dir_all(
                remembered
                    .parent()
                    .context("remembered file has a parent")?,
            )?;
            let other = fixture.user_home.path().join("remembered");
            fs::write(&remembered, format!("{}\n", other.display()))?;
            other
        }
        HomeSelection::Remembered => fixture.user_home.path().join(".gents"),
        HomeSelection::Fallback => fixture.other.path().to_path_buf(),
    };
    place(&other, case.other.0, case.other.1)?;
    let held = if case.key_present {
        let paths = gents_desktop_core::client::DesktopPaths::from_root(
            fixture.identities.path().join("cli"),
        );
        paths.ensure_root_dirs().await?;
        gents_desktop_core::client::PrincipalIdentity::load_or_create(&paths).await?;
        Some(fs::read(fixture.key())?)
    } else {
        None
    };

    let output = tokio::process::Command::from(fixture.command(command, case.selection, &home)?)
        .output()
        .await?;

    let stderr = stderr_of(&output);
    anyhow::ensure!(
        output.status.code() != Some(2),
        "{command:?} did not parse:\n{stderr}"
    );
    let after = fs::read(fixture.key()).ok();
    let refused = stderr.contains(&format!(
        "This machine has no Gents client identity for {}",
        home.display()
    ));
    Ok(match (held, after) {
        (Some(before), Some(after)) if before == after && !refused => "load",
        (None, Some(_)) => "create",
        (None, None) if refused => "refuse",
        _ => "neither",
    })
}

#[tokio::test]
async fn every_client_command_decides_its_identity_by_the_selected_home() -> Result<()> {
    use gents_lean_vocab::families::install_repair_client_commands as family;
    let cases = family::cases::<String, String>();
    let identities = cases
        .iter()
        .map(|case| IdentityCase {
            selection: match case.input.selection.as_str() {
                "flag" => HomeSelection::Flag,
                "env" => HomeSelection::Env,
                "remembered" => HomeSelection::Remembered,
                "fallback" => HomeSelection::Fallback,
                other => panic!("unknown home selection {other}"),
            },
            key_present: case.input.key_present,
            selected: (
                case.input.selected.settings_present,
                case.input.selected.pending,
            ),
            other: (case.input.other.settings_present, case.input.other.pending),
        })
        .collect::<Vec<_>>();
    let mut observations = futures::stream::iter(identities.iter().flat_map(|identity| {
        RESOLVING_COMMANDS
            .iter()
            .map(move |command| observe_identity(identity, command))
    }))
    .buffered(std::thread::available_parallelism()?.get());
    for (case, identity) in cases.iter().zip(&identities) {
        for command in RESOLVING_COMMANDS {
            let observed = observations
                .next()
                .await
                .context("every command of every case is observed")??;
            eprintln!(
                "gents {} with the home by {:?}",
                command.join(" "),
                identity.selection
            );
            case.check(observed.to_owned());
        }
    }
    Ok(())
}

const FIRST_RUN_HOME_ENV: &str = "GENTS_TEST_FIRST_RUN_HOME";
const FIRST_RUN_STEP_ENV: &str = "GENTS_TEST_FIRST_RUN_STEP";
const FIRST_RUN_GRAPHQL_ENV: &str = "GENTS_TEST_FIRST_RUN_GRAPHQL";

fn first_run_step(user_home: &Path, home: &Path, step: &str, graphql: &str) -> Result<()> {
    let output = Command::new(std::env::current_exe()?)
        .args([
            "cli_onboarding::desktop_first_run_in_fixture_home",
            "--exact",
            "--include-ignored",
            "--nocapture",
        ])
        .env("HOME", user_home)
        .env_remove("GENTS_HOME")
        .env_remove(gents_desktop_core::client::DESKTOP_HOME_ENV)
        .env(FIRST_RUN_HOME_ENV, home)
        .env(FIRST_RUN_STEP_ENV, step)
        .env(FIRST_RUN_GRAPHQL_ENV, graphql)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "first-run step {step} failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

/// The desktop's first run on a new machine with no inference provider: its
/// provisioning call sets up the home and its Engineer, the node is served,
/// and the desktop's own writes then reach that Engineer, whose backend is
/// still disabled (Lean `SelfConfig.candidateResolution`: deferred).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_first_run_without_inference_sets_up_the_engineer() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let user_home = tempdir.path();
    let home = user_home.join(".gents");
    first_run_step(user_home, &home, "provision", "")?;
    let stored = gents_service::home_state::read_init_config(&home)?
        .context("the desktop's provisioning initialized no home")?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let (mut server, _) = spawn_server_with_ready_json(
        user_home,
        port,
        &["--home", home.to_str().context("home path")?],
        &[],
    )?;
    wait_for_port(port, &mut server)?;
    wait_for_runtime_ready(
        &graphql,
        &stored.node_did,
        std::time::Duration::from_secs(60),
    )
    .await?;
    first_run_step(user_home, &home, "desktop", &graphql)?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "run by desktop_first_run_without_inference_sets_up_the_engineer in the fixture's HOME"]
async fn desktop_first_run_in_fixture_home() -> Result<()> {
    let home = PathBuf::from(std::env::var_os(FIRST_RUN_HOME_ENV).context("fixture home")?);
    let step = std::env::var(FIRST_RUN_STEP_ENV).context("fixture step")?;
    if step == "provision" {
        let root = home.with_file_name("root");
        fs::create_dir_all(&root)?;
        return gents_server::server_host::ensure_standard_home(
            &gents_store::test_support::runtime_handle(),
            gents_server::server_host::ProvisionOptions {
                home,
                node_name: "Forge".to_owned(),
                tool_ceiling: gents_service::args::ToolCeilingArg::Readwrite,
                tool_root: Some(fs::canonicalize(root)?),
                store_key_custody: gents_store::local::store_key::StoreKeyCustodyChoice::File,
                default_packs: true,
            },
        )
        .await;
    }
    let graphql = std::env::var(FIRST_RUN_GRAPHQL_ENV).context("fixture graphql")?;
    let stored = gents_service::home_state::read_init_config(&home)?.context("initialized home")?;
    let owner = stored.node_did.as_str();
    let user = gents_service::home_state::client_identity_for(&home).await?;
    let access = gents_store::config_client::ConfigAccess::Graphql(
        gents_store::config_client::GraphqlEndpoint::as_principal(graphql, user.did())
            .with_client_signing_capability(
                gents_store::config_client::ClientSigningCapability::from_ed25519_key(
                    user.private_key_bytes(),
                )?,
            )?
            .with_client_signed_agent_creates(),
    );
    let agents = access
        .read_view(&gents_documents::views::agents::AgentList::new(owner))
        .await?;
    let engineer = agents
        .iter()
        .find(|row| {
            row.agent
                .tags
                .iter()
                .any(|tag| tag == gents_tools::builtin::self_config::ENGINEER_AGENT_TAG)
        })
        .with_context(|| format!("no Engineer among {agents:?}"))?
        .agent
        .clone();
    anyhow::ensure!(
        engineer.enabled && engineer.display_name.as_deref() == Some("The Engineer"),
        "the Engineer is not set up: {engineer:?}"
    );
    let engineer_did = engineer.agent_did.clone();
    let read = |agent_did: String| {
        let access = access.clone();
        async move {
            access
                .transact_readonly("test.first_run.engineer", |txn| {
                    let agent_did = agent_did.clone();
                    Box::pin(async move {
                        gents_documents::load_agent_context_and_tools_in_txn(txn, owner, &agent_did)
                            .await
                    })
                })
                .await
        }
    };
    let (context, tools) = read(engineer_did.clone()).await?;
    anyhow::ensure!(
        context.as_ref().and_then(|c| c.system_prompt.as_deref())
            == Some(gents_protocol::ENGINEER_PROMPT),
        "the Engineer's prompt is not the shared one: {context:?}"
    );
    anyhow::ensure!(
        tools.as_ref().and_then(|t| t.self_config.as_ref())
            == Some(&gents_tools::builtin::self_config::engineer_self_config()),
        "the Engineer's tools lack its self-configuration: {tools:?}"
    );
    let backends: Vec<gents_documents::InferenceBackend> = access
        .transact_readonly("test.first_run.backends", |txn| {
            Box::pin(async move {
                let refs = gents_documents::ConfigReferences::load_in_txn(txn, owner).await?;
                refs.documents()
                    .filter(|((collection, _), _)| {
                        *collection == gents_store::collection::Collection::InferenceBackend
                    })
                    .map(|(_, value)| Ok(serde_json::from_value(value.clone())?))
                    .collect::<anyhow::Result<Vec<_>>>()
            })
        })
        .await?;
    anyhow::ensure!(
        !backends.is_empty() && backends.iter().all(|backend| !backend.enabled),
        "the node must have no enabled inference: {backends:?}"
    );
    gents_desktop_core::client::patch_config_components_on(
        &access,
        owner,
        &[(
            gents_documents::patch::SelfConfigTarget::Agent,
            engineer_did.clone(),
            vec![(
                "display_name".to_owned(),
                Some(serde_json::json!("Forge's Engineer")),
            )],
        )],
    )
    .await
    .context("the desktop's edit of the Engineer before inference")?;
    let renamed = access
        .read_view(
            &gents_documents::views::agents::AgentList::new(owner)
                .agent_did(&engineer_did)
                .limit(1),
        )
        .await?;
    anyhow::ensure!(
        renamed
            .first()
            .and_then(|row| row.agent.display_name.as_deref())
            == Some("Forge's Engineer"),
        "the Engineer's edit was not applied: {renamed:?}"
    );
    Ok(())
}
