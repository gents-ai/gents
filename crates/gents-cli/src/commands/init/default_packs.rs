use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::cli::ScopeArgs;
use crate::commands::pack as command;

/// The packs a fresh Engineer home starts with. Each is resolved by name,
/// from the home's pack store first and then the registry, at the newest
/// version the registry publishes: the registry resolves a name without a
/// version, so none is pinned here. Nothing is embedded in the binary.
pub(crate) const ENGINEER_DEFAULT_PACKS: &[&str] = &[
    "gents/pack_builder",
    "gents/web",
    "gents/dynamic_workflows",
    "gents/charts",
    "gents/data_tables",
];

/// The default packs whose approval is also the consent to dynamic workflow
/// scripts: approving one sets its job block to `dynamic: run` as it is wired
/// (Lean `consentedDynamic`), since a pack can only ship `check`, which
/// refuses every spawn a script asks for.
const DYNAMIC_RUN_ON_APPROVAL: &[&str] = &["gents/dynamic_workflows"];

/// How long one default pack may take to resolve. Init succeeds without its
/// default packs, so an unreachable registry must not hold a first run for
/// the operating system's connection timeout once per pack.
const RESOLVE_BOUND: Duration = Duration::from_secs(20);

/// Who answers when a default pack's plugins request permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Approval {
    /// Ask on the terminal, pack by pack.
    Ask,
    /// `--approve-default-packs`: the caller approved every request.
    Approved,
    /// Nobody can be asked and nothing was approved: a pack that requests
    /// permissions is left uninstalled and reported.
    Unattended,
}

impl Approval {
    /// A terminal is asked; anything else needs the explicit flag.
    pub(crate) fn resolve(approve: bool, may_prompt: bool) -> Self {
        use std::io::IsTerminal as _;
        if approve {
            Self::Approved
        } else if may_prompt && std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
            Self::Ask
        } else {
            Self::Unattended
        }
    }
}

fn install_command(home: &Path, pack: &str, engineer: &str, grant: bool, wire: bool) -> String {
    format!(
        "gents config pack install {pack} --home {}{}{}",
        home.display(),
        if grant { " --grant-authority" } else { "" },
        if wire {
            format!(" --wire {engineer}")
        } else {
            String::new()
        },
    )
}

/// Installs `packs` into the fresh home `home` for its Engineer `engineer`:
/// each is announced, the permissions its plugins request are approved per
/// pack through `approval` and granted through the pack installer's own
/// consent, its plugins are wired into the Engineer's Tools and its skills
/// attached to the Engineer's Context. A pack that cannot be resolved,
/// approved or installed is reported with the command that installs it
/// later; it never fails the init.
pub(crate) async fn install(
    home: &Path,
    engineer: &str,
    packs: &[&str],
    registry: Option<&str>,
    approval: Approval,
) -> Value {
    eprintln!(
        "\nThe Engineer starts with these packs: {}.\nEach is installed from this home's pack store or the registry. {}",
        packs.join(", "),
        match approval {
            Approval::Ask => "Before a pack whose plugins request permissions is installed, you are asked to approve them.",
            Approval::Approved => "The permissions their plugins request were approved with --approve-default-packs.",
            Approval::Unattended => "A pack whose plugins request permissions is not installed without your approval; each is listed with the command that installs it.",
        }
    );
    let mut installed = Vec::new();
    let mut not_installed = Vec::new();
    for pack in packs {
        match install_one(home, engineer, pack, registry, approval).await {
            Ok(Outcome::Installed(report)) => {
                eprintln!("  installed {pack}");
                installed.push(report);
            }
            Ok(Outcome::Unapproved { requested, wire }) => {
                let command = install_command(home, pack, engineer, true, wire);
                eprintln!("  {pack} was not installed: its plugins request permissions that were not approved. Install it with: {command}");
                not_installed.push(json!({
                    "pack": pack,
                    "reason": "its plugins request permissions that were not approved",
                    "requested_permissions": requested,
                    "install_with": command,
                }));
            }
            Err(error) => {
                let command = install_command(home, pack, engineer, false, false);
                eprintln!(
                    "  {pack} was not installed: {error:#}. Install it later with: {command}"
                );
                not_installed.push(json!({
                    "pack": pack,
                    "reason": format!("{error:#}"),
                    "install_with": command,
                }));
            }
        }
    }
    json!({"installed": installed, "not_installed": not_installed})
}

enum Outcome {
    Installed(Value),
    Unapproved { requested: Value, wire: bool },
}

async fn install_one(
    home: &Path,
    engineer: &str,
    pack: &str,
    registry: Option<&str>,
    approval: Approval,
) -> Result<Outcome> {
    let source = tokio::time::timeout(
        RESOLVE_BOUND,
        command::resolve_pack_source(pack, registry, home),
    )
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "it is not in this home's pack store and the registry did not answer within {} seconds",
            RESOLVE_BOUND.as_secs()
        )
    })??;
    let mut dependencies = Vec::new();
    for coordinate in &source.manifest().metadata.dependencies {
        dependencies.push(
            tokio::time::timeout(
                RESOLVE_BOUND,
                command::resolve_pack_source(coordinate, registry, home),
            )
            .await
            .map_err(|_| anyhow::anyhow!("its dependency {coordinate} did not resolve in time"))?
            .with_context(|| format!("resolving its dependency {coordinate}"))?,
        );
    }
    let requested = command::wire::requested_authority(
        source.manifest(),
        &dependencies
            .iter()
            .map(|dependency| dependency.manifest())
            .collect::<Vec<_>>(),
    )?;
    let wire = !source.manifest().metadata.plugins.is_empty();
    let asks = requested.as_array().is_some_and(|asks| !asks.is_empty());
    let grant = asks
        && match approval {
            Approval::Approved => true,
            Approval::Unattended => false,
            Approval::Ask => {
                eprintln!("\n{pack} requests these permissions for its plugins:");
                for ask in requested.as_array().into_iter().flatten() {
                    eprintln!(
                        "  {}: {}",
                        ask["plugin"].as_str().unwrap_or_default(),
                        ask["asks"].as_str().unwrap_or_default()
                    );
                }
                if DYNAMIC_RUN_ON_APPROVAL.contains(&pack) {
                    eprintln!("  Approving also lets workflow scripts the model writes start agents (job.policy.dynamic: run).");
                }
                crate::interactive_backend::confirm(
                    &format!("Install {pack} and allow this?"),
                    false,
                )
                .await
            }
        };
    if asks && !grant {
        return Ok(Outcome::Unapproved { requested, wire });
    }
    let scope = ScopeArgs {
        home: Some(home.to_owned()),
        graphql: None,
        node_did: None,
    };
    let coordinate = format!(
        "{}/{}",
        source.manifest().metadata.namespace,
        source.manifest().name
    );
    // One held access installs the pack, wires its plugins and attaches its
    // skills, and its node is drained before the next pack opens the home.
    let (access, owner) = command::resolve_scope_owner(&scope).await?;
    let installed = install_as_engineer(
        &access,
        &owner,
        home,
        engineer,
        &format!("{coordinate}@{}", source.manifest().version),
        source.digest(),
    )
    .await;
    let attached = async {
        installed?;
        let host_tool_roots = command::wire::grant_pack_root_readers(&access, &owner, &coordinate)
            .await
            .with_context(|| {
                format!("{pack} is installed, but its agents were not granted their tool root")
            })?;
        let wired = if wire {
            command::wire_pack_plugins_consenting(
                &access,
                &owner,
                home,
                &source,
                &[engineer.to_owned()],
                grant && DYNAMIC_RUN_ON_APPROVAL.contains(&coordinate.as_str()),
            )
            .await
            .with_context(|| {
                format!("{pack} is installed, but it was not wired into the Engineer")
            })?
        } else {
            Vec::new()
        };
        let skills = command::wire::attach_pack_skills(
            &access,
            &owner,
            engineer,
            &coordinate,
            source.digest(),
        )
        .await
        .with_context(|| {
            format!("{pack} is installed, but its skills were not attached to the Engineer")
        })?;
        Ok::<_, anyhow::Error>((wired, skills, host_tool_roots))
    }
    .await;
    if let Some(node) = access.local_node() {
        if let Err(error) = node.shutdown().await {
            tracing::warn!(error = %gents_protocol::error_chain::ErrorChain(&error), "failed to shut down embedded node");
        }
    }
    drop(access);
    let (wired, skills, host_tool_roots) = attached?;
    Ok(Outcome::Installed(json!({
        "pack": coordinate,
        "version": source.manifest().version,
        "digest": source.digest(),
        "granted_permissions": if grant { requested } else { json!([]) },
        "wired": wired,
        "skills": skills,
        "host_tool_roots": host_tool_roots,
    })))
}

/// Installs `package` at `digest` as the Engineer `engineer`, so the pack's
/// documents are the Engineer's: it reads them, references them and removes
/// the pack again. The user's approval of what the plugins ask for is the
/// caller's; the Engineer's pack authority is what grants it (Lean
/// `installerProvenance`), so the install records `operator`.
async fn install_as_engineer(
    access: &gents::ConfigAccess,
    owner: &str,
    home: &Path,
    engineer: &str,
    package: &str,
    digest: &str,
) -> Result<Value> {
    let receipt = engineer_config(access, owner, home, engineer)
        .await?
        .call(json!({"argv": ["pack", "install", package, "--digest", digest]}).to_string())
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    serde_json::from_str(&receipt).context("the Engineer's install receipt is not JSON")
}

/// The config tool as the runtime gives it to the Engineer `engineer`, with
/// the grants its own Tools hold. `access` holds the home's node.
async fn engineer_config(
    access: &gents::ConfigAccess,
    owner: &str,
    home: &Path,
    engineer: &str,
) -> Result<Box<dyn gents::inference::llm::tool::ToolDyn>> {
    use gents::tools::builtin::self_config;
    let node = access
        .local_node()
        .context("a runtime is serving this home; ask its Engineer to install the pack")?
        .clone();
    let stored = crate::home_state::read_init_config(home)?.context("home is not initialized")?;
    let runtime = crate::home_state::load_initialized_home_identity(home, &stored)?;
    let binding =
        crate::commands::inference_binding::load_bound_agent_binding(&node, owner, engineer)
            .await?;
    let agent = gents::store::identity::resolve_agent_identity(runtime.as_ref(), &binding)?;
    let held = access
        .transact_readonly("cli.init.engineer_grants", |txn| {
            Box::pin(async move {
                gents::documents::load_agent_context_and_tools_in_txn(txn, owner, engineer).await
            })
        })
        .await?
        .1
        .and_then(|tools| tools.self_config)
        .context("the Engineer's Tools hold no self_config grant")?;
    self_config::build_runtime_self_config_tools(
        std::sync::Arc::default(),
        node,
        agent,
        runtime,
        &gents::tools::surface::SelfConfigToolConfig {
            enabled: held.enable_self_config.unwrap_or(false),
            agent_did: engineer.to_owned(),
            categories: held
                .self_config_categories
                .unwrap_or_default()
                .into_iter()
                .collect(),
            no_lockout: held.self_config_no_lockout.unwrap_or(false),
            preview: held.self_config_preview.unwrap_or(false),
            enable_pack_install: held.enable_pack_install.unwrap_or(false),
            enable_pack_authority: held.enable_pack_authority.unwrap_or(false),
            ..Default::default()
        },
        std::sync::Arc::new(gents::plugin::executor::PluginExecutor::new(Some(
            home.to_owned(),
        ))),
    )
    .await?
    .into_iter()
    .find(|tool| tool.name() == self_config::CONFIG_TOOL_NAME)
    .context("the Engineer's Tools do not grant the config tool")
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    fn write_pack(dir: &Path, manifest: Value, files: &[(&str, Vec<u8>)]) {
        std::fs::create_dir_all(dir.join("plugins")).unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        std::fs::write(dir.join("README.md"), "# fixture").unwrap();
        for (path, bytes) in files {
            std::fs::write(dir.join(path), bytes).unwrap();
        }
    }

    /// Every Context's skills and every Tools' plugin selections in `home`.
    async fn engineer_documents(home: &Path) -> Value {
        let (access, _) = crate::resolve_config_access(Some(home), None)
            .await
            .unwrap();
        let response = access
            .execute("{ AgentContext { skill_ids } Tools { integrations } }")
            .await
            .unwrap();
        if let Some(node) = access.local_node() {
            node.shutdown().await.expect("node shutdown");
        }
        drop(access);
        let rows = |collection: &str, pointer: &str| {
            response["data"][collection]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|row| row.pointer(pointer))
                .flat_map(|value| value.as_array().cloned().unwrap_or_default())
                .collect::<Vec<_>>()
        };
        json!({
            "skills": rows("AgentContext", "/skill_ids"),
            "plugins": rows("Tools", "/integrations/plugins"),
        })
    }

    /// A fresh Engineer home takes its default packs from the pack store: a
    /// skills pack installs and its skill reaches the Engineer's Context; a
    /// plugin pack that requests permissions is left out until they are
    /// approved, then installs with them granted and its plugin wired into
    /// the Engineer's Tools; and a pack nothing publishes, with the registry
    /// unreachable, is reported with the command that installs it later.
    #[tokio::test]
    async fn default_packs_install_from_the_store_with_per_pack_approval() {
        let home = tempfile::tempdir().unwrap();
        let cli = crate::cli::Cli::try_parse_from([
            "gents",
            "init",
            "--store-key-custody",
            "file",
            "--engineer",
            "--no-default-packs",
            "--inference-url",
            "http://127.0.0.1:9/v1",
            "--home",
            home.path().to_str().unwrap(),
        ])
        .unwrap();
        let Some(crate::cli::Command::Init(args)) = cli.command else {
            panic!("expected init")
        };
        let initialized = crate::commands::init::init_home(args).await.unwrap();
        assert_eq!(initialized.get("default_packs"), None, "{initialized}");
        let engineer = initialized["default_agent_did"]
            .as_str()
            .unwrap()
            .to_owned();

        let packs = tempfile::tempdir().unwrap();
        let skills = packs.path().join("engineer_skills");
        write_pack(
            &skills,
            json!({
                "manifest_version": 1, "name": "engineer_skills", "namespace": "fixture",
                "version": "1.0.0", "description": "A skill for the Engineer",
                "authors": ["tests"], "tags": [], "kind": "documents",
                "assets": ["README.md", "pack_config.json"], "dependencies": [],
                "config": "pack_config.json",
            }),
            &[(
                "pack_config.json",
                serde_json::to_vec(&json!({
                    "node": {"node_did": "${GENTS_PACK_NODE_DID}"},
                    "skills": [{"skill_id": "fixture-pack-building",
                        "node_did": "${GENTS_PACK_NODE_DID}", "name": "pack-building",
                        "description": "How to build a pack.", "instructions": "Build it."}],
                }))
                .unwrap(),
            )],
        );
        let afb = tokio::task::spawn_blocking(|| {
            crate::commands::plugin::testing::build_plugin_afb(
                "fetch",
                b"fn main() { println!(\"{{}}\"); }",
            )
        })
        .await
        .unwrap();
        let tools = packs.path().join("engineer_tools");
        write_pack(
            &tools,
            json!({
                "manifest_version": 1, "name": "engineer_tools", "namespace": "fixture",
                "version": "1.0.0", "description": "A plugin that asks for an environment variable",
                "authors": ["tests"], "tags": [], "kind": "plugins",
                "assets": ["README.md", "plugins/fetch.afb"],
                "plugins": [{
                    "name": "fetch", "description": "test", "artifact": "plugins/fetch.afb",
                    "language": "rust", "input_schema": {"type": "object"},
                    "manifold": {"fs": "None", "net": "None", "crypto": false,
                        "child_process": false, "env": {"AllowList": ["SOME_VAR"]},
                        "allow_exit": false, "http_timeout_ms": null, "listen": "None"},
                }],
            }),
            &[("plugins/fetch.afb", afb)],
        );
        for dir in [&skills, &tools] {
            gents::pack::local::open(&gents::pack::local::LocalName::Path(dir), home.path())
                .unwrap();
        }
        let names = [
            "fixture/engineer_skills",
            "fixture/engineer_tools",
            "fixture/unpublished",
        ];
        let unreachable = Some("http://127.0.0.1:9");

        let report = install(
            home.path(),
            &engineer,
            &names,
            unreachable,
            Approval::Unattended,
        )
        .await;
        assert_eq!(
            report["installed"][0]["pack"], "fixture/engineer_skills",
            "{report}"
        );
        assert_eq!(report["installed"].as_array().unwrap().len(), 1, "{report}");
        let pending = &report["not_installed"];
        assert_eq!(pending[0]["pack"], "fixture/engineer_tools", "{report}");
        assert_eq!(
            pending[0]["requested_permissions"][0]["plugin"], "fetch",
            "{report}"
        );
        let later = pending[0]["install_with"].as_str().unwrap();
        assert!(
            later.starts_with("gents config pack install fixture/engineer_tools --home ")
                && later.contains(" --grant-authority")
                && later.ends_with(&format!(" --wire {engineer}")),
            "{later}"
        );
        assert_eq!(pending[1]["pack"], "fixture/unpublished", "{report}");
        assert!(
            pending[1]["install_with"]
                .as_str()
                .unwrap()
                .starts_with("gents config pack install fixture/unpublished --home "),
            "{report}"
        );
        assert!(
            gents::plugin::store::read_record(home.path(), "fixture", "fetch").is_err(),
            "nothing is granted without approval"
        );
        let documents = engineer_documents(home.path()).await;
        assert_eq!(
            documents["skills"],
            json!(["fixture-pack-building"]),
            "{documents}"
        );

        let report = install(
            home.path(),
            &engineer,
            &names[1..2],
            unreachable,
            Approval::Approved,
        )
        .await;
        assert_eq!(report["not_installed"], json!([]), "{report}");
        assert_eq!(
            report["installed"][0]["granted_permissions"][0]["plugin"], "fetch",
            "{report}"
        );
        let record = gents::plugin::store::read_record(home.path(), "fixture", "fetch").unwrap();
        assert!(
            record.granted.is_some(),
            "the approval is the recorded grant"
        );
        let documents = engineer_documents(home.path()).await;
        assert_eq!(
            documents["plugins"][0]["plugin"], "fixture/fetch",
            "{documents}"
        );
    }

    /// A default pack is installed as the Engineer, so what it ships is the
    /// Engineer's to use and to remove (gents-ai/packs#61): the pack's agent
    /// is in its agent list, it targets and wires that agent, and it removes
    /// the pack with everything the pack left.
    #[tokio::test]
    async fn the_engineer_owns_the_default_packs_it_starts_with() {
        let home = tempfile::tempdir().unwrap();
        let cli = crate::cli::Cli::try_parse_from([
            "gents",
            "init",
            "--store-key-custody",
            "file",
            "--engineer",
            "--no-default-packs",
            "--model-name",
            "test-model",
            "--inference-url",
            "http://127.0.0.1:9/v1",
            "--home",
            home.path().to_str().unwrap(),
        ])
        .unwrap();
        let Some(crate::cli::Command::Init(args)) = cli.command else {
            panic!("expected init")
        };
        let initialized = crate::commands::init::init_home(args).await.unwrap();
        let engineer = initialized["default_agent_did"]
            .as_str()
            .unwrap()
            .to_owned();
        let afb = tokio::task::spawn_blocking(|| {
            crate::commands::plugin::testing::build_plugin_afb(
                "probe",
                b"fn main() { println!(\"{{}}\"); }",
            )
        })
        .await
        .unwrap();
        let packs = tempfile::tempdir().unwrap();
        let crew = packs.path().join("crew");
        write_pack(
            &crew,
            json!({
                "manifest_version": 1, "name": "crew", "namespace": "fixture",
                "version": "1.0.0", "description": "An agent, its target and a plugin",
                "authors": ["tests"], "tags": [], "kind": "documents",
                "assets": ["README.md", "pack_config.json", "plugins/probe.afb"],
                "dependencies": [], "config": "pack_config.json",
                "inference_slots": [{"name": "worker", "description": "The worker.",
                    "agents": ["crew-worker"]}],
                "plugins": [{
                    "name": "probe", "description": "test", "artifact": "plugins/probe.afb",
                    "language": "rust", "input_schema": {"type": "object"},
                    "manifold": {"fs": "None", "net": "None", "crypto": false,
                        "child_process": false, "env": {"AllowList": ["SOME_VAR"]},
                        "allow_exit": false, "http_timeout_ms": null, "listen": "None"},
                }],
            }),
            &[
                ("plugins/probe.afb", afb),
                (
                    "pack_config.json",
                    serde_json::to_vec(&json!({
                        "node": {},
                        "agents": [{"agent_slot": "crew-worker", "context_id": "crew-context",
                            "display_name": "Crew worker", "description": "Does one task.",
                            "inference_profile_id": "gents:inference-slot:worker"}],
                        "contexts": [{"context_id": "crew-context", "system_prompt": "Work.",
                            "tools_id": "crew-tools"}],
                        "tools": [{"tools_id": "crew-tools", "display_name": "Read-only files",
                            "host": {"files": {"mode": "ReadOnly"}}}],
                        "agent_targets": [{"agent_did": "gents:agent-slot:crew-worker",
                            "name": "crew-worker", "target_id": "crew-worker",
                            "target_node_did": "${GENTS_PACK_NODE_DID}",
                            "description": "The crew worker."}],
                    }))
                    .unwrap(),
                ),
            ],
        );
        gents::pack::local::open(&gents::pack::local::LocalName::Path(&crew), home.path()).unwrap();

        let report = install(
            home.path(),
            &engineer,
            &["fixture/crew"],
            Some("http://127.0.0.1:9"),
            Approval::Approved,
        )
        .await;
        assert_eq!(report["not_installed"], json!([]), "{report}");
        assert_eq!(
            report["installed"][0]["wired"][0]["plugin"], "fixture/probe",
            "{report}"
        );
        assert_eq!(
            gents::pack::read_home_install(home.path(), "fixture/crew")
                .unwrap()
                .unwrap()
                .installed_via,
            Some(gents::pack::InstalledVia::Operator),
            "an install under the Engineer's pack authority records operator"
        );

        let (access, owner) = command::resolve_scope_owner(&ScopeArgs {
            home: Some(home.path().to_owned()),
            graphql: None,
            node_did: None,
        })
        .await
        .unwrap();
        let config = engineer_config(&access, &owner, home.path(), &engineer)
            .await
            .unwrap();

        // The pack's agent runs host tools under the Engineer's tool root:
        // its Tools took that root at install, and reading as itself it finds
        // the published root that covers it, which is what the runtime's
        // execution boundary asks for.
        let engineer_root = initialized["tool_root"].as_str().unwrap();
        let granted = &report["installed"][0]["host_tool_roots"];
        assert_eq!(granted.as_array().map(Vec::len), Some(1), "{report}");
        let worker = granted[0]["agent"].as_str().unwrap().to_owned();
        let node = access.local_node().unwrap().clone();
        let stored = crate::home_state::read_init_config(home.path())
            .unwrap()
            .unwrap();
        let runtime =
            crate::home_state::load_initialized_home_identity(home.path(), &stored).unwrap();
        let binding =
            crate::commands::inference_binding::load_bound_agent_binding(&node, &owner, &worker)
                .await
                .unwrap();
        let worker_identity =
            gents::store::identity::resolve_agent_identity(runtime.as_ref(), &binding).unwrap();
        let as_worker = gents::ConfigAccess::local_managed_agent(
            node,
            identity::Did::new(&worker).unwrap(),
            worker_identity.as_ref(),
        )
        .unwrap();
        let reader = gents::tools::surface::agent_tool_root_reader(&as_worker, &owner, &worker)
            .await
            .unwrap();
        let gents::tools::surface::ToolRootReader::Published(published) = reader else {
            panic!("the pack's agent reads no published root covering its tool root: {reader:?}")
        };
        assert_eq!(granted[0]["reader_on"], published.as_str(), "{report}");
        assert!(
            std::path::Path::new(engineer_root).starts_with(&published)
                || std::fs::canonicalize(engineer_root)
                    .unwrap()
                    .starts_with(&published),
            "{engineer_root} is not under {published}"
        );
        drop(as_worker);

        let call = |call: Value| {
            let config = &config;
            async move {
                let answer = config.call(call.to_string()).await;
                let answer = answer.unwrap_or_else(|error| panic!("{call}: {error}"));
                serde_json::from_str::<Value>(&answer).unwrap()
            }
        };
        let agents = call(json!({"argv": ["agent", "list"]})).await;
        let worker = agents["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|agent| agent["display_name"] == "Crew worker")
            .unwrap_or_else(|| panic!("the Engineer lists the pack's agent: {agents}"))
            ["agent_did"]
            .clone();
        call(
            json!({"argv": ["agent-target", "create"], "target_id": "crew-helper",
            "set": {"agent_did": worker, "name": "crew-helper"}}),
        )
        .await;
        call(json!({"argv": ["tools", "update"],
            "set": {"agents": {"enabled": true, "target_ids": ["engineer", "crew-helper", "crew-worker"]}}}))
        .await;
        let tools = call(json!({"argv": ["tools", "get"]})).await;
        assert_eq!(
            tools["document"]["agents"]["target_ids"],
            json!(["engineer", "crew-helper", "crew-worker"]),
            "{tools}"
        );
        let refused = |call: Value| {
            let config = &config;
            async move { config.call(call.to_string()).await.unwrap_err().to_string() }
        };
        // The Engineer wires and unwires a pack itself, through the owner the
        // operator's command uses; a preview writes nothing.
        let unwired = call(json!({"argv": ["pack", "wire", "fixture/crew", "--remove"]})).await;
        assert_eq!(unwired["committed"], true, "{unwired}");
        assert_eq!(
            unwired["wired"][0]["changed"][0], "removed plugin fixture/probe",
            "{unwired}"
        );
        let preview = call(json!({"argv": ["pack", "preview", "wire", "fixture/crew"]})).await;
        assert_eq!(preview["committed"], false, "{preview}");
        assert_eq!(
            preview["apply_with"]["argv"],
            json!(["pack", "wire", "fixture/crew"]),
            "{preview}"
        );
        let unselected = call(json!({"argv": ["tools", "get"]})).await;
        assert_eq!(
            unselected["document"]["integrations"]["plugins"]
                .as_array()
                .map_or(0, Vec::len),
            0,
            "{unselected}"
        );
        let rewired = call(preview["apply_with"].clone()).await;
        assert!(
            rewired["wired"][0]["changed"][0]
                .as_str()
                .unwrap()
                .starts_with("added plugin fixture/probe at sha256:"),
            "{rewired}"
        );
        let selected = call(json!({"argv": ["tools", "get"]})).await;
        assert_eq!(
            selected["document"]["integrations"]["plugins"][0]["plugin"], "fixture/probe",
            "{selected}"
        );

        let still_wired = refused(json!({"argv": ["pack", "remove", "fixture/crew"]})).await;
        assert!(
            still_wired.contains(
                "Deletion would leave a broken reference to AgentTarget \\\"crew-worker\\\""
            ),
            "{still_wired}"
        );
        drop(config);
        if let Some(node) = access.local_node() {
            node.shutdown().await.expect("node shutdown");
        }
    }

    /// Only a terminal is asked, and only when the caller may prompt; the
    /// explicit flag approves without one.
    #[test]
    fn approval_needs_a_terminal_or_the_flag() {
        assert_eq!(Approval::resolve(true, false), Approval::Approved);
        assert_eq!(Approval::resolve(false, false), Approval::Unattended);
    }
}
