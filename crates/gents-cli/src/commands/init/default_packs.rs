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
    "gents/script",
];

/// What a default pack is for and what the Engineer lacks without it, as the
/// init notice, its questions and the desktop's approval card say it.
fn describe(pack: &str) -> (&'static str, &'static str) {
    match pack {
        "gents/pack_builder" => (
            "skills for building and sharing packs",
            "Without it the Engineer has no guide for packaging what it builds.",
        ),
        "gents/web" => (
            "searching and reading the web",
            "Without it the Engineer cannot search or read the web.",
        ),
        "gents/dynamic_workflows" => (
            "workflows that put several agents on one job",
            "Without it the Engineer cannot run workflows.",
        ),
        "gents/charts" => (
            "charts on the pages it builds",
            "Without it the Engineer's pages have no charts.",
        ),
        "gents/data_tables" => (
            "data tables on the pages it builds",
            "Without it the Engineer's pages have no data tables.",
        ),
        "gents/script" => (
            "short scripts over its own tools: waiting, trying logic, bulk changes",
            "Without it the Engineer makes every tool call one at a time.",
        ),
        _ => ("", ""),
    }
}

/// The default packs whose approval is also the consent to dynamic workflow
/// scripts: approving one sets its job block to `dynamic: run` as it is wired
/// (Lean `consentedDynamic`), since a pack can only ship `check`, which
/// refuses every spawn a script asks for.
const DYNAMIC_RUN_ON_APPROVAL: &[&str] = &["gents/dynamic_workflows"];

/// The default packs whose approval is also the consent to their plugins
/// calling the Engineer's own tools: approving one wires its plugins with
/// `tool_calls: true`. Without it a script only computes and sleeps.
const TOOL_CALLS_ON_APPROVAL: &[&str] = &["gents/script"];

/// What approving a pack that ships agents with host tools grants them: the
/// User's reader grant on the Engineer's own tool root, which their Tools
/// take at install.
const READS_ENGINEER_ROOT: &str =
    "Lets the agents it ships use their file and shell tools in the Engineer's folder.";

/// What approving `pack` turns on beyond the permissions its plugins request,
/// in plain words. `reads_root` is whether it ships Tools that run under the
/// Engineer's tool root.
fn turns_on(pack: &str, reads_root: bool) -> Vec<&'static str> {
    let mut turned_on = Vec::new();
    if reads_root {
        turned_on.push(READS_ENGINEER_ROOT);
    }
    if DYNAMIC_RUN_ON_APPROVAL.contains(&pack) {
        turned_on.push("Lets workflow scripts the model writes start agents on this node.");
    }
    if TOOL_CALLS_ON_APPROVAL.contains(&pack) {
        turned_on.push(
            "Lets a script it runs call the Engineer's own tools. Declined, a script only computes and waits.",
        );
    }
    turned_on
}

/// How long one default pack may take to resolve. Init succeeds without its
/// default packs, so an unreachable registry must not hold a first run for
/// the operating system's connection timeout once per pack.
const RESOLVE_BOUND: Duration = Duration::from_secs(20);

/// Who answers when a default pack's plugins request permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Approval {
    /// Ask on the terminal, pack by pack.
    Ask,
    /// The caller approved every request: `--approve-default-packs`, a pack
    /// named to `gents config pack starter --approve`, or the desktop's card.
    Approved,
    /// Nobody can be asked and nothing was approved: a pack that requests
    /// permissions, or whose agents would be granted the Engineer's folder,
    /// is left uninstalled and reported.
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

fn approve_command(home: &Path, pack: &str) -> String {
    format!(
        "gents config pack starter --approve {pack} --home {}",
        home.display()
    )
}

fn scope(home: &Path) -> ScopeArgs {
    ScopeArgs {
        home: Some(home.to_owned()),
        graphql: None,
        node_did: None,
    }
}

async fn release(access: gents::ConfigAccess) {
    if let Some(node) = access.local_node() {
        if let Err(error) = node.shutdown().await {
            tracing::warn!(error = %gents_protocol::error_chain::ErrorChain(&error), "failed to shut down embedded node");
        }
    }
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
        "\nThe Engineer starts with these packs, from this home's pack store or the pack registry (which needs the network):"
    );
    for pack in packs {
        match describe(pack).0 {
            "" => eprintln!("  {pack}"),
            purpose => eprintln!("  {pack}: {purpose}"),
        }
    }
    eprintln!(
        "{}",
        match approval {
            Approval::Ask => "Before a pack whose plugins request permissions is installed, you are asked to approve them.",
            Approval::Approved => "The permissions their plugins request were approved with --approve-default-packs.",
            Approval::Unattended => "A pack whose plugins request permissions is not installed without your approval; each is listed with the command that approves and installs it.",
        }
    );
    let mut installed = Vec::new();
    let mut not_installed = Vec::new();
    for pack in packs {
        match install_one(home, engineer, pack, registry, approval, None).await {
            Ok(Outcome::Installed(report)) => {
                eprintln!("  installed {pack}");
                installed.push(report);
            }
            Ok(Outcome::Unapproved {
                requested,
                turns_on,
            }) => {
                let command = approve_command(home, pack);
                let reason = if requested.as_array().is_some_and(|asks| !asks.is_empty()) {
                    "its plugins request permissions that were not approved"
                } else {
                    "it gives its agents the Engineer's folder, which was not approved"
                };
                eprintln!(
                    "  {pack} was not installed: {reason}. Approve and install it with: {command}"
                );
                not_installed.push(json!({
                    "pack": pack,
                    "reason": reason,
                    "requested_permissions": requested,
                    "turns_on": turns_on,
                    "install_with": command,
                }));
            }
            Err(error) => {
                let command = approve_command(home, pack);
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

/// One default pack as `home` has it: what the desktop's approval card and
/// `gents config pack starter` list.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct PackStatus {
    pub(crate) pack: String,
    pub(crate) purpose: &'static str,
    /// What the Engineer lacks while the pack is not installed.
    pub(crate) without: &'static str,
    pub(crate) state: PackState,
    pub(crate) version: Option<String>,
    /// The archive an approval installs; the approval names it back.
    pub(crate) digest: Option<String>,
    /// `[{plugin, asks}]`: the permissions its plugins request.
    pub(crate) requested: Value,
    /// What approving turns on beyond those permissions.
    pub(crate) turns_on: Vec<&'static str>,
    /// Why an `unavailable` pack could not be resolved.
    pub(crate) reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PackState {
    /// Installed, with its plugins wired into the Engineer's Tools as an
    /// approval wires them and its skills attached to the Engineer's Context.
    Installed,
    /// Not installed as approved, and installing it grants something.
    NeedsApproval,
    /// Not installed, and installing it grants nothing.
    Ready,
    /// Neither the home's pack store nor the registry resolved it.
    Unavailable,
}

/// The Engineer of `home`'s node: its default agent, when that agent is the
/// Engineer. `None` on a home with another default agent, which has no
/// default packs.
async fn engineer_of(access: &gents::ConfigAccess, owner: &str) -> Result<Option<String>> {
    let Ok(agent) = crate::commands::start::default_agent(access, owner).await else {
        return Ok(None);
    };
    let rows = crate::graphql_rows(
        access,
        "Agent",
        &format!(
            r#"{{ Agent(filter: {{agent_did: {{_eq: "{}"}}}}, limit: 1) {{ tags }} }}"#,
            gents::store::graphql::escape_graphql_string(&agent)
        ),
    )
    .await?;
    let engineer = rows.first().is_some_and(|row| {
        row["tags"].as_array().is_some_and(|tags| {
            tags.iter().any(|tag| {
                tag.as_str() == Some(gents::tools::builtin::self_config::ENGINEER_AGENT_TAG)
            })
        })
    });
    Ok(engineer.then_some(agent))
}

/// Whether the installed `record` is in the Engineer's Tools and Context the
/// way an approval leaves it: every plugin selected, with the consents its
/// pack's approval carries, and every skill attached.
async fn installed_as_approved(
    access: &gents::ConfigAccess,
    owner: &str,
    pack: &str,
    record: &gents::pack::HomePackInstall,
    context: &gents::documents::AgentContext,
    tools: &gents::documents::Tools,
) -> Result<bool> {
    let (namespace, _) = gents::pack::registry::split_pack_coordinate(pack);
    let selected: Vec<_> = tools
        .integrations
        .iter()
        .flat_map(|integrations| integrations.plugins.iter().flatten())
        .collect();
    for shipped in &record.plugins {
        let coordinate = format!("{namespace}/{}", shipped.name);
        let Some(selection) = selected.iter().find(|plugin| plugin.plugin == coordinate) else {
            return Ok(false);
        };
        if TOOL_CALLS_ON_APPROVAL.contains(&pack) && !selection.tool_calls {
            return Ok(false);
        }
        if DYNAMIC_RUN_ON_APPROVAL.contains(&pack)
            && selection.job.as_ref().is_some_and(|job| {
                gents::tools::background::job::settings::consented_dynamic(true, job.policy.dynamic)
                    != job.policy.dynamic
            })
        {
            return Ok(false);
        }
    }
    Ok(gents::pack::installed_pack_documents(access, owner, pack)
        .await?
        .into_iter()
        .filter(|(collection, _)| *collection == gents::store::collection::Collection::Skill)
        .all(|(_, skill)| context.skill_ids.contains(&skill)))
}

/// Each of `packs` as `home` has it, read through whatever serves the home:
/// its own store, or the runtime running on it. A home whose default agent
/// is not the Engineer has none.
pub(crate) async fn status(
    home: &Path,
    packs: &[&str],
    registry: Option<&str>,
) -> Result<Vec<PackStatus>> {
    let (access, owner) = command::resolve_scope_owner(&scope(home)).await?;
    let read = async {
        let Some(engineer) = engineer_of(&access, &owner).await? else {
            return Ok(None);
        };
        let (context, tools) = access
            .transact_readonly("cli.default_packs.status", |txn| {
                let (owner, engineer) = (&owner, &engineer);
                Box::pin(async move {
                    gents::documents::load_agent_context_and_tools_in_txn(txn, owner, engineer)
                        .await
                })
            })
            .await?;
        let context = context.context("the Engineer has no Context document")?;
        let tools = tools.context("the Engineer has no Tools document")?;
        let mut installed = Vec::new();
        for pack in packs {
            installed.push(match gents::pack::read_home_install(home, pack)? {
                Some(record) => {
                    installed_as_approved(&access, &owner, pack, &record, &context, &tools)
                        .await?
                        .then_some(record)
                }
                None => None,
            });
        }
        Ok::<_, anyhow::Error>(Some(installed))
    }
    .await;
    release(access).await;
    let Some(installed) = read? else {
        return Ok(Vec::new());
    };
    let resolved = futures_util::future::join_all(packs.iter().zip(&installed).map(
        |(pack, installed)| async move {
            match installed {
                Some(_) => None,
                None => Some(resolve(home, pack, registry).await),
            }
        },
    ))
    .await;
    Ok(packs
        .iter()
        .zip(installed)
        .zip(resolved)
        .map(|((pack, installed), resolved)| {
            let (purpose, without) = describe(pack);
            let mut status = PackStatus {
                pack: (*pack).to_owned(),
                purpose,
                without,
                state: PackState::Installed,
                version: installed.as_ref().map(|record| record.version.clone()),
                digest: installed.as_ref().map(|record| record.digest.clone()),
                requested: json!([]),
                turns_on: turns_on(pack, false),
                reason: None,
            };
            match resolved {
                None => {}
                Some(Ok(resolved)) => {
                    status.turns_on = turns_on(pack, !resolved.host_tools.is_empty());
                    status.state = if resolved.asks() || !status.turns_on.is_empty() {
                        PackState::NeedsApproval
                    } else {
                        PackState::Ready
                    };
                    status.version = Some(resolved.source.manifest().version.clone());
                    status.digest = Some(resolved.source.digest().to_owned());
                    status.requested = resolved.requested;
                }
                Some(Err(error)) => {
                    status.state = PackState::Unavailable;
                    status.reason = Some(format!("{error:#}"));
                }
            }
            status
        })
        .collect())
}

/// Installs each approved pack of `approved` (a name of `packs`, with the
/// digest its status showed when one is given) for `home`'s Engineer, as
/// init installs a pack it was told to approve. The home's own store is
/// opened, so a runtime serving the home is refused before anything is
/// written. Returns `{installed, failed}`; a pack that fails leaves the
/// others installed and is retried by approving it again.
pub(crate) async fn approve(
    home: &Path,
    packs: &[&str],
    approved: &[(String, Option<String>)],
    registry: Option<&str>,
) -> Result<Value> {
    for (pack, _) in approved {
        anyhow::ensure!(
            packs.contains(&pack.as_str()),
            "{pack} is not one of the Engineer's starter packs ({}). Install any other pack with: gents config pack install {pack} --home {}",
            packs.join(", "),
            home.display()
        );
    }
    let (access, owner) = command::resolve_scope_owner(&scope(home)).await?;
    let engineer = match access.local_node() {
        Some(_) => engineer_of(&access, &owner).await,
        None => Err(anyhow::anyhow!(
            "a runtime is serving this home, and a starter pack is installed through the home's own store. Nothing was installed. Stop the runtime (gents service stop --home {}), approve again, then start it",
            home.display()
        )),
    };
    release(access).await;
    let engineer = engineer?
        .context("this home's default agent is not the Engineer, so it has no starter packs")?;
    let mut installed = Vec::new();
    let mut failed = Vec::new();
    for (pack, digest) in approved {
        match install_one(
            home,
            &engineer,
            pack,
            registry,
            Approval::Approved,
            digest.as_deref(),
        )
        .await
        {
            Ok(Outcome::Installed(report)) => installed.push(report),
            Ok(Outcome::Unapproved { .. }) => unreachable!("an approved pack is never unapproved"),
            Err(error) => failed.push(json!({"pack": pack, "error": format!("{error:#}")})),
        }
    }
    Ok(json!({"installed": installed, "failed": failed}))
}

enum Outcome {
    Installed(Value),
    /// Not installed: it grants something that was not approved.
    Unapproved {
        requested: Value,
        turns_on: Vec<&'static str>,
    },
}

/// A default pack's archive with its dependencies and what its plugins ask.
struct Resolved {
    source: command::PackSource,
    requested: Value,
    /// The archives of the packs it depends on.
    dependencies: Vec<command::PackSource>,
    /// The Tools it and its dependencies ship that run under the Engineer's
    /// tool root.
    host_tools: Vec<String>,
}

impl Resolved {
    fn asks(&self) -> bool {
        self.requested
            .as_array()
            .is_some_and(|asks| !asks.is_empty())
    }
}

async fn resolve(home: &Path, pack: &str, registry: Option<&str>) -> Result<Resolved> {
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
    let host_tools = std::iter::once(&source)
        .chain(&dependencies)
        .flat_map(command::wire::tools_taking_installer_root)
        .collect();
    Ok(Resolved {
        source,
        requested,
        dependencies,
        host_tools,
    })
}

#[cfg(test)]
static FAIL_NEXT_WIRE: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);

async fn install_one(
    home: &Path,
    engineer: &str,
    pack: &str,
    registry: Option<&str>,
    approval: Approval,
    expect_digest: Option<&str>,
) -> Result<Outcome> {
    let Resolved {
        source,
        requested,
        dependencies,
        host_tools,
    } = resolve(home, pack, registry).await?;
    command::check_expected_digest(expect_digest, source.digest())?;
    let wire = !source.manifest().metadata.plugins.is_empty();
    let asks = requested.as_array().is_some_and(|asks| !asks.is_empty());
    let tool_calls = TOOL_CALLS_ON_APPROVAL.contains(&pack);
    let dynamic = DYNAMIC_RUN_ON_APPROVAL.contains(&pack);
    let reads_root = !host_tools.is_empty();
    let approved = (asks || tool_calls || dynamic || reads_root)
        && match approval {
            Approval::Approved => true,
            Approval::Unattended => false,
            Approval::Ask => {
                if asks {
                    eprintln!("\n{pack} requests these permissions for its plugins:");
                } else {
                    eprintln!("\n{pack}:");
                }
                for ask in requested.as_array().into_iter().flatten() {
                    eprintln!(
                        "  {}: {}",
                        ask["plugin"].as_str().unwrap_or_default(),
                        ask["asks"].as_str().unwrap_or_default()
                    );
                }
                for turned_on in turns_on(pack, reads_root) {
                    eprintln!("  {turned_on}");
                }
                match describe(pack).1 {
                    "" => {}
                    without => eprintln!("  {without}"),
                }
                crate::interactive_backend::confirm(
                    &if asks || reads_root {
                        format!("Install {pack} with these permissions?")
                    } else {
                        format!("Give {pack} this permission?")
                    },
                    true,
                )
                .await
            }
        };
    let grant = asks && approved;
    if (asks || reads_root) && !approved {
        return Ok(Outcome::Unapproved {
            requested,
            turns_on: turns_on(pack, reads_root),
        });
    }
    let coordinate = format!(
        "{}/{}",
        source.manifest().metadata.namespace,
        source.manifest().name
    );
    // One held access installs the pack, wires its plugins and attaches its
    // skills, and its node is drained before the next pack opens the home.
    let (access, owner) = command::resolve_scope_owner(&scope(home)).await?;
    let already = gents::pack::read_home_install(home, &coordinate)?
        .is_some_and(|record| record.digest == source.digest());
    // The pack and the dependencies this install installs with it: the
    // agents an approval grants the Engineer's root.
    let installing = async {
        let mut installing = vec![coordinate.clone()];
        for dependency in command::wire::new_dependencies(&access, &owner, &dependencies).await? {
            installing.push(format!(
                "{}/{}",
                dependency.manifest().metadata.namespace,
                dependency.manifest().name
            ));
        }
        anyhow::Ok(installing)
    }
    .await;
    let installed = if already {
        Ok(())
    } else {
        install_as_engineer(
            &access,
            &owner,
            home,
            engineer,
            &format!("{coordinate}@{}", source.manifest().version),
            source.digest(),
        )
        .await
        .map(drop)
    };
    let attached = async {
        let installing = installing?;
        installed?;
        // The Engineer installed the pack, so its rootless Tools took the
        // Engineer's root: the one root this approval grants reader on.
        let host_tool_roots = async {
            let supplied =
                gents::pack::installer_tool_root(&access, &owner, Some(engineer)).await?;
            command::wire::grant_pack_root_readers(
                &access,
                &owner,
                &installing,
                supplied.as_deref(),
                approved,
            )
            .await
        }
        .await
        .with_context(|| {
            format!("{pack} is installed, but its agents were not granted their tool root")
        })?;
        #[cfg(test)]
        if FAIL_NEXT_WIRE
            .lock()
            .unwrap()
            .take_if(|failing| failing == home)
            .is_some()
        {
            anyhow::bail!("simulated wire failure for {pack}");
        }
        let wired = if wire {
            command::wire_pack_plugins_consenting(
                &access,
                &owner,
                home,
                &source,
                &[engineer.to_owned()],
                approved && dynamic,
                approved && tool_calls,
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
    release(access).await;
    let (wired, skills, host_tool_roots) = attached?;
    if tool_calls && !approved {
        eprintln!("  {pack} is wired without the Engineer's tools, which was not approved: a script only computes and waits. Approve it with: {}", approve_command(home, pack));
    }
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

/// `gents config pack starter`: the Engineer's starter packs as this home
/// has them, or with `--approve` the named ones installed with what they
/// request approved.
pub(crate) async fn starter(args: crate::cli::PackStarterArgs) -> Result<()> {
    let home = crate::home_state::resolve_home_dir(args.home.as_deref());
    let registry = args.registry.as_deref();
    if args.approve.is_empty() {
        return crate::print_json(&json!({
            "packs": status(&home, ENGINEER_DEFAULT_PACKS, registry).await?,
        }));
    }
    let approved: Vec<_> = args
        .approve
        .iter()
        .map(|pack| (pack.clone(), None))
        .collect();
    let report = approve(&home, ENGINEER_DEFAULT_PACKS, &approved, registry).await?;
    crate::print_json(&report)?;
    anyhow::ensure!(
        report["failed"].as_array().is_none_or(Vec::is_empty),
        "a starter pack was not installed; its error is in the report above"
    );
    Ok(())
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

/// The Agent document's identity binding, read as the node that owns it.
async fn agent_identity_binding(
    access: &gents::ConfigAccess,
    owner: &str,
    agent_did: &str,
) -> Result<gents_protocol::agent_identity::AgentIdentityBinding> {
    access
        .transact_readonly("cli.init.agent_binding", |txn| {
            Box::pin(async move {
                let (_, value) = gents::store::config_client::read_desired_state_record_in_txn(
                    txn,
                    gents::Collection::Agent,
                    owner,
                    agent_did,
                )
                .await?
                .with_context(|| format!("agent {agent_did:?} missing for {owner:?}"))?;
                let agent: gents::documents::Agent = serde_json::from_value(value)?;
                Ok(agent.identity_binding())
            })
        })
        .await
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
    let binding = agent_identity_binding(access, owner, engineer).await?;
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
            later.starts_with("gents config pack starter --approve fixture/engineer_tools --home "),
            "{later}"
        );
        assert_eq!(pending[1]["pack"], "fixture/unpublished", "{report}");
        assert!(
            pending[1]["install_with"]
                .as_str()
                .unwrap()
                .starts_with("gents config pack starter --approve fixture/unpublished --home "),
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

        // What the desktop's card and `pack starter` list: the installed
        // pack, the one waiting on an approval with what it requests and the
        // archive an approval installs, and the one nothing resolves.
        let states =
            |listed: &[PackStatus]| listed.iter().map(|pack| pack.state).collect::<Vec<_>>();
        let listed = status(home.path(), &names, unreachable).await.unwrap();
        assert_eq!(
            states(&listed),
            [
                PackState::Installed,
                PackState::NeedsApproval,
                PackState::Unavailable
            ],
            "{listed:?}"
        );
        assert_eq!(listed[1].requested[0]["plugin"], "fetch", "{listed:?}");
        assert!(listed[2].reason.is_some(), "{listed:?}");
        let digest = listed[1].digest.clone().unwrap();
        let tools_pack =
            |digest: &str| vec![("fixture/engineer_tools".to_owned(), Some(digest.to_owned()))];

        // An approval installs only a listed pack, and only the archive that
        // was shown.
        let unlisted = approve(
            home.path(),
            &names,
            &[("fixture/other".into(), None)],
            unreachable,
        )
        .await
        .unwrap_err();
        assert!(
            format!("{unlisted:#}").contains("is not one of the Engineer's starter packs"),
            "{unlisted:#}"
        );
        let other = approve(
            home.path(),
            &names,
            &tools_pack("sha256:other"),
            unreachable,
        )
        .await
        .unwrap();
        assert!(
            other["failed"][0]["error"]
                .as_str()
                .unwrap()
                .contains("not the expected sha256:other"),
            "{other}"
        );
        assert!(gents::plugin::store::read_record(home.path(), "fixture", "fetch").is_err());

        // An install whose wiring fails leaves the pack waiting, and
        // approving it again finishes it.
        *FAIL_NEXT_WIRE.lock().unwrap() = Some(home.path().to_owned());
        let half = approve(home.path(), &names, &tools_pack(&digest), unreachable)
            .await
            .unwrap();
        assert!(
            half["failed"][0]["error"]
                .as_str()
                .unwrap()
                .contains("simulated wire failure"),
            "{half}"
        );
        assert!(
            gents::pack::read_home_install(home.path(), "fixture/engineer_tools")
                .unwrap()
                .is_some()
        );
        let listed = status(home.path(), &names, unreachable).await.unwrap();
        assert_eq!(listed[1].state, PackState::NeedsApproval, "{listed:?}");
        assert_eq!(
            listed[1].digest.as_deref(),
            Some(digest.as_str()),
            "{listed:?}"
        );

        for _ in 0..2 {
            let report = approve(home.path(), &names, &tools_pack(&digest), unreachable)
                .await
                .unwrap();
            assert_eq!(report["failed"], json!([]), "{report}");
            assert_eq!(
                report["installed"][0]["granted_permissions"][0]["plugin"], "fetch",
                "{report}"
            );
        }
        let record = gents::plugin::store::read_record(home.path(), "fixture", "fetch").unwrap();
        assert!(
            record.granted.is_some(),
            "the approval is the recorded grant"
        );
        let documents = engineer_documents(home.path()).await;
        assert_eq!(
            documents["plugins"].as_array().map(Vec::len),
            Some(1),
            "approving again selects the plugin once: {documents}"
        );
        assert_eq!(
            documents["plugins"][0]["plugin"], "fixture/fetch",
            "{documents}"
        );
        let listed = status(home.path(), &names, unreachable).await.unwrap();
        assert_eq!(
            states(&listed),
            [
                PackState::Installed,
                PackState::Installed,
                PackState::Unavailable
            ],
            "{listed:?}"
        );
    }

    /// `init --engineer` installs its default packs itself, after it has let
    /// go of the home: each pack opens the home again, which a node init
    /// still held would lock.
    #[tokio::test]
    async fn init_installs_the_default_packs_its_home_store_holds() {
        let home = tempfile::tempdir().unwrap();
        let packs = tempfile::tempdir().unwrap();
        for pack in ENGINEER_DEFAULT_PACKS {
            let (namespace, name) = gents::pack::registry::split_pack_coordinate(pack);
            let dir = packs.path().join(name);
            write_pack(
                &dir,
                json!({
                    "manifest_version": 1, "name": name, "namespace": namespace,
                    "version": "1.0.0", "description": "A starter skill",
                    "authors": ["tests"], "tags": [], "kind": "documents",
                    "assets": ["README.md", "pack_config.json"], "dependencies": [],
                    "config": "pack_config.json",
                }),
                &[(
                    "pack_config.json",
                    serde_json::to_vec(&json!({
                        "node": {"node_did": "${GENTS_PACK_NODE_DID}"},
                        "skills": [{"skill_id": format!("starter-{name}"),
                            "node_did": "${GENTS_PACK_NODE_DID}", "name": name,
                            "description": "A starter skill.", "instructions": "Use it."}],
                    }))
                    .unwrap(),
                )],
            );
            gents::pack::local::open(&gents::pack::local::LocalName::Path(&dir), home.path())
                .unwrap();
        }
        let cli = crate::cli::Cli::try_parse_from([
            "gents",
            "init",
            "--store-key-custody",
            "file",
            "--engineer",
            "--inference-url",
            "http://127.0.0.1:9/v1",
            "--home",
            home.path().to_str().unwrap(),
        ])
        .unwrap();
        let Some(crate::cli::Command::Init(mut args)) = cli.command else {
            panic!("expected init")
        };
        args.unattended = true;
        let initialized = crate::commands::init::init_home(args).await.unwrap();
        let report = &initialized["default_packs"];
        assert_eq!(report["not_installed"], json!([]), "{report}");
        assert_eq!(
            report["installed"]
                .as_array()
                .unwrap()
                .iter()
                .map(|pack| pack["pack"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ENGINEER_DEFAULT_PACKS,
            "{report}"
        );
        let documents = engineer_documents(home.path()).await;
        assert_eq!(
            documents["skills"].as_array().map(Vec::len),
            Some(ENGINEER_DEFAULT_PACKS.len()),
            "{documents}"
        );
        for pack in ENGINEER_DEFAULT_PACKS {
            let (purpose, without) = describe(pack);
            assert!(!purpose.is_empty() && !without.is_empty(), "{pack}");
        }
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
        let binding = agent_identity_binding(&access, &owner, &worker)
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
            still_wired.contains("fixture/crew was not removed")
                && still_wired.contains("references missing AgentTarget")
                && still_wired.contains("crew-worker"),
            "{still_wired}"
        );

        // The Engineer removes the pack it was given. What it made around
        // the pack is its own to delete first: a target of its own that names
        // the pack's agent would be left dangling. A plugin its Tools still
        // select refuses the retirement and names the unwiring call; unwired,
        // the pack goes with its agent, its target and its plugin.
        call(
            json!({"argv": ["tools", "update"], "options": {"allow-drop": "agents"},
            "set": {"agents": {"enabled": true, "target_ids": ["engineer"]}}}),
        )
        .await;
        let delete =
            call(json!({"argv": ["agent-target", "preview", "delete", "crew-helper"]})).await;
        call(delete["apply_with"].clone()).await;
        let selected = refused(json!({"argv": ["pack", "remove", "fixture/crew"]})).await;
        assert!(
            selected.contains("fixture/crew was not removed (fixture/probe): selected by Tools")
                && selected.contains(r#""argv":["pack","wire","fixture/crew","--remove"]"#),
            "{selected}"
        );
        call(json!({"argv": ["pack", "wire", "fixture/crew", "--remove"]})).await;
        let removed = call(json!({"argv": ["pack", "remove", "fixture/crew"]})).await;
        let agents = call(json!({"argv": ["agent", "list"]})).await;
        assert!(
            !agents.to_string().contains("Crew worker"),
            "{removed}\n{agents}"
        );
        let targets = call(json!({"argv": ["agent-target", "list"]})).await;
        assert_eq!(
            targets["items"].as_array().map(Vec::len),
            Some(1),
            "only the Engineer's own target is left: {targets}"
        );
        assert!(
            gents::plugin::store::read_record(home.path(), "fixture", "probe").is_err(),
            "the retired plugin has no record: {removed}"
        );
        assert_eq!(
            gents::pack::read_home_install(home.path(), "fixture/crew").unwrap(),
            None,
            "{removed}"
        );
        drop(config);
        if let Some(node) = access.local_node() {
            node.shutdown().await.expect("node shutdown");
        }
    }

    /// A pack whose agents run host tools takes the Engineer's tool root,
    /// and the User's reader grant on it is part of the approval: unattended
    /// the pack is left out and nothing is granted; approved it installs with
    /// the grant listed.
    #[tokio::test]
    async fn a_pack_reads_the_engineers_folder_only_when_approved() {
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
        let packs = tempfile::tempdir().unwrap();
        let hands = packs.path().join("hands");
        write_pack(
            &hands,
            json!({
                "manifest_version": 1, "name": "hands", "namespace": "fixture",
                "version": "1.0.0", "description": "An agent with host tools",
                "authors": ["tests"], "tags": [], "kind": "documents",
                "assets": ["README.md", "pack_config.json"],
                "dependencies": [], "config": "pack_config.json",
                "inference_slots": [{"name": "worker", "description": "The worker.",
                    "agents": ["hands-worker"]}],
            }),
            &[(
                "pack_config.json",
                serde_json::to_vec(&json!({
                    "node": {},
                    "agents": [{"agent_slot": "hands-worker", "context_id": "hands-context",
                        "display_name": "Hands worker", "description": "Does one task.",
                        "inference_profile_id": "gents:inference-slot:worker"}],
                    "contexts": [{"context_id": "hands-context", "system_prompt": "Work.",
                        "tools_id": "hands-tools"}],
                    "tools": [{"tools_id": "hands-tools", "display_name": "Read-only files",
                        "host": {"files": {"mode": "ReadOnly"}}}],
                }))
                .unwrap(),
            )],
        );
        gents::pack::local::open(&gents::pack::local::LocalName::Path(&hands), home.path())
            .unwrap();
        let names = ["fixture/hands"];
        let unreachable = Some("http://127.0.0.1:9");

        let report = install(
            home.path(),
            &engineer,
            &names,
            unreachable,
            Approval::Unattended,
        )
        .await;
        assert_eq!(report["installed"], json!([]), "{report}");
        assert_eq!(
            report["not_installed"][0]["turns_on"],
            json!([READS_ENGINEER_ROOT]),
            "{report}"
        );
        assert_eq!(
            report["not_installed"][0]["requested_permissions"],
            json!([]),
            "{report}"
        );
        assert_eq!(
            gents::pack::read_home_install(home.path(), "fixture/hands").unwrap(),
            None,
            "nothing is installed or granted without approval"
        );
        let listed = status(home.path(), &names, unreachable).await.unwrap();
        assert_eq!(listed[0].state, PackState::NeedsApproval, "{listed:?}");
        assert_eq!(listed[0].turns_on, [READS_ENGINEER_ROOT], "{listed:?}");

        let report = install(
            home.path(),
            &engineer,
            &names,
            unreachable,
            Approval::Approved,
        )
        .await;
        assert_eq!(report["not_installed"], json!([]), "{report}");
        let granted = &report["installed"][0]["host_tool_roots"];
        assert_eq!(granted.as_array().map(Vec::len), Some(1), "{report}");
        assert_eq!(
            granted[0]["root"], initialized["tool_root"],
            "the grant is for the Engineer's own root: {report}"
        );
        assert!(granted[0]["reader_on"].is_string(), "{report}");
    }

    /// `gents/script` is a default pack, and approving it is what lets its
    /// scripts call the Engineer's tools: unattended it installs (its plugin
    /// requests no permissions) wired without `tool_calls`, and approved it
    /// is wired with `tool_calls: true`.
    #[tokio::test]
    async fn the_script_pack_calls_the_engineers_tools_only_when_approved() {
        assert_eq!(ENGINEER_DEFAULT_PACKS.len(), 6);
        assert!(TOOL_CALLS_ON_APPROVAL
            .iter()
            .all(|pack| ENGINEER_DEFAULT_PACKS.contains(pack)));
        let afb = tokio::task::spawn_blocking(|| {
            crate::commands::plugin::testing::build_plugin_afb(
                "script",
                b"fn main() { println!(\"{{}}\"); }",
            )
        })
        .await
        .unwrap();
        for (approval, expected) in [(Approval::Unattended, false), (Approval::Approved, true)] {
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
            let engineer = initialized["default_agent_did"]
                .as_str()
                .unwrap()
                .to_owned();
            let packs = tempfile::tempdir().unwrap();
            let script = packs.path().join("script");
            write_pack(
                &script,
                json!({
                    "manifest_version": 1, "name": "script", "namespace": "gents",
                    "version": "1.0.0", "description": "A script over the caller's tools",
                    "authors": ["tests"], "tags": [], "kind": "plugins",
                    "assets": ["README.md", "plugins/script.afb"],
                    "plugins": [{
                        "name": "script", "description": "test", "artifact": "plugins/script.afb",
                        "language": "rust", "input_schema": {"type": "object"},
                        "manifold": {"fs": "None", "net": "None", "crypto": false,
                            "child_process": false, "env": "None",
                            "allow_exit": false, "http_timeout_ms": null, "listen": "None"},
                    }],
                }),
                &[("plugins/script.afb", afb.clone())],
            );
            gents::pack::local::open(&gents::pack::local::LocalName::Path(&script), home.path())
                .unwrap();
            let report = install(
                home.path(),
                &engineer,
                &["gents/script"],
                Some("http://127.0.0.1:9"),
                approval,
            )
            .await;
            assert_eq!(report["not_installed"], json!([]), "{report}");
            assert_eq!(
                report["installed"][0]["granted_permissions"],
                json!([]),
                "{report}"
            );
            let documents = engineer_documents(home.path()).await;
            assert_eq!(
                documents["plugins"][0]["plugin"], "gents/script",
                "{documents}"
            );
            assert_eq!(
                documents["plugins"][0]["tool_calls"]
                    .as_bool()
                    .unwrap_or(false),
                expected,
                "{approval:?}: {documents}"
            );
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
