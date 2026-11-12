use std::path::{Path, PathBuf};

use chrono::Utc;
use gents_desktop_core::client::{
    delete_event_source_on, delete_schedule_on, delete_skill_on, delete_tool_service_registry_on,
    delete_trigger_on, patch_config_components_on, set_default_agent_on, upsert_event_source_on,
    upsert_schedule_on, upsert_skill_on, upsert_trigger_on,
};
use gents_desktop_core::inference::{load_inference_setup_state, AgentView, InferenceSetupState};
use gents_documents::mailbox_write_decl::MailboxKind;
use gents_documents::patch::SelfConfigTarget;
use gents_documents::InferenceProfile;
use gents_pack::pack::BindAccess;
use gents_pack::plugin::store::{list_records, InstalledPlugin};
use gents_pack::plugin::{allowed, ux};
use gents_session::session::mailbox::MailboxItem;
use gents_shell::sample::{MenuId, Tone};
use gents_shell::source::{Doc, MenuAct, MenuRow, PackAct};
use gents_store::config_client::ConfigAccess;
use gents_toolcalls::background::job::promote::{list_promotions, PromotionRecord};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use super::forms;
use super::sections::{self, new_id, Sources};
use super::work::Work;
use crate::commands::accounts::{account_rows, AccountRow};
use crate::commands::config::crud::{
    load_one, query_collection_sorted, ConfigDocumentSpec, EVENT_SOURCE_SPEC, MCP_SPEC,
    SCHEDULE_SPEC, SKILL_SPEC, TOOLS_SPEC, TRIGGER_SPEC,
};
use crate::commands::config::task_run::{enqueue_task_run_on, TaskRunOutput};
use crate::commands::status::load_runtime_status_output;
use crate::commands::task::load_task_inventory;

/// Runs the scripts an agent writes on the fly rather than being a workflow itself.
const RUNNER: &str = "gents/workflow";

/// What a section, form or act the shell asks for answers until it is wired.
pub(super) const NOT_WIRED: &str = "not wired yet";

/// `menu`'s rows, read as the node through `work`, or why they could not be. The installed
/// workflows, allowed folders and UX plugins are read from `home` alone, so they are listed while
/// `work` is off.
pub(super) async fn read_menu(
    work: &Result<Work, String>,
    home: &Path,
    node_did: &str,
    menu: MenuId,
) -> Result<Vec<MenuRow>, String> {
    if matches!(
        menu,
        MenuId::Workflows | MenuId::AllowedFolders | MenuId::UxPlugins
    ) {
        let home = home.to_path_buf();
        let rows = blocking(move || read_home(&home, menu)).await;
        return rows.map_err(|error| format!("{error:#}"));
    }
    let work = work.as_ref().map_err(String::clone)?;
    let access = work.access();
    let rows = match menu {
        MenuId::Accounts => account_rows(&access, node_did, None, Utc::now())
            .await
            .map(|rows| accounts(&rows)),
        MenuId::Agents => load_inference_setup_state(&access, node_did)
            .await
            .map(|state| agents(&state.agents, &state.inference_profiles)),
        MenuId::Tools => query_collection_sorted(&access, TOOLS_SPEC, node_did)
            .await
            .map(|documents| tools(&documents)),
        MenuId::Skills => query_collection_sorted(&access, SKILL_SPEC, node_did)
            .await
            .map(|documents| sections::skills(&documents)),
        MenuId::Triggers => read_triggers(&access, node_did).await,
        MenuId::Schedules => query_collection_sorted(&access, SCHEDULE_SPEC, node_did)
            .await
            .map(|documents| sections::schedules(&documents)),
        MenuId::EventSources => query_collection_sorted(&access, EVENT_SOURCE_SPEC, node_did)
            .await
            .map(|documents| sections::event_sources(&documents)),
        MenuId::RemoteTools => query_collection_sorted(&access, MCP_SPEC, node_did)
            .await
            .map(|documents| sections::remote_tools(&documents)),
        MenuId::Tasks => load_task_inventory(&access, None)
            .await
            .map(|inventory| tasks(&inventory.tasks)),
        MenuId::Mailbox => {
            gents_session::operations::mailbox::list(&access, &Default::default())
                .await
                .map(|items| mailbox(&items))
                .map_err(crate::commands::mailbox::failure)
        }
        MenuId::Status => load_runtime_status_output(Some(home), &work.endpoint, node_did)
            .await
            .map(|output| status(&output)),
        // The home's menus are read above, and the shell never asks for its own menus' rows.
        MenuId::NodeSettings
        | MenuId::AppSettings
        | MenuId::Workflows
        | MenuId::AllowedFolders
        | MenuId::UxPlugins
        | MenuId::More
        | MenuId::Ask
        | MenuId::General
        | MenuId::KeySets
        | MenuId::Developer
        | MenuId::Experimental => Ok(Vec::new()),
        // Asked for by agent, which `read_profiles` knows.
        MenuId::Profiles => Ok(Vec::new()),
        // Read by `packs::read`, with the registry search the source holds.
        MenuId::Packs => Ok(Vec::new()),
        // Read by `usage::read`, which the source also runs for a refresh.
        MenuId::Usage => Ok(Vec::new()),
    };
    rows.map_err(|error| format!("{error:#}"))
}

/// The profiles `agent` can run on, read as the node through `work`.
pub(super) async fn read_profiles(
    work: &Result<Work, String>,
    node_did: &str,
    agent: &str,
) -> Result<Vec<MenuRow>, String> {
    let access = work.as_ref().map_err(String::clone)?.access();
    let state = load_inference_setup_state(&access, node_did).await;
    state
        .map(|state| profiles(&state, agent))
        .map_err(|error| format!("{error:#}"))
}

/// Runs `act` as the node through `work`. Both the `Ok` and the `Err` are its notice.
pub(super) async fn act(
    work: &Work,
    home: &Path,
    node_did: &str,
    act: &MenuAct,
) -> Result<String, String> {
    let access = work.access();
    match act {
        MenuAct::SetDefaultAgent { agent, name } => set_default_agent_on(&access, node_did, agent)
            .await
            .map(|()| format!("default agent: {name}"))
            .map_err(|error| format!("could not make {name} the default: {error:#}")),
        MenuAct::RunTask { task } => run_task(&access, home, task)
            .await
            .map(|_| format!("started {task}"))
            .map_err(|error| format!("could not run {task}: {error:#}")),
        MenuAct::SetEnabled {
            doc,
            id,
            label,
            enabled,
        } => {
            let state = if *enabled { "on" } else { "off" };
            let target = match doc {
                Doc::Task => SelfConfigTarget::Task,
                Doc::Skill => {
                    use gents_protocol::operations::document::WriteMode;
                    use gents_protocol::operations::skill::SkillEdit;
                    return gents::operations::skill::save(
                        &access,
                        &gents::operations::skill::SkillWrite {
                            owner: node_did,
                            skill_id: id,
                            mode: WriteMode::Update,
                            patch: &SkillEdit::enabled(*enabled).patch(),
                            node_binding: None,
                        },
                    )
                    .await
                    .map(|_| format!("{label} is {state}"))
                    .map_err(|error| format!("Couldn’t turn it {state}: {error}"));
                }
                Doc::Trigger => SelfConfigTarget::Trigger,
                Doc::Schedule | Doc::EventSource | Doc::RemoteTool => {
                    return Err(NOT_WIRED.to_string())
                }
            };
            let patch = vec![("enabled".to_string(), Some(json!(enabled)))];
            patch_config_components_on(&access, node_did, &[(target, id.clone(), patch)])
                .await
                .map(|()| format!("{label} is {state}"))
                .map_err(|error| format!("Couldn’t turn it {state}: {error:#}"))
        }
        MenuAct::Duplicate(doc, id) => duplicate(&access, node_did, *doc, id)
            .await
            .map_err(|error| format!("Couldn’t duplicate {id}: {error:#}")),
        MenuAct::Delete { doc, id, label } => {
            let deleted = match doc {
                Doc::Skill => delete_skill_on(&access, node_did, id).await,
                Doc::Trigger => delete_trigger_on(&access, node_did, id).await,
                Doc::Schedule => delete_schedule_on(&access, node_did, id).await,
                Doc::EventSource => delete_event_source_on(&access, node_did, id).await,
                Doc::RemoteTool => delete_tool_service_registry_on(&access, node_did, id).await,
                Doc::Task => return Err(NOT_WIRED.to_string()),
            };
            deleted
                .map(|_| format!("Deleted {label}"))
                .map_err(|error| format!("Couldn’t delete {label}: {error:#}"))
        }
        MenuAct::Dismiss { item } => {
            gents_session::operations::mailbox::dismiss(&access, item)
                .await
                .map(|_| "dismissed".to_string())
                .map_err(|error| {
                    format!(
                        "could not dismiss it: {:#}",
                        crate::commands::mailbox::failure(error)
                    )
                })
        }
        MenuAct::SetFolderAccess { path, access } => grant(home, path, access)
            .await
            .map_err(|error| format!("{error:#}")),
        MenuAct::RemoveFolder(path) => {
            let (home, folder) = (home.to_path_buf(), PathBuf::from(path));
            blocking(move || allowed::remove(&home, &folder))
                .await
                .map(|_| format!("{path} removed"))
                .map_err(|error| format!("{error:#}"))
        }
        // The desktop's agent editor says "Saved" and "Couldn’t save: {error}" for this change.
        MenuAct::SetAgentProfile { agent, profile, .. } => {
            let patch = vec![("inference_profile_id".to_string(), Some(json!(profile)))];
            patch_config_components_on(
                &access,
                node_did,
                &[(SelfConfigTarget::Agent, agent.clone(), patch)],
            )
            .await
            .map(|()| "Saved".to_string())
            .map_err(|error| format!("Couldn’t save: {error:#}"))
        }
        MenuAct::OpenSession { .. }
        | MenuAct::Insert { .. }
        | MenuAct::OpenSetup
        | MenuAct::Open(_)
        | MenuAct::ConfirmDelete { .. }
        | MenuAct::ChooseProfile { .. } => {
            unreachable!("the shell runs these acts itself")
        }
        MenuAct::Pack(
            PackAct::Page(_)
            | PackAct::Install { .. }
            | PackAct::Update { .. }
            | PackAct::Remove { .. }
            | PackAct::ChooseSlot { .. }
            | PackAct::BindSlot { .. }
            | PackAct::SignOut
            | PackAct::Info { .. }
            | PackAct::Keyword(_),
        ) => unreachable!("the source runs Packs' acts itself"),
        MenuAct::RefreshUsage => unreachable!("the source reads usage itself"),
    }
}

/// Grants `path` `access` (`read` or `read_write`) among `home`'s allowed folders, and answers
/// the notice.
pub(super) async fn grant(home: &Path, path: &str, access: &str) -> anyhow::Result<String> {
    let access: BindAccess = access.parse()?;
    let (home, path) = (home.to_path_buf(), PathBuf::from(path));
    let granted = blocking(move || allowed::add(&home, &path, access)).await?;
    let access = match granted.access {
        BindAccess::Read => "read only",
        BindAccess::ReadWrite => "read and write",
    };
    Ok(format!("{} {access}", granted.path.display()))
}

/// `work` off the runtime's threads: the home's files are locked and read synchronously.
pub(super) async fn blocking<T, F>(work: F) -> anyhow::Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(work).await?
}

/// Runs `task` now as this home's node, signed by its local signer.
pub(super) async fn run_task(
    access: &ConfigAccess,
    home: &Path,
    task: &str,
) -> anyhow::Result<TaskRunOutput> {
    enqueue_task_run_on(access, Some(home), task, json!({}), None, None).await
}

async fn read_triggers(access: &ConfigAccess, node_did: &str) -> anyhow::Result<Vec<MenuRow>> {
    let (triggers, sources) = trigger_sources(access, node_did).await?;
    Ok(sections::triggers(&triggers, &sources))
}

/// The node's triggers and the documents they refer to. The inventory's triggers carry what the
/// runtime observed of them, as the desktop's bridge reads them (`snapshot/runtime.rs:208-251`).
/// Its schedules and event sources are keyed by id across nodes, so they are read for the node.
pub(super) async fn trigger_sources(
    access: &ConfigAccess,
    node_did: &str,
) -> anyhow::Result<(Vec<Value>, Sources)> {
    let inventory = load_task_inventory(access, None).await?;
    let owned = |rows: Vec<Value>| -> Vec<Value> {
        rows.into_iter()
            .filter(|row| row["node_did"] == node_did)
            .collect()
    };
    let sources = Sources {
        schedules: query_collection_sorted(access, SCHEDULE_SPEC, node_did).await?,
        event_sources: query_collection_sorted(access, EVENT_SOURCE_SPEC, node_did).await?,
        tasks: owned(inventory.tasks),
        agents: load_inference_setup_state(access, node_did).await?.agents,
    };
    Ok((owned(inventory.triggers), sources))
}

/// Writes a copy of `doc`'s `id` and answers the desktop's toast (`RowMenu.tsx:61`).
async fn duplicate(
    access: &ConfigAccess,
    node_did: &str,
    doc: Doc,
    id: &str,
) -> anyhow::Result<String> {
    let title = match doc {
        Doc::Skill => {
            let (title, copy) = copy_of(access, node_did, doc, id, SKILL_SPEC).await?;
            upsert_skill_on(access, &copy).await?;
            title
        }
        Doc::Trigger => {
            let (title, copy) = copy_of(access, node_did, doc, id, TRIGGER_SPEC).await?;
            upsert_trigger_on(access, &copy).await?;
            title
        }
        Doc::Schedule => {
            let (title, copy) = copy_of(access, node_did, doc, id, SCHEDULE_SPEC).await?;
            upsert_schedule_on(access, &copy).await?;
            title
        }
        Doc::EventSource => {
            let (title, copy) = copy_of(access, node_did, doc, id, EVENT_SOURCE_SPEC).await?;
            upsert_event_source_on(access, &copy).await?;
            title
        }
        Doc::Task | Doc::RemoteTool => anyhow::bail!(NOT_WIRED),
    };
    Ok(format!("Duplicated {title}"))
}

/// `id`'s title and its document as the desktop's duplicates write it (`SkillsPanel.tsx:310-322`,
/// `TriggersPanel.tsx:506-519`, `SchedulesPanel.tsx:252-264`, `EventSourcesPanel.tsx:332-344`):
/// a new id, "{title} copy", no timestamps; a skill's name gains "-copy" and a trigger's copy is
/// off.
async fn copy_of<T>(
    access: &ConfigAccess,
    node_did: &str,
    doc: Doc,
    id: &str,
    spec: ConfigDocumentSpec,
) -> anyhow::Result<(String, T)>
where
    T: DeserializeOwned,
{
    let mut document = load_one(access, spec, node_did, id).await?;
    let title = sections::title(doc, &document);
    let mut copy = json!({ "display_name": format!("{title} copy") });
    copy[spec.collection.unique_field()] = json!(new_id(sections::prefix(doc)));
    if doc == Doc::Skill {
        let name = sections::named(&document["name"]).unwrap_or(id);
        copy["name"] = json!(format!("{name}-copy"));
    }
    if doc == Doc::Trigger {
        copy["enabled"] = json!(false);
    }
    if let Some(stored) = document.as_object_mut() {
        stored.remove("created_at");
        stored.remove("updated_at");
    }
    Ok((title, forms::merged(Some(&document), copy)?))
}

fn read_home(home: &Path, menu: MenuId) -> anyhow::Result<Vec<MenuRow>> {
    Ok(match menu {
        MenuId::AllowedFolders => {
            sections::folders(&allowed::list(home)?, allowed::user_home().as_deref())
        }
        MenuId::UxPlugins => sections::ux_plugins(&ux::list(home)?),
        _ => workflows(&list_records(home)?, &list_promotions(home)?),
    })
}

pub(super) fn row(text: String, tone: Tone) -> MenuRow {
    MenuRow {
        text,
        tone,
        enter: None,
        d: None,
        more: Vec::new(),
    }
}

fn accounts(rows: &[AccountRow]) -> Vec<MenuRow> {
    let shown = |account: &AccountRow| {
        let (mark, tone) = match account.status.as_str() {
            "enabled" => ("●", Tone::Plain),
            _ => ("○", Tone::Label),
        };
        let plan = account
            .plan
            .as_ref()
            .map_or(String::new(), |plan| format!("   {plan}"));
        let text = format!(" {mark} {:<10} {}{plan}", account.label, account.status);
        row(text, tone)
    };
    let add = MenuRow {
        enter: Some(MenuAct::OpenSetup),
        ..row(" + Add a provider".to_string(), Tone::Accent)
    };
    std::iter::once(add).chain(rows.iter().map(shown)).collect()
}

fn agents(agents: &[AgentView], profiles: &[InferenceProfile]) -> Vec<MenuRow> {
    let shown = |agent: &AgentView| {
        let name = &agent.display_name;
        let model = match &agent.inference_profile_id {
            None => "No model".to_string(),
            Some(id) => profiles
                .iter()
                .find(|p| &p.profile_id == id)
                .map_or_else(|| id.clone(), profile_label),
        };
        let more = vec![(
            "Inference profile…".to_string(),
            MenuAct::ChooseProfile {
                agent: agent.agent_did.clone(),
                name: name.clone(),
            },
        )];
        if agent.is_default {
            return MenuRow {
                more,
                ..row(format!(" ● {name}   default   {model}"), Tone::Accent)
            };
        }
        let (text, tone) = match agent.enabled {
            true => (format!("   {name}   {model}"), Tone::Plain),
            false => (format!(" ○ {name}   off   {model}"), Tone::Label),
        };
        let make_default = MenuAct::SetDefaultAgent {
            agent: agent.agent_did.clone(),
            name: name.clone(),
        };
        MenuRow {
            enter: Some(make_default),
            more,
            ..row(text, tone)
        }
    };
    agents.iter().map(shown).collect()
}

/// The desktop's `profileLabel`: the display name, with the model unless the name holds it.
pub(super) fn profile_label(profile: &InferenceProfile) -> String {
    let name = profile
        .display_name
        .as_deref()
        .unwrap_or(&profile.profile_id);
    if name.contains(&profile.model_name) {
        name.to_string()
    } else {
        format!("{name} · {}", profile.model_name)
    }
}

/// One row per node profile, `agent`'s current one marked.
fn profiles(state: &InferenceSetupState, agent: &str) -> Vec<MenuRow> {
    let current = state
        .agents
        .iter()
        .find(|view| view.agent_did == agent)
        .and_then(|view| view.inference_profile_id.as_deref());
    let name = state
        .agents
        .iter()
        .find(|view| view.agent_did == agent)
        .map_or(agent, |view| view.display_name.as_str());
    let shown = |profile: &InferenceProfile| {
        let label = profile_label(profile);
        let (text, tone) = match current == Some(profile.profile_id.as_str()) {
            true => (format!(" ● {label}"), Tone::Accent),
            false => (format!("   {label}"), Tone::Plain),
        };
        MenuRow {
            enter: Some(MenuAct::SetAgentProfile {
                agent: agent.to_string(),
                name: name.to_string(),
                profile: profile.profile_id.clone(),
                label,
            }),
            ..row(text, tone)
        }
    };
    state.inference_profiles.iter().map(shown).collect()
}

/// Rows of `query_collection`'s `Tools` documents, whose absent groups read `null`.
fn tools(documents: &[Value]) -> Vec<MenuRow> {
    let shown = |tools: &Value| {
        let groups = [
            ("host", "host"),
            ("remote", "remote"),
            ("agents", "agents"),
            ("built_ins", "built-ins"),
            ("datastore", "datastore"),
            ("integrations", "integrations"),
            ("self_config", "self-config"),
        ];
        let present: Vec<_> = groups
            .into_iter()
            .filter_map(|(field, group)| (!tools[field].is_null()).then_some(group))
            .collect();
        let id = tools["tools_id"].as_str().unwrap_or_default();
        let name = tools["display_name"].as_str().unwrap_or(id);
        row(format!(" {name}   {}", present.join(" · ")), Tone::Plain)
    };
    documents.iter().map(shown).collect()
}

fn tasks(tasks: &[Value]) -> Vec<MenuRow> {
    let shown = |task: &Value| {
        let id = task["task_id"].as_str().unwrap_or_default();
        let agent = task["agent_did"].as_str().unwrap_or_default();
        let enabled = task["enabled"].as_bool().unwrap_or(false);
        let (text, tone) = match enabled {
            true => (format!(" ● {id}   {agent}"), Tone::Plain),
            false => (format!(" ○ {id}   {agent}   off"), Tone::Label),
        };
        MenuRow {
            text,
            tone,
            enter: Some(MenuAct::RunTask {
                task: id.to_string(),
            }),
            d: Some(MenuAct::SetEnabled {
                doc: Doc::Task,
                id: id.to_string(),
                label: sections::task_name(task).to_string(),
                enabled: !enabled,
            }),
            more: Vec::new(),
        }
    };
    tasks.iter().map(shown).collect()
}

fn mailbox(items: &[MailboxItem]) -> Vec<MenuRow> {
    let shown = |item: &MailboxItem| {
        let asks = [MailboxKind::Ask, MailboxKind::Gate]
            .iter()
            .any(|kind| item.kind == kind.as_str());
        let (mark, tone) = match asks {
            true => ("!", Tone::Warn),
            false => ("·", Tone::Plain),
        };
        MenuRow {
            text: format!(" {mark} {}: {}", item.kind, item.title),
            tone,
            enter: item
                .session_id
                .clone()
                .map(|session| MenuAct::OpenSession { session }),
            d: Some(MenuAct::Dismiss {
                item: item.doc_id.clone(),
            }),
            more: Vec::new(),
        }
    };
    items.iter().map(shown).collect()
}

/// Rows of `load_runtime_status_output`. Its `runtime_state` is the home's runtime record, an
/// object, so the runtime row shows the node's `process_state`.
fn status(output: &Value) -> Vec<MenuRow> {
    let process = output["process_state"].as_str().unwrap_or("unknown");
    let readiness = output["readiness_status"].as_str().unwrap_or("unknown");
    let runnable = &output["runnable_agent_count"];
    let mut rows = vec![
        row(format!(" runtime     {process}"), Tone::Plain),
        row(format!(" readiness   {readiness}"), Tone::Plain),
        row(format!(" agents      {runnable} runnable"), Tone::Plain),
    ];
    let unavailable = output["unavailable_agents"]
        .as_object()
        .into_iter()
        .flatten();
    rows.extend(unavailable.map(|(agent, reason)| {
        let reason = reason.as_str().unwrap_or_default();
        row(format!(" ! {agent}   {reason}"), Tone::Warn)
    }));
    let probes = output["backend_probes"].as_object().into_iter().flatten();
    rows.extend(probes.map(|(backend, probe)| {
        let state = probe["state"].as_str().unwrap_or_default();
        row(format!(" backend {backend}   {state}"), Tone::Plain)
    }));
    rows
}

fn workflows(installed: &[InstalledPlugin], promotions: &[PromotionRecord]) -> Vec<MenuRow> {
    let mut listed: Vec<_> = installed
        .iter()
        .map(|record| (format!("{}/{}", record.namespace, record.name), record))
        .filter(|(plugin, record)| record.declaration.job.0 && plugin != RUNNER)
        .collect();
    listed.sort_by(|(_, a), (_, b)| a.name.cmp(&b.name));
    let shown = |(plugin, record): (String, &InstalledPlugin)| {
        let promoted = match promotions
            .iter()
            .any(|promotion| promotion.plugin == plugin)
        {
            true => "   promoted",
            false => "",
        };
        let (name, description) = (&record.name, &record.declaration.description);
        let insert = MenuAct::Insert {
            text: format!("use the {name} workflow: "),
        };
        MenuRow {
            enter: Some(insert),
            ..row(format!(" {name}   {description}{promoted}"), Tone::Plain)
        }
    };
    listed.into_iter().map(shown).collect()
}

#[cfg(test)]
mod tests {
    use gents_shell::sample::Tone;
    use gents_shell::source::MenuAct;
    use serde_json::json;

    use super::super::testing::agent;
    use super::super::tests::mailbox_item;
    use super::*;

    fn row(text: &str, tone: Tone, enter: Option<MenuAct>, d: Option<MenuAct>) -> MenuRow {
        MenuRow {
            text: text.to_string(),
            tone,
            enter,
            d,
            more: Vec::new(),
        }
    }

    fn account(label: &str, status: &str, plan: Option<&str>) -> AccountRow {
        AccountRow {
            provider: "claude".to_string(),
            label: label.to_string(),
            identity: None,
            plan: plan.map(str::to_string),
            status: status.to_string(),
            default: false,
            credential_id: None,
            account_ref: None,
            backend_id: None,
            profiles: Vec::new(),
            usage: None,
            read: None,
        }
    }

    #[test]
    fn account_rows_mark_enabled_accounts_and_add_the_plan() {
        let rows = accounts(&[
            account("work", "enabled", Some("max")),
            account("personal", "disabled", None),
        ]);

        let add = Some(MenuAct::OpenSetup);
        assert_eq!(
            rows,
            [
                row(" + Add a provider", Tone::Accent, add, None),
                row(" ● work       enabled   max", Tone::Plain, None, None),
                row(" ○ personal   disabled", Tone::Label, None, None),
            ]
        );
    }

    #[test]
    fn agent_rows_mark_the_default_and_offer_the_rest() {
        let rows = agents(
            &[
                agent("engineer", "Engineer", true, true),
                agent("reviewer", "Reviewer", true, false),
                agent("scout", "Scout", false, false),
            ],
            &[],
        );
        let rows: Vec<_> = rows
            .into_iter()
            .map(|row| MenuRow {
                more: Vec::new(),
                ..row
            })
            .collect();

        let make_default = |agent: &str, name: &str| {
            Some(MenuAct::SetDefaultAgent {
                agent: agent.to_string(),
                name: name.to_string(),
            })
        };
        assert_eq!(
            rows,
            [
                row(" ● Engineer   default   No model", Tone::Accent, None, None),
                row(
                    "   Reviewer   No model",
                    Tone::Plain,
                    make_default("reviewer", "Reviewer"),
                    None
                ),
                row(
                    " ○ Scout   off   No model",
                    Tone::Label,
                    make_default("scout", "Scout"),
                    None
                ),
            ]
        );
    }

    #[test]
    fn tool_rows_name_the_groups_present() {
        let main = json!({
            "_docID": "bae-main",
            "tools_id": "main",
            "node_did": "did:key:z",
            "display_name": "Main tools",
            "host": {},
            "remote": null,
            "agents": {},
            "built_ins": null,
            "datastore": null,
            "integrations": null,
            "self_config": {},
        });
        let bare = json!({
            "_docID": "bae-bare",
            "tools_id": "bare",
            "node_did": "did:key:z",
            "display_name": null,
            "datastore": {},
        });

        assert_eq!(
            tools(&[main, bare]),
            [
                row(
                    " Main tools   host · agents · self-config",
                    Tone::Plain,
                    None,
                    None
                ),
                row(" bare   datastore", Tone::Plain, None, None),
            ]
        );
    }

    #[test]
    fn task_rows_offer_run_and_on_off() {
        let rows = tasks(&[
            json!({ "task_id": "digest", "display_name": "Digest", "agent_did": "engineer", "enabled": true }),
            json!({ "task_id": "sweep", "agent_did": "scout", "enabled": false }),
        ]);

        let run = |task: &str| {
            Some(MenuAct::RunTask {
                task: task.to_string(),
            })
        };
        let set = |task: &str, label: &str, enabled| {
            Some(MenuAct::SetEnabled {
                doc: Doc::Task,
                id: task.to_string(),
                label: label.to_string(),
                enabled,
            })
        };
        assert_eq!(
            rows,
            [
                row(
                    " ● digest   engineer",
                    Tone::Plain,
                    run("digest"),
                    set("digest", "Digest", false)
                ),
                row(
                    " ○ sweep   scout   off",
                    Tone::Label,
                    run("sweep"),
                    set("sweep", "sweep", true)
                ),
            ]
        );
    }

    #[test]
    fn an_agent_on_a_missing_profile_shows_its_id() {
        let mut scout = agent("scout", "Scout", true, false);
        scout.inference_profile_id = Some("gone".to_string());

        let rows = agents(&[scout], &[]);

        assert_eq!(rows[0].text, "   Scout   gone");
    }

    #[test]
    fn mailbox_rows_warn_on_questions_and_open_their_session() {
        let mut finished = mailbox_item("i3", "s1", "finished", "ack", "did:key:z", 3);
        finished["session_id"] = Value::Null;
        let items = [
            mailbox_item("i1", "s1", "ask", "ack", "did:key:z", 1),
            mailbox_item("i2", "s2", "gate", "ack", "did:key:z", 2),
            finished,
        ]
        .map(|item| serde_json::from_value(item).unwrap());

        let open = |session: &str| {
            Some(MenuAct::OpenSession {
                session: session.to_string(),
            })
        };
        let dismiss = |item: &str| {
            Some(MenuAct::Dismiss {
                item: item.to_string(),
            })
        };
        assert_eq!(
            mailbox(&items),
            [
                row(" ! ask: i1 title", Tone::Warn, open("s1"), dismiss("i1")),
                row(" ! gate: i2 title", Tone::Warn, open("s2"), dismiss("i2")),
                row(" · finished: i3 title", Tone::Plain, None, dismiss("i3")),
            ]
        );
    }

    #[test]
    fn status_rows_list_unavailable_agents() {
        let status = status(&json!({
            "process_state": "ready",
            "readiness_status": "degraded",
            "runnable_agent_count": 1,
            "unavailable_agents": { "scout": "its backend is unreachable" },
            "backend_probes": {
                "anthropic": { "backend_id": "anthropic", "state": "healthy" },
            },
        }));

        assert_eq!(
            status,
            [
                row(" runtime     ready", Tone::Plain, None, None),
                row(" readiness   degraded", Tone::Plain, None, None),
                row(" agents      1 runnable", Tone::Plain, None, None),
                row(
                    " ! scout   its backend is unreachable",
                    Tone::Warn,
                    None,
                    None
                ),
                row(" backend anthropic   healthy", Tone::Plain, None, None),
            ]
        );
    }

    fn installed(namespace: &str, name: &str, description: &str, job: bool) -> InstalledPlugin {
        serde_json::from_value(json!({
            "namespace": namespace,
            "name": name,
            "version": "1.0.0",
            "digest": "d",
            "language": "javascript",
            "declaration": {
                "name": name,
                "description": description,
                "artifact": "plugin.js",
                "language": "javascript",
                "input_schema": {},
                "job": job,
            },
        }))
        .unwrap()
    }

    #[test]
    fn workflow_rows_leave_out_the_runner_and_mark_promoted_ones() {
        let installed = [
            installed("gents", "workflow", "Runs a script", true),
            installed(
                "acme",
                "review_changes",
                "Review a diff with three readers",
                true,
            ),
            installed("acme", "ocr", "Reads the text in an image", false),
            installed("gents", "release_notes", "Write the release notes", true),
        ];
        let promoted = PromotionRecord {
            name: "release_notes".to_string(),
            plugin: "gents/release_notes".to_string(),
            script_id: "abc".to_string(),
            address: "s1:call_1".to_string(),
            session_id: "s1".to_string(),
            promoted_at: "2026-11-01T00:00:00Z".to_string(),
        };

        let insert = |name: &str| {
            Some(MenuAct::Insert {
                text: format!("use the {name} workflow: "),
            })
        };
        assert_eq!(
            workflows(&installed, &[promoted]),
            [
                row(
                    " release_notes   Write the release notes   promoted",
                    Tone::Plain,
                    insert("release_notes"),
                    None
                ),
                row(
                    " review_changes   Review a diff with three readers",
                    Tone::Plain,
                    insert("review_changes"),
                    None
                ),
            ]
        );
    }

    #[tokio::test]
    async fn setting_the_default_and_turning_a_task_off_notice_what_changed() {
        const OWNER: &str = gents_session::test_support::lineage_fixtures::FIXTURE_DID;
        let did = |name| gents_host::test_support::fixture_agent_named(name).agent_did;
        let served = gents_host::test_support::served_config(json!({
            "node": { "node_did": OWNER },
            "default_agent_selection": { "node_did": OWNER, "default_agent_did": "engineer" },
            "inference_backends": [{
                "node_did": OWNER,
                "backend_id": "backend",
                "name": "Backend",
                "provider_kind": "OpenAiCompatible",
                "endpoint": "http://127.0.0.1:1/v1",
                "auth": { "kind": "unauthenticated" },
            }],
            "inference_profiles": [{
                "node_did": OWNER,
                "profile_id": "profile",
                "backend_id": "backend",
                "model_name": "model",
            }],
            "agents": [
                { "node_did": OWNER, "agent_did": "engineer", "inference_profile_id": "profile" },
                { "node_did": OWNER, "agent_did": "reviewer", "inference_profile_id": "profile" },
            ],
            "tasks": [{
                "node_did": OWNER,
                "task_id": "digest",
                "agent_did": "engineer",
                "prompt_template": "digest the day",
            }],
        }))
        .await;
        let home = tempfile::tempdir().unwrap();
        let work = Work {
            endpoint: served.user.clone(),
            home: home.path().into(),
        };
        let make_default = MenuAct::SetDefaultAgent {
            agent: did("reviewer"),
            name: "Reviewer".to_string(),
        };
        let off = MenuAct::SetEnabled {
            doc: Doc::Task,
            id: "digest".to_string(),
            label: "Digest".to_string(),
            enabled: false,
        };

        let made = act(&work, home.path(), OWNER, &make_default).await;
        let turned = act(&work, home.path(), OWNER, &off).await;

        assert_eq!(made, Ok("default agent: Reviewer".to_string()));
        assert_eq!(turned, Ok("Digest is off".to_string()));
        let work = Ok(work);
        let agents = read_menu(&work, home.path(), OWNER, MenuId::Agents).await;
        let tasks = read_menu(&work, home.path(), OWNER, MenuId::Tasks).await;
        let texts = |rows: Result<Vec<MenuRow>, String>| {
            let rows = rows.unwrap();
            rows.into_iter().map(|row| row.text).collect::<Vec<_>>()
        };
        assert_eq!(
            texts(agents),
            [
                format!(" ● {}   default   profile · model", did("reviewer")),
                format!("   {}   profile · model", did("engineer")),
            ]
        );
        assert_eq!(
            texts(tasks),
            [format!(" ○ digest   {}   off", did("engineer"))]
        );
        served.node.shutdown().await.expect("node shutdown");
    }
}
