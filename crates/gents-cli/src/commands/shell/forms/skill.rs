use gents::documents::SkillDocument;
use gents::store::config_client::ConfigAccess;
use gents_desktop_core::inference::load_inference_setup_state;
use gents_shell::source::{Doc, FieldKind, Form, FormError};
use serde_json::{json, Value};

use super::{
    delete, field, form_ref, helped, hinted, items, list, merged, optional, problem, refused,
    shown, switch, tags, tags_field, value,
};
use crate::commands::config::crud::{query_collection_sorted, CONTEXT_SPEC};
use crate::commands::shell::sections;

/// The desktop's skill editor (`SkillsPanel.tsx:28-246`) on `current`, or on the stub a new skill
/// starts from (`:335-348`) with `id`. `users` are the agents whose context lists it.
pub(super) fn form(current: Option<&Value>, id: &str, users: &[String]) -> Form {
    let stub = json!({ "skill_id": id, "name": "New skill", "instructions": "", "enabled": false });
    let skill = current.unwrap_or(&stub);
    let text = |key: &str, label: &str, kind| field(key, label, kind, &shown(&skill[key]));
    let tags: Vec<String> = serde_json::from_value(skill["tags"].clone()).unwrap_or_default();
    let pack = gents::pack::pack_origin_from_tags(&tags).ok().flatten();
    let users = match users {
        [] => "no agent yet".to_string(),
        users => users.join(", "),
    };
    let tools = field(
        "tool_refs",
        "Tool dependencies",
        FieldKind::Text,
        &items(&skill["tool_refs"]).collect::<Vec<_>>().join(", "),
    );
    let instructions = text("instructions", "Instructions", FieldKind::Lines);
    let name = sections::named(&skill["name"]).unwrap_or(id).to_string();
    Form {
        form: form_ref(Doc::Skill, current, id),
        title: sections::title(Doc::Skill, skill),
        note: vec![format!(
            "Used by {users} · From {}",
            pack.unwrap_or("Your own")
        )],
        fields: vec![
            field("about", "About", FieldKind::Heading, ""),
            text("name", "Name", FieldKind::Text),
            text("display_name", "Display name", FieldKind::Text),
            helped(
                text("description", "Description", FieldKind::Lines),
                "Shown in the skills catalog.",
            ),
            hinted(
                helped(instructions, "What an agent reads when it loads the skill."),
                "No instructions yet.",
            ),
            field("details", "Details", FieldKind::Heading, ""),
            helped(
                field("skill_id", "Skill ID", FieldKind::Fixed, id),
                "Cannot be renamed after creation.",
            ),
            helped(
                text("source_directory", "Source directory", FieldKind::Text),
                "Where the skill's files live.",
            ),
            hinted(
                helped(tools, "Intersected with the agent's ceiling."),
                "Add a tool",
            ),
            helped(
                text("interface_json", "Interface JSON", FieldKind::Lines),
                "Optional structured interface metadata. Must be valid JSON.",
            ),
            tags_field(skill),
            switch("enabled", "Enabled", skill["enabled"] != false),
        ],
        actions: current
            .map(|_| delete(Doc::Skill, name))
            .into_iter()
            .collect(),
    }
}

/// The skill `values` save as `id`, over `current`, as the desktop's editor writes it
/// (`SkillsPanel.tsx:52-80`).
pub(super) fn document(
    current: Option<&Value>,
    values: &[(String, String)],
    node_did: &str,
    id: &str,
) -> Result<SkillDocument, FormError> {
    let name = value(values, "name").trim();
    if name.is_empty() {
        return Err(problem("name", "Name is required"));
    }
    let interface = value(values, "interface_json");
    if !interface.trim().is_empty() && serde_json::from_str::<Value>(interface).is_err() {
        return Err(problem(
            "interface_json",
            "Interface JSON must be valid JSON",
        ));
    }
    let fields = json!({
        "node_did": node_did,
        "skill_id": id,
        "name": name,
        "display_name": optional(value(values, "display_name")),
        "description": optional(value(values, "description")),
        "instructions": value(values, "instructions"),
        "source_directory": optional(value(values, "source_directory")),
        "tool_refs": list(value(values, "tool_refs")),
        "interface_json": optional(interface),
        "enabled": value(values, "enabled") == "on",
        "tags": tags(current, values),
    });
    merged(current, fields).map_err(refused)
}

/// The agents whose context lists `skill`, as the desktop's `skillUsers` finds them
/// (`SkillsPanel.tsx:248-253`).
pub(super) async fn users(
    access: &ConfigAccess,
    node_did: &str,
    skill: &str,
) -> anyhow::Result<Vec<String>> {
    let contexts = query_collection_sorted(access, CONTEXT_SPEC, node_did).await?;
    let agents = load_inference_setup_state(access, node_did).await?.agents;
    let lists = |context: &&Value| items(&context["skill_ids"]).any(|listed| listed == skill);
    let users = contexts.iter().filter(lists).flat_map(|context| {
        let using = context["context_id"].as_str();
        let users = agents
            .iter()
            .filter(move |agent| agent.context_id.as_deref() == using);
        users.map(|agent| agent.display_name.clone())
    });
    Ok(users.collect())
}

/// The skill ids `agent`'s context allows, as the desktop's `effectiveContextSkills`
/// reads them (`slashSkills.ts:11-23`): none for an agent with no context.
pub(in crate::commands::shell) async fn allowed(
    access: &ConfigAccess,
    node_did: &str,
    agent: &str,
) -> anyhow::Result<Vec<String>> {
    let contexts = query_collection_sorted(access, CONTEXT_SPEC, node_did).await?;
    let agents = load_inference_setup_state(access, node_did).await?.agents;
    let using = agents.iter().find(|shown| shown.agent_did == agent);
    let Some(using) = using.and_then(|agent| agent.context_id.as_deref()) else {
        return Ok(Vec::new());
    };
    let context = contexts
        .iter()
        .find(|context| context["context_id"] == using);
    let allowed = context
        .into_iter()
        .flat_map(|context| items(&context["skill_ids"]));
    Ok(allowed.map(str::to_string).collect())
}

#[cfg(test)]
mod tests {
    use gents_shell::source::{FormAct, FormAction, FormRef};
    use serde_json::json;

    use super::*;
    use crate::commands::shell::testing::{keys_of, sent, shown_values};

    #[test]
    fn skill_form_mirrors_the_desktop() {
        let skill = json!({
            "_docID": "bae-commit",
            "skill_id": "commit",
            "node_did": "owner",
            "name": "commit",
            "display_name": "Commit",
            "description": "Writes commits",
            "instructions": "Stage, then commit.\nKeep it short.",
            "source_directory": "/skills/commit",
            "tool_refs": ["git", "read"],
            "interface_json": "{\"args\":[]}",
            "enabled": null,
            "created_at": "2026-01-01T00:00:00Z",
            "tags": ["gents:pack:git", "vcs", "daily"],
        });
        let users = ["Engineer".to_string(), "Reviewer".to_string()];

        let edit = form(Some(&skill), "commit", &users);

        assert_eq!(edit.form, FormRef::Edit(Doc::Skill, "commit".to_string()));
        assert_eq!(edit.title, "Commit");
        assert_eq!(edit.note, ["Used by Engineer, Reviewer · From git"]);
        assert_eq!(
            shown_values(&edit),
            [
                ("about", ""),
                ("name", "commit"),
                ("display_name", "Commit"),
                ("description", "Writes commits"),
                ("instructions", "Stage, then commit.\nKeep it short."),
                ("details", ""),
                ("skill_id", "commit"),
                ("source_directory", "/skills/commit"),
                ("tool_refs", "git, read"),
                ("interface_json", "{\"args\":[]}"),
                ("tags", "vcs, daily"),
                ("enabled", "on"),
            ]
        );
        assert_eq!(keys_of(&edit, &FieldKind::Fixed), ["skill_id"]);
        let lines = ["description", "instructions", "interface_json"];
        assert_eq!(keys_of(&edit, &FieldKind::Lines), lines);
        let delete = FormAct::Delete {
            label: "commit".to_string(),
            name: "commit".to_string(),
        };
        let delete = FormAction {
            label: "Delete this skill".to_string(),
            act: delete,
        };
        assert_eq!(edit.actions, [delete]);

        let new = form(None, "skill-abc", &[]);

        assert_eq!(new.form, FormRef::New(Doc::Skill));
        assert_eq!(new.title, "New skill");
        assert_eq!(new.note, ["Used by no agent yet · From Your own"]);
        assert_eq!(
            shown_values(&new),
            [
                ("about", ""),
                ("name", "New skill"),
                ("display_name", ""),
                ("description", ""),
                ("instructions", ""),
                ("details", ""),
                ("skill_id", "skill-abc"),
                ("source_directory", ""),
                ("tool_refs", ""),
                ("interface_json", ""),
                ("tags", ""),
                ("enabled", "off"),
            ]
        );
        assert_eq!(new.actions, []);
    }

    #[test]
    fn a_new_skill_needs_a_name() {
        let named = |name: &str| sent(&[("name", name), ("instructions", ""), ("enabled", "off")]);

        let blank = document(None, &named("  "), "owner", "skill-a");
        let saved = document(None, &named(" review "), "owner", "skill-a");

        let required = FormError {
            field: Some("name".to_string()),
            message: "Name is required".to_string(),
        };
        assert_eq!(blank, Err(required));
        let stub = json!({
            "skill_id": "skill-a",
            "node_did": "owner",
            "name": "review",
            "instructions": "",
            "enabled": false,
        });
        assert_eq!(saved, Ok(serde_json::from_value(stub).unwrap()));
    }

    #[test]
    fn interface_json_must_parse() {
        let with = |name: &str, interface: &str| {
            sent(&[
                ("name", name),
                ("interface_json", interface),
                ("enabled", "on"),
            ])
        };

        let broken = document(None, &with("review", "{not json"), "owner", "skill-a");
        let both = document(None, &with("", "{not json"), "owner", "skill-a");
        let parsed = document(
            None,
            &with("review", " {\"args\": []} "),
            "owner",
            "skill-a",
        );
        let blank = document(None, &with("review", "  "), "owner", "skill-a");

        let invalid = FormError {
            field: Some("interface_json".to_string()),
            message: "Interface JSON must be valid JSON".to_string(),
        };
        assert_eq!(broken, Err(invalid));
        assert_eq!(both.unwrap_err().field.as_deref(), Some("name"));
        let parsed = parsed.unwrap().interface_json;
        assert_eq!(parsed.as_deref(), Some("{\"args\": []}"));
        assert_eq!(blank.unwrap().interface_json, None);
    }
}
