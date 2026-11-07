use std::path::Path;

use chrono::Utc;
use gents::plugin::store::{list_records, InstalledPlugin};
use gents::session::mailbox::{MailboxItem, MailboxKind};
use gents::store::config_client::patch::SelfConfigTarget;
use gents::tools::background::job::promote::{list_promotions, PromotionRecord};
use gents::Collection;
use gents_desktop_core::client::{patch_config_components_on, set_default_agent_on};
use gents_desktop_core::inference::{load_inference_setup_state, AgentView};
use gents_shell::sample::{MenuId, Tone};
use gents_shell::source::{Doc, MenuAct, MenuRow};
use serde_json::{json, Value};

use super::work::Work;
use crate::commands::accounts::{account_rows, AccountRow};
use crate::commands::config::crud::{query_collection, sort_rows, TOOLS_SPEC};
use crate::commands::config::task_run::enqueue_task_run_on;
use crate::commands::mailbox::{dismiss_served, served_items};
use crate::commands::status::load_runtime_status_output;
use crate::commands::task::load_task_inventory;

/// Runs the scripts an agent writes on the fly rather than being a workflow itself.
const RUNNER: &str = "gents/workflow";

/// What a section, form or act the shell asks for answers until it is wired.
pub(super) const NOT_WIRED: &str = "not wired yet";

/// `menu`'s rows, read as the node through `work`, or why they could not be. The installed
/// workflows are read from `home` alone, so they are listed while `work` is off.
pub(super) async fn read_menu(
    work: &Result<Work, String>,
    home: &Path,
    node_did: &str,
    menu: MenuId,
) -> Result<Vec<MenuRow>, String> {
    if menu == MenuId::Workflows {
        let home = home.to_path_buf();
        return tokio::task::spawn_blocking(move || read_workflows(&home))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| format!("{error:#}"));
    }
    let work = work.as_ref().map_err(String::clone)?;
    let access = work.access();
    let rows = match menu {
        MenuId::Accounts => account_rows(&access, node_did, None, Utc::now())
            .await
            .map(|rows| accounts(&rows)),
        MenuId::Agents => load_inference_setup_state(&access, node_did)
            .await
            .map(|state| agents(&state.agents)),
        MenuId::Tools => query_collection(&access, TOOLS_SPEC, node_did, None)
            .await
            .map(|mut documents| {
                sort_rows(&mut documents, Collection::Tools.unique_field());
                tools(&documents)
            }),
        MenuId::Tasks => load_task_inventory(&access, None)
            .await
            .map(|inventory| tasks(&inventory.tasks)),
        MenuId::Mailbox => served_items(&access, node_did, false)
            .await
            .map(|items| mailbox(&items)),
        MenuId::Status => load_runtime_status_output(Some(home), &work.endpoint, node_did)
            .await
            .map(|output| status(&output)),
        // Workflows is read above, and the shell never asks for its own menus' rows.
        MenuId::NodeSettings
        | MenuId::AppSettings
        | MenuId::Workflows
        | MenuId::More
        | MenuId::General
        | MenuId::Developer
        | MenuId::Experimental => Ok(Vec::new()),
        MenuId::Skills
        | MenuId::Triggers
        | MenuId::Schedules
        | MenuId::EventSources
        | MenuId::RemoteTools
        | MenuId::AllowedFolders
        | MenuId::UxPlugins
        | MenuId::Profiles => Err(anyhow::anyhow!(NOT_WIRED)),
    };
    rows.map_err(|error| format!("{error:#}"))
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
        MenuAct::RunTask { task } => {
            enqueue_task_run_on(&access, Some(home), task, json!({}), None, None)
                .await
                .map(|_| format!("started {task}"))
                .map_err(|error| format!("could not run {task}: {error:#}"))
        }
        MenuAct::SetEnabled {
            doc: Doc::Task,
            id: task,
            enabled,
        } => {
            let state = if *enabled { "on" } else { "off" };
            let patch = vec![("enabled".to_string(), Some(json!(enabled)))];
            let patches = [(SelfConfigTarget::Task, task.clone(), patch)];
            patch_config_components_on(&access, node_did, &patches)
                .await
                .map(|()| format!("{task} {state}"))
                .map_err(|error| format!("could not turn {task} {state}: {error:#}"))
        }
        MenuAct::Dismiss { item } => dismiss_served(&access, node_did, item)
            .await
            .map(|_| "dismissed".to_string())
            .map_err(|error| format!("could not dismiss it: {error:#}")),
        MenuAct::SetEnabled { .. }
        | MenuAct::Delete(..)
        | MenuAct::Duplicate(..)
        | MenuAct::SetFolderAccess { .. }
        | MenuAct::RemoveFolder(_)
        | MenuAct::SetAgentProfile { .. } => Err(NOT_WIRED.to_string()),
        MenuAct::OpenSession { .. }
        | MenuAct::Insert { .. }
        | MenuAct::OpenSetup
        | MenuAct::Open(_)
        | MenuAct::ConfirmDelete { .. }
        | MenuAct::ChooseProfile { .. } => {
            unreachable!("the shell runs these acts itself")
        }
    }
}

fn read_workflows(home: &Path) -> anyhow::Result<Vec<MenuRow>> {
    Ok(workflows(&list_records(home)?, &list_promotions(home)?))
}

fn row(text: String, tone: Tone) -> MenuRow {
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

fn agents(agents: &[AgentView]) -> Vec<MenuRow> {
    let shown = |agent: &AgentView| {
        let name = &agent.display_name;
        if agent.is_default {
            return row(format!(" ● {name}   default"), Tone::Accent);
        }
        let (text, tone) = match agent.enabled {
            true => (format!("   {name}"), Tone::Plain),
            false => (format!(" ○ {name}   off"), Tone::Label),
        };
        let make_default = MenuAct::SetDefaultAgent {
            agent: agent.agent_id.clone(),
            name: name.clone(),
        };
        MenuRow {
            enter: Some(make_default),
            ..row(text, tone)
        }
    };
    agents.iter().map(shown).collect()
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
        let agent = task["agent_id"].as_str().unwrap_or_default();
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

    fn agent(id: &str, name: &str, enabled: bool, is_default: bool) -> AgentView {
        AgentView {
            agent_id: id.to_string(),
            node_did: "did:key:z".to_string(),
            display_name: name.to_string(),
            description: None,
            context_id: None,
            inference_profile_id: None,
            enabled,
            is_default,
            tags: Vec::new(),
            created_at: None,
        }
    }

    #[test]
    fn agent_rows_mark_the_default_and_offer_the_rest() {
        let rows = agents(&[
            agent("engineer", "Engineer", true, true),
            agent("reviewer", "Reviewer", true, false),
            agent("scout", "Scout", false, false),
        ]);

        let make_default = |agent: &str, name: &str| {
            Some(MenuAct::SetDefaultAgent {
                agent: agent.to_string(),
                name: name.to_string(),
            })
        };
        assert_eq!(
            rows,
            [
                row(" ● Engineer   default", Tone::Accent, None, None),
                row(
                    "   Reviewer",
                    Tone::Plain,
                    make_default("reviewer", "Reviewer"),
                    None
                ),
                row(
                    " ○ Scout   off",
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
            json!({ "task_id": "digest", "agent_id": "engineer", "enabled": true }),
            json!({ "task_id": "sweep", "agent_id": "scout", "enabled": false }),
        ]);

        let run = |task: &str| {
            Some(MenuAct::RunTask {
                task: task.to_string(),
            })
        };
        let set = |task: &str, enabled| {
            Some(MenuAct::SetEnabled {
                doc: Doc::Task,
                id: task.to_string(),
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
                    set("digest", false)
                ),
                row(
                    " ○ sweep   scout   off",
                    Tone::Label,
                    run("sweep"),
                    set("sweep", true)
                ),
            ]
        );
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
        let (node, graphql) = crate::shared::test_support::served_config(json!({
            "node": { "node_did": "owner", "default_agent_id": "engineer" },
            "inference_backends": [{
                "node_did": "owner",
                "backend_id": "backend",
                "name": "Backend",
                "provider_kind": "OpenAiCompatible",
                "endpoint": "http://127.0.0.1:1/v1",
                "auth": { "kind": "unauthenticated" },
            }],
            "inference_profiles": [{
                "node_did": "owner",
                "profile_id": "profile",
                "backend_id": "backend",
                "model_name": "model",
            }],
            "agents": [
                { "node_did": "owner", "agent_id": "engineer", "inference_profile_id": "profile" },
                { "node_did": "owner", "agent_id": "reviewer", "inference_profile_id": "profile" },
            ],
            "tasks": [{
                "node_did": "owner",
                "task_id": "digest",
                "agent_id": "engineer",
                "prompt_template": "digest the day",
            }],
        }))
        .await;
        let home = tempfile::tempdir().unwrap();
        let work = Work {
            endpoint: gents::store::config_client::GraphqlEndpoint::anonymous(graphql),
            home: home.path().into(),
        };
        let make_default = MenuAct::SetDefaultAgent {
            agent: "reviewer".to_string(),
            name: "Reviewer".to_string(),
        };
        let off = MenuAct::SetEnabled {
            doc: Doc::Task,
            id: "digest".to_string(),
            enabled: false,
        };

        let made = act(&work, home.path(), "owner", &make_default).await;
        let turned = act(&work, home.path(), "owner", &off).await;

        assert_eq!(made, Ok("default agent: Reviewer".to_string()));
        assert_eq!(turned, Ok("digest off".to_string()));
        let work = Ok(work);
        let agents = read_menu(&work, home.path(), "owner", MenuId::Agents).await;
        let tasks = read_menu(&work, home.path(), "owner", MenuId::Tasks).await;
        let texts = |rows: Result<Vec<MenuRow>, String>| {
            let rows = rows.unwrap();
            rows.into_iter().map(|row| row.text).collect::<Vec<_>>()
        };
        assert_eq!(texts(agents), [" ● reviewer   default", "   engineer"]);
        assert_eq!(texts(tasks), [" ○ digest   engineer   off"]);
        node.shutdown().await;
    }
}
