use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};
use gents::pack::{PackChangeError, SlotBindingError};
use gents::plugin::authority::GrantError;
use gents::store::config_client::ConfigAccess;
use gents::store::local::storage_backend::drive_unsendable;
use gents_desktop_core::inference::load_inference_setup_state;
use gents_shell::sample::Tone;
use gents_shell::source::{
    EditedChoice, FieldKind, Form, FormError, FormRef, MenuAct, MenuRow, PackAct, Update,
};
use serde_json::Value;
use tokio::runtime::Handle;

use super::forms::{field, hinted, items, refused};
use super::menus::{blocking, row};
use super::source::closed;
use super::work::Work;
use crate::commands::config::crud::{
    query_collection_sorted, EVENT_SOURCE_SPEC, SCHEDULE_SPEC, SKILL_SPEC, TOOLS_SPEC,
};
use crate::commands::task::load_task_inventory;
use crate::packs::EditedDocuments;

#[cfg(test)]
mod tests;

/// The kinds of document `packContents.ts:12-44` counts, `(one, many)`, in its order.
const KINDS: [(&str, &str); 8] = [
    ("agent", "agents"),
    ("skill", "skills"),
    ("task", "tasks"),
    ("tool set", "tool sets"),
    ("profile", "profiles"),
    ("provider", "providers"),
    ("schedule", "schedules"),
    ("event source", "event sources"),
];

/// Packs' rows, showing the registry's `page` for `query`, and the notice `pack_rows` answers with
/// them. The pack calls run only once the home's runtime answers: without it they open and claim
/// the home's store (`resolve_config_access`).
pub(super) async fn read(
    work: &Result<Work, String>,
    home: &Path,
    node_did: &str,
    runtime: &Handle,
    query: &str,
    page: u32,
) -> Result<(Vec<MenuRow>, Option<String>), String> {
    let work = work.as_ref().map_err(String::clone)?;
    if !runtime_answers(home).await {
        return Err(closed(None));
    }
    let query_sent = query.to_string();
    let access = work.access();
    let (installed, slots, account, registry, tags) = tokio::join!(
        call(runtime, home, |home| crate::packs::installed(home, None)),
        call(runtime, home, crate::packs::plugin_slots),
        call(runtime, home, |home| crate::packs::whoami(home, None)),
        call(runtime, home, move |_| crate::packs::search(
            None, query_sent, page
        )),
        tags(&access, node_did),
    );
    let account = account.ok();
    let account = account
        .as_ref()
        .and_then(|me| me["account"]["username"].as_str());
    // The desktop derives these from documents it has already read, so their failure shows nowhere.
    let tags = tags.unwrap_or_else(|error| {
        let error = format!("{error:#}");
        tracing::warn!(%error, "reading the documents packs added failed; Packs shows no Adds lines");
        Vec::new()
    });
    Ok(pack_rows(
        installed.as_ref().map_err(String::as_str),
        slots.as_ref().map_err(String::as_str),
        account,
        registry.as_ref().map_err(String::as_str),
        query,
        page,
        &adds(&tags),
    ))
}

async fn runtime_answers(home: &Path) -> bool {
    let home = home.to_path_buf();
    match blocking(move || crate::home_state::read_runtime_state(&home)).await {
        Ok(Some(state)) => crate::graphql_access::graphql_endpoint_available(&state.graphql).await,
        Ok(None) | Err(_) => false,
    }
}

/// A pack call once the home's runtime answers, as `read`'s calls wait for it.
async fn guarded<F, Fut>(runtime: Handle, home: PathBuf, pack: F) -> Result<Value, String>
where
    F: FnOnce(PathBuf) -> Fut + Send + 'static,
    Fut: Future<Output = anyhow::Result<Value>>,
{
    if !runtime_answers(&home).await {
        return Err(closed(None));
    }
    call(&runtime, &home, pack).await
}

/// A pack call, whose future is not `Send`, driven on one of `runtime`'s blocking threads.
async fn call<F, Fut>(runtime: &Handle, home: &Path, pack: F) -> Result<Value, String>
where
    F: FnOnce(PathBuf) -> Fut + Send + 'static,
    Fut: Future<Output = anyhow::Result<Value>>,
{
    let home = home.to_path_buf();
    match drive_unsendable(runtime, move || pack(home)).await {
        Ok(answer) => answer.map_err(|error| format!("{error:#}")),
        // The desktop bridge's words (`tauri_commands/packs.rs:46`).
        Err(stopped) => Err(format!("the pack operation stopped: {stopped}")),
    }
}

/// The tags of the node's documents of each of `KINDS`, in its order.
async fn tags(access: &ConfigAccess, node_did: &str) -> anyhow::Result<Vec<Vec<Vec<String>>>> {
    let setup = load_inference_setup_state(access, node_did).await?;
    let tasks = load_task_inventory(access, None).await?.tasks;
    let tasks = tasks
        .into_iter()
        .filter(|task| task["node_did"] == node_did);
    let of = |documents: Vec<Value>| -> Vec<Vec<String>> {
        let tags = documents
            .into_iter()
            .map(|mut document| document["tags"].take());
        tags.map(|tags| serde_json::from_value(tags).unwrap_or_default())
            .collect()
    };
    let read = |spec| query_collection_sorted(access, spec, node_did);
    Ok(vec![
        setup.agents.into_iter().map(|agent| agent.tags).collect(),
        of(read(SKILL_SPEC).await?),
        of(tasks.collect()),
        of(read(TOOLS_SPEC).await?),
        setup
            .inference_profiles
            .into_iter()
            .map(|p| p.tags)
            .collect(),
        setup
            .inference_backends
            .into_iter()
            .map(|b| b.tags)
            .collect(),
        of(read(SCHEDULE_SPEC).await?),
        of(read(EVENT_SOURCE_SPEC).await?),
    ])
}

/// What each pack installed, by its name, in `packContents.ts:7-46`'s words: "2 agents · 1 skill".
/// `tags` holds the tags of each of `KINDS`' documents, in its order; a document whose origin tags
/// do not parse is no pack's.
pub(super) fn adds(tags: &[Vec<Vec<String>>]) -> BTreeMap<String, String> {
    let mut adds: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for (documents, (one, many)) in tags.iter().zip(KINDS) {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for tags in documents {
            if let Ok(Some(pack)) = gents::pack::pack_origin_from_tags(tags) {
                *counts.entry(pack).or_default() += 1;
            }
        }
        for (pack, count) in counts {
            let kind = if count == 1 { one } else { many };
            adds.entry(pack)
                .or_default()
                .push(format!("{count} {kind}"));
        }
    }
    let adds = adds.into_iter();
    adds.map(|(pack, kinds)| (pack.to_string(), kinds.join(" · ")))
        .collect()
}

/// `PacksPanel.tsx:261-437`: Installed, Model calls while a plugin has a model slot, Registry and
/// Account, with the notice for the desktop's toast. A failed `slots` read drops its group and
/// the notice tells it, as the desktop toasts it and empties what it read (`:68-75`); an account
/// that could not be read is signed out (`:82-87`).
pub(super) fn pack_rows(
    installed: Result<&Value, &str>,
    slots: Result<&Value, &str>,
    account: Option<&str>,
    registry: Result<&Value, &str>,
    query: &str,
    page: u32,
    adds: &BTreeMap<String, String>,
) -> (Vec<MenuRow>, Option<String>) {
    let label = |text: &str| row(text.to_string(), Tone::Label);
    let packs = installed.ok().and_then(|report| report["packs"].as_array());
    let packs = packs.map_or(&[][..], Vec::as_slice);
    let mut rows = vec![label(" Installed")];
    match installed {
        Err(error) => rows.extend([
            row(" Couldn't read the installed packs".to_string(), Tone::Warn),
            row(format!("   {error}"), Tone::Warn),
        ]),
        Ok(_) if packs.is_empty() => rows.push(label(" No packs installed")),
        Ok(_) => rows.extend(packs.iter().flat_map(|pack| installed_rows(pack, adds))),
    }
    let read = slots.ok().unwrap_or(&Value::Null);
    let plugins = read["plugins"].as_array().map_or(&[][..], Vec::as_slice);
    if !plugins.is_empty() {
        rows.push(label(" Model calls"));
        rows.extend(
            plugins
                .iter()
                .map(|slot| model_call(slot, &read["profiles"])),
        );
    }
    rows.push(label(" Registry"));
    let shown = if query.is_empty() {
        "Packs and plugins"
    } else {
        query
    };
    rows.push(MenuRow {
        enter: Some(MenuAct::Open(FormRef::PackSearch)),
        ..row(format!(" Search   {shown}"), Tone::Plain)
    });
    let found = registry.ok().and_then(|report| report["packs"].as_array());
    let found = found.map_or(&[][..], Vec::as_slice);
    match registry {
        Err(error) => rows.push(row(format!("   {error}"), Tone::Warn)),
        Ok(_) if found.is_empty() && query.is_empty() => {
            rows.push(label(" The registry has no packs yet"));
        }
        Ok(_) if found.is_empty() => rows.push(label(" No packs match")),
        Ok(_) => rows.extend(found.iter().map(|pack| registry_row(pack, packs))),
    }
    let has_more = registry.is_ok_and(|report| report["has_more"] == true);
    if page > 1 || has_more {
        let to = |text: &str, page| MenuRow {
            enter: Some(MenuAct::Pack(PackAct::Page(page))),
            ..row(text.to_string(), Tone::Plain)
        };
        rows.push(label(&format!(" Page {page}")));
        rows.extend((page > 1).then(|| to(" Previous", page - 1)));
        rows.extend(has_more.then(|| to(" Next", page + 1)));
    }
    rows.push(label(" Account"));
    rows.push(match account {
        Some(name) => MenuRow {
            more: vec![("Sign out".to_string(), MenuAct::Pack(PackAct::SignOut))],
            ..row(format!(" {name}   Signed in to the registry"), Tone::Plain)
        },
        None => MenuRow {
            enter: Some(MenuAct::Open(FormRef::PackSignIn)),
            ..row(
                " Sign in   A token from your registry dashboard".to_string(),
                Tone::Plain,
            )
        },
    });
    (rows, slots.err().map(str::to_string))
}

/// `PacksPanel.tsx:269-308`: its version, then what it added (`:443-466`), with Update while one
/// is available, and Remove. The documents a pack installed carry its name alone as their origin
/// (`packContents.ts:5-8`).
fn installed_rows(pack: &Value, adds: &BTreeMap<String, String>) -> Vec<MenuRow> {
    let name = pack["pack"].as_str().unwrap_or_default();
    let version = pack["installed"].as_str().unwrap_or_default();
    let outdated = pack["outdated"] == true;
    let status = match (outdated, pack["error"].as_str()) {
        (true, _) => format!(
            ", {} available",
            pack["latest"].as_str().unwrap_or_default()
        ),
        (false, Some(error)) if !error.is_empty() => " · couldn't check for updates".to_string(),
        (false, _) => String::new(),
    };
    let update = PackAct::Update {
        pack: name.to_string(),
        edited: EditedChoice::Refuse,
    };
    let remove = PackAct::Remove {
        pack: name.to_string(),
        confirmed: false,
    };
    let more = outdated.then(|| ("Update".to_string(), MenuAct::Pack(update)));
    let more = more
        .into_iter()
        .chain([("Remove".to_string(), MenuAct::Pack(remove))]);
    let mut rows = vec![MenuRow {
        more: more.collect(),
        ..row(format!(" {name}   {version}{status}"), Tone::Plain)
    }];
    let origin = name.rsplit('/').next().unwrap_or_default();
    if let Some(kinds) = adds.get(origin) {
        rows.push(row(format!("   Adds {kinds}"), Tone::Label));
    }
    rows
}

/// `PacksPanel.tsx:314-328`: the plugin and its bound profile among the usable ones, or "Not
/// set"; enter chooses another.
fn model_call(slot: &Value, profiles: &Value) -> MenuRow {
    let plugin = slot["plugin"].as_str().unwrap_or_default();
    let bound = slot["profile"].as_str().map(|id| {
        match usable(profiles).find(|profile| profile["profile_id"] == id) {
            Some(profile) => profile_name(profile),
            None => id,
        }
    });
    let choose = PackAct::ChooseSlot {
        plugin: plugin.to_string(),
    };
    MenuRow {
        enter: Some(MenuAct::Pack(choose)),
        ..row(
            format!(" {plugin}   {}", bound.unwrap_or("Not set")),
            Tone::Plain,
        )
    }
}

fn usable(profiles: &Value) -> impl Iterator<Item = &Value> {
    let profiles = profiles.as_array().into_iter().flatten();
    profiles.filter(|profile| profile["usable"] == true)
}

/// A profile as `PacksPanel.tsx:326` names it: its display name, or else its model.
fn profile_name(profile: &Value) -> &str {
    let name = profile["display_name"]
        .as_str()
        .filter(|name| !name.is_empty());
    let name = name.or(profile["model_name"].as_str());
    name.or(profile["profile_id"].as_str()).unwrap_or_default()
}

/// `RegistryPack.tsx:97-155`: its name and description, and whether it is installed; enter
/// installs one that is not, and `.` opens its page or searches its first keywords (`:126-131`).
fn registry_row(pack: &Value, installed: &[Value]) -> MenuRow {
    let name = format!(
        "{}/{}",
        pack["namespace"].as_str().unwrap_or_default(),
        pack["name"].as_str().unwrap_or_default()
    );
    let description = match pack["description"].as_str() {
        Some(text) if !text.trim().is_empty() => format!("   {text}"),
        _ => String::new(),
    };
    let installed = installed.iter().any(|pack| pack["pack"] == name.as_str());
    let about = PackAct::Info {
        pack: name.clone(),
        installed,
    };
    let keywords = items(&pack["keywords"]).take(2).map(|word| {
        let search = PackAct::Keyword(word.to_string());
        (format!("Search for {word}"), MenuAct::Pack(search))
    });
    let about = ("About this pack".to_string(), MenuAct::Pack(about));
    let more = std::iter::once(about).chain(keywords).collect();
    match installed {
        true => MenuRow {
            more,
            ..row(format!(" {name}{description} · Installed"), Tone::Label)
        },
        false => MenuRow {
            enter: Some(MenuAct::Pack(PackAct::Install {
                pack: name.clone(),
                edited: EditedChoice::Refuse,
                grant: false,
                slots: Vec::new(),
            })),
            more,
            ..row(format!(" {name}{description}"), Tone::Plain)
        },
    }
}

/// Installs `pack` as the desktop does (`PacksPanel.tsx:131-160`), once the home's runtime
/// answers.
pub(super) async fn install(
    runtime: Handle,
    home: PathBuf,
    pack: String,
    edited: EditedChoice,
    grant: bool,
    slots: Vec<String>,
) -> Result<Value, PackChangeError> {
    let install =
        move |home| crate::packs::install(home, pack, None, slots, documents(edited), grant, false);
    change(&runtime, &home, install).await
}

/// `PacksPanel.tsx:162-180`.
pub(super) async fn update(
    runtime: Handle,
    home: PathBuf,
    pack: String,
    edited: EditedChoice,
) -> Result<Value, PackChangeError> {
    let update = move |home| crate::packs::update(home, Some(pack), None, documents(edited));
    change(&runtime, &home, update).await
}

/// `PacksPanel.tsx:182-195`.
pub(super) async fn remove(
    runtime: Handle,
    home: PathBuf,
    pack: String,
) -> Result<Value, PackChangeError> {
    let remove = move |home| crate::packs::remove(home, pack, EditedDocuments::Refuse);
    change(&runtime, &home, remove).await
}

/// What a remove says (`PacksPanel.tsx:187-191`): every refusal, edited documents' too, is its
/// notice.
pub(super) fn removed(pack: &str, done: Result<Value, PackChangeError>) -> Update {
    match done {
        Ok(_) => Update::Notice(format!("Removed {pack}")),
        Err(error) => Update::Notice(format!("{error:#}")),
    }
}

/// `PacksPanel.tsx:183`: removing asks first.
pub(super) fn confirm_remove(pack: String) -> Update {
    let title = format!("Remove {pack} and everything it installed?");
    let remove = PackAct::Remove {
        pack,
        confirmed: true,
    };
    let rows = vec![choice(" Remove", remove)];
    Update::Ask { title, rows }
}

/// What an install says (`PacksPanel.tsx:139-156`): the toast, or the `window.confirm` asked
/// before trying again. A retry keeps what the other questions chose. Where the desktop only
/// toasts that a pack's slot needs a model, the shell asks which, one slot per retry.
pub(super) fn installed(
    pack: String,
    edited: EditedChoice,
    grant: bool,
    slots: Vec<String>,
    done: Result<Value, PackChangeError>,
) -> Update {
    let install = |edited, grant, slots| PackAct::Install {
        pack: pack.clone(),
        edited,
        grant,
        slots,
    };
    let title = format!("Install {pack}");
    match done {
        Ok(_) => Update::Notice(format!("Installed {pack}")),
        Err(refusal @ PackChangeError::Grant(GrantError::Ungranted { .. })) => {
            let allow = vec![
                row(" Allow it?".to_string(), Tone::Label),
                choice(" Allow", install(edited, true, slots)),
            ];
            ask(title, &refusal, allow)
        }
        Err(refusal @ PackChangeError::Edited { .. }) => {
            let replace = install(EditedChoice::Overwrite, grant, slots.clone());
            let keep = install(EditedChoice::Keep, grant, slots);
            ask(title, &refusal, keep_or_replace(replace, keep))
        }
        Err(PackChangeError::Slots(SlotBindingError::Unbound {
            slots: unbound,
            profiles,
            ..
        })) => {
            let slot = &unbound[0];
            let profiles = serde_json::to_value(profiles).unwrap_or_default();
            let mut rows = vec![row(format!(" Which model should {slot} use?"), Tone::Label)];
            rows.extend(usable(&profiles).map(|profile| {
                let id = profile["profile_id"].as_str().unwrap_or_default();
                let chosen = slots.iter().cloned().chain([format!("{slot}={id}")]);
                let pick = install(edited, grant, chosen.collect());
                choice(&format!(" {}", profile_name(profile)), pick)
            }));
            Update::Ask { title, rows }
        }
        Err(
            error @ (PackChangeError::Grant(GrantError::Other(_))
            | PackChangeError::Slots(SlotBindingError::Other(_))
            | PackChangeError::Other(_)),
        ) => Update::Notice(format!("{error:#}")),
    }
}

/// What an update says (`PacksPanel.tsx:166-176`). An update cannot grant authority, so that
/// refusal is its notice.
pub(super) fn updated(pack: String, done: Result<Value, PackChangeError>) -> Update {
    let update = |edited| PackAct::Update {
        pack: pack.clone(),
        edited,
    };
    match done {
        Ok(_) => Update::Notice(format!("Updated {pack}")),
        Err(refusal @ PackChangeError::Edited { .. }) => {
            let (replace, keep) = (update(EditedChoice::Overwrite), update(EditedChoice::Keep));
            ask(
                format!("Update {pack}"),
                &refusal,
                keep_or_replace(replace, keep),
            )
        }
        Err(
            error @ (PackChangeError::Grant(_)
            | PackChangeError::Slots(_)
            | PackChangeError::Other(_)),
        ) => Update::Notice(format!("{error:#}")),
    }
}

/// The desktop's `window.confirm` as a question: the refusal, then the choices.
fn ask(title: String, refusal: &PackChangeError, choices: Vec<MenuRow>) -> Update {
    let mut rows = vec![row(format!(" {refusal}"), Tone::Warn)];
    rows.extend(choices);
    Update::Ask { title, rows }
}

/// "OK replaces them, Cancel keeps your edits." (`PacksPanel.tsx:152`), as two choices.
fn keep_or_replace(replace: PackAct, keep: PackAct) -> Vec<MenuRow> {
    vec![
        choice(" Replace them", replace),
        choice(" Keep my edits", keep),
    ]
}

fn choice(text: &str, act: PackAct) -> MenuRow {
    MenuRow {
        enter: Some(MenuAct::Pack(act)),
        ..row(text.to_string(), Tone::Plain)
    }
}

fn documents(edited: EditedChoice) -> EditedDocuments {
    match edited {
        EditedChoice::Refuse => EditedDocuments::Refuse,
        EditedChoice::Overwrite => EditedDocuments::Overwrite,
        EditedChoice::Keep => EditedDocuments::Keep,
    }
}

/// A pack change, driven as `call` drives a read, once the home's runtime answers.
async fn change<F, Fut>(runtime: &Handle, home: &Path, pack: F) -> Result<Value, PackChangeError>
where
    F: FnOnce(PathBuf) -> Fut + Send + 'static,
    Fut: Future<Output = Result<Value, PackChangeError>>,
{
    if !runtime_answers(home).await {
        return Err(PackChangeError::Other(anyhow::Error::msg(closed(None))));
    }
    let home = home.to_path_buf();
    drive_unsendable(runtime, move || pack(home))
        .await
        .unwrap_or_else(|stopped| {
            let stopped = format!("the pack operation stopped: {stopped}");
            Err(PackChangeError::Other(anyhow::Error::msg(stopped)))
        })
}

/// The desktop's search box (`PacksPanel.tsx:334-349`), holding `query`.
pub(super) fn search_form(query: &str) -> Form {
    Form {
        form: FormRef::PackSearch,
        title: "Search the registry".to_string(),
        note: Vec::new(),
        fields: vec![hinted(
            field("query", "Search", FieldKind::Text, query),
            "Packs and plugins",
        )],
        actions: Vec::new(),
    }
}

/// The profiles `plugin`'s model calls can run on (`PacksPanel.tsx:312-331`), read again once the
/// home's runtime answers.
pub(super) async fn choose_slot(runtime: Handle, home: PathBuf, plugin: String) -> Update {
    let slots = guarded(runtime, home, crate::packs::plugin_slots).await;
    slot_choices(&plugin, slots.as_ref().map_err(String::as_str))
}

/// Not set, then the usable profiles, the bound one marked; a failed read is its notice, as the
/// desktop toasts it (`PacksPanel.tsx:72-75`).
pub(super) fn slot_choices(plugin: &str, slots: Result<&Value, &str>) -> Update {
    let slots = match slots {
        Ok(slots) => slots,
        Err(error) => return Update::Notice(error.to_string()),
    };
    let mut plugins = slots["plugins"].as_array().into_iter().flatten();
    let slot = plugins.find(|slot| slot["plugin"] == plugin);
    let bound = slot.and_then(|slot| slot["profile"].as_str());
    let bind = |profile: Option<&str>| PackAct::BindSlot {
        plugin: plugin.to_string(),
        profile: profile.map(str::to_string),
    };
    let note = " Optional. Unset, the plugin runs without a model.";
    let mut rows = vec![
        row(note.to_string(), Tone::Label),
        choice(" Not set", bind(None)),
    ];
    rows.extend(usable(&slots["profiles"]).map(|profile| {
        let id = profile["profile_id"].as_str().unwrap_or_default();
        let name = profile_name(profile);
        let (text, tone) = match bound == Some(id) {
            true => (format!(" ● {name}"), Tone::Accent),
            false => (format!("   {name}"), Tone::Plain),
        };
        MenuRow {
            enter: Some(MenuAct::Pack(bind(Some(id)))),
            ..row(text, tone)
        }
    }));
    let title = plugin.to_string();
    Update::Ask { title, rows }
}

/// Binds or, with no profile, unbinds `plugin`'s model calls (`PacksPanel.tsx:197-207`), once the
/// home's runtime answers.
pub(super) async fn bind(
    runtime: Handle,
    home: PathBuf,
    plugin: String,
    profile: Option<String>,
) -> Result<Value, String> {
    let bind = move |home| crate::packs::bind_plugin_slot(home, plugin, profile);
    guarded(runtime, home, bind).await
}

/// What a bind or a sign-out says: the desktop toasts only an error (`PacksPanel.tsx:197-207`,
/// `:222-227`).
pub(super) fn quiet(done: Result<Value, String>) -> Option<Update> {
    done.err().map(Update::Notice)
}

/// `PacksPanel.tsx:414-437`: the token from the registry's dashboard.
pub(super) fn sign_in_form() -> Form {
    let token = field("token", "Registry token", FieldKind::Secret, "");
    Form {
        form: FormRef::PackSignIn,
        title: "Sign in".to_string(),
        note: vec!["A token from your registry dashboard".to_string()],
        fields: vec![hinted(token, "Registry token")],
        actions: Vec::new(),
    }
}

/// `PacksPanel.tsx:209-220`, once the home's runtime answers.
pub(super) async fn sign_in(
    runtime: Handle,
    home: PathBuf,
    token: String,
) -> Result<Value, String> {
    let login = move |home| crate::packs::login(home, None, token);
    guarded(runtime, home, login).await
}

/// What a sign-in's save says, where the desktop refreshes without a toast.
pub(super) fn signed_in(done: Result<Value, String>) -> Result<String, FormError> {
    let done = done.map(|_| "Signed in to the registry".to_string());
    done.map_err(refused)
}

/// `PacksPanel.tsx:222-227`, once the home's runtime answers.
pub(super) async fn sign_out(runtime: Handle, home: PathBuf) -> Result<Value, String> {
    guarded(runtime, home, |home| crate::packs::logout(home, None)).await
}

/// A registry pack's page, read from the registry alone.
pub(super) async fn page(runtime: Handle, home: PathBuf, pack: String, installed: bool) -> Update {
    let asked = pack.clone();
    let info = call(&runtime, &home, move |_| crate::packs::info(None, asked)).await;
    pack_page(&pack, installed, info.as_ref().map_err(String::as_str))
}

/// `RegistryPack.tsx:171-243`: the facts of its latest version, then Install while it is not
/// installed (`:143-155`).
pub(super) fn pack_page(pack: &str, installed: bool, info: Result<&Value, &str>) -> Update {
    let mut rows: Vec<_> = match info {
        Ok(info) => facts(info)
            .into_iter()
            .map(|fact| row(format!(" {fact}"), Tone::Plain))
            .collect(),
        Err(error) => {
            let unread = format!(" Couldn't read this pack's page: {error}");
            vec![row(unread, Tone::Warn)]
        }
    };
    if !installed {
        let install = PackAct::Install {
            pack: pack.to_string(),
            edited: EditedChoice::Refuse,
            grant: false,
            slots: Vec::new(),
        };
        rows.push(choice(" Install", install));
    }
    let title = pack.to_string();
    Update::Ask { title, rows }
}

/// `PackDetail`'s facts (`RegistryPack.tsx:195-214`) and its homepage (`:228-240`): none while no
/// version stands that is not yanked.
fn facts(info: &Value) -> Vec<String> {
    let versions = info["versions"].as_array().into_iter().flatten();
    let versions: Vec<_> = versions
        .filter(|version| version["version"].is_string() && version["yanked"] != true)
        .collect();
    let latest = versions
        .iter()
        .find(|version| version["version"] == info["latest"]);
    let Some(latest) = latest.or(versions.last()) else {
        return Vec::new();
    };
    let mut facts = Vec::new();
    facts.extend(nonblank(&info["owner"]).map(|owner| format!("by {owner}")));
    let version = latest["version"]
        .as_str()
        .filter(|version| !version.is_empty());
    let published = nonblank(&latest["published_at"]).map(|at| format!("published {}", day(at)));
    let released: Vec<_> = version
        .map(str::to_string)
        .into_iter()
        .chain(published)
        .collect();
    facts.extend((!released.is_empty()).then(|| released.join(", ")));
    facts.extend(latest["size_bytes"].as_u64().map(bytes));
    let plugins = info["plugins"].as_array().into_iter().flatten();
    let plugins: Vec<_> = plugins
        .filter_map(|plugin| plugin.as_str().or(plugin["name"].as_str()))
        .collect();
    let (count, older) = (plugins.len(), versions.len() - 1);
    let plural = |count, one, many| if count == 1 { one } else { many };
    if count > 0 {
        let plugin = plural(count, "plugin", "plugins");
        facts.push(format!("{count} {plugin}: {}", plugins.join(", ")));
    }
    if older > 0 {
        let version = plural(older, "version", "versions");
        facts.push(format!("{older} older {version}"));
    }
    let homepage = nonblank(&info["homepage"]).filter(|url| external(url));
    facts.extend(homepage.map(|url| format!("Homepage {url}")));
    facts
}

/// A string that is not blank, as `RegistryPack.tsx:21-22` reads one.
fn nonblank(value: &Value) -> Option<&str> {
    value.as_str().filter(|text| !text.trim().is_empty())
}

/// `RegistryPack.tsx:52-56`.
fn bytes(count: u64) -> String {
    let (kb, mb) = (1024.0, 1024.0 * 1024.0);
    match count as f64 {
        size if size < kb => format!("{count} B"),
        size if size < mb => format!("{:.1} KB", size / kb),
        size => format!("{:.1} MB", size / mb),
    }
}

/// `RegistryPack.tsx:58-63` in the desktop's English: "Mar 5, 2026" on the local clock.
fn day(at: &str) -> String {
    match DateTime::parse_from_rfc3339(at) {
        Ok(at) => at.with_timezone(&Local).format("%b %-d, %Y").to_string(),
        Err(_) => at.to_string(),
    }
}

/// The links the desktop opens (`lib/externalLinks.ts:3-13`).
fn external(url: &str) -> bool {
    let scheme = |scheme: &str| {
        url.get(..scheme.len())
            .is_some_and(|at| at.eq_ignore_ascii_case(scheme))
    };
    ["http:", "https:", "mailto:"].into_iter().any(scheme)
}
