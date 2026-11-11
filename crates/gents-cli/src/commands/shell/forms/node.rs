use anyhow::Context;
use gents_desktop_core::client::{set_default_agent_on, upsert_node_on};
use gents_desktop_core::inference::{load_inference_setup_state, AgentView};
use gents_documents::Node;
use gents_shell::source::{FieldKind, Form, FormError, FormRef};
use gents_store::config_client::ConfigAccess;
use serde_json::{json, Value};

use super::{
    choice, field, helped, merged, optional, problem, refused, shown, switch, tags, tags_field,
    value,
};
use crate::commands::config::crud::{query_collection, NODE_SPEC};
use crate::commands::shell::sections;

/// The node page's form, read as the node.
pub(super) async fn open(access: &ConfigAccess, node_did: &str) -> anyhow::Result<Form> {
    let node = stored(access, node_did).await?;
    let agents = load_inference_setup_state(access, node_did).await?.agents;
    Ok(form(&node, &agents))
}

/// Saves `values` over the node as the desktop does (`AgentPanel.tsx:42-59`): a new default agent
/// is set first, which enables it, and then the node, read again, is written whole.
pub(super) async fn save(
    access: &ConfigAccess,
    node_did: &str,
    values: &[(String, String)],
) -> Result<(), FormError> {
    let before = stored(access, node_did).await.map_err(refused)?;
    document(&before, values)?;
    let default = value(values, "default_agent_did");
    if before["default_agent_did"] != default {
        let set = set_default_agent_on(access, node_did, default).await;
        set.map_err(refused)?;
    }
    let current = stored(access, node_did).await.map_err(refused)?;
    let node = document(&current, values)?;
    upsert_node_on(access, &node).await.map_err(refused)
}

async fn stored(access: &ConfigAccess, node_did: &str) -> anyhow::Result<Value> {
    let nodes = query_collection(access, NODE_SPEC, node_did, None).await?;
    let node = nodes.into_iter().next();
    node.with_context(|| format!("not found: Node {node_did:?}"))
}

/// The desktop's node page (`AgentCard.tsx:249-420`, `AgentPanel.tsx:106-194`) on `node`, whose
/// default agent is one of `agents`. Its header's name and `Disabled` badge are the note.
pub(super) fn form(node: &Value, agents: &[AgentView]) -> Form {
    let did = node["node_did"].as_str().unwrap_or_default();
    let name = sections::named(&node["display_name"]).unwrap_or(did);
    let enabled = node["enabled"] != false;
    let note = match enabled {
        true => name.to_string(),
        false => format!("{name}   Disabled"),
    };
    let text = |key: &str, label: &str| field(key, label, FieldKind::Text, &shown(&node[key]));
    let agents = agents.iter().map(|agent| {
        let name = &agent.display_name;
        let label = match agent.enabled {
            true => name.clone(),
            false => format!("{name} · disabled, enabled when saved as default"),
        };
        (agent.agent_did.clone(), label)
    });
    let default = field(
        "default_agent_did",
        "Default agent",
        choice(agents),
        &shown(&node["default_agent_did"]),
    );
    let created = node["created_at"].as_str();
    let created = created.map_or(String::new(), |at| sections::local(at, "%Y-%m-%d"));
    Form {
        form: FormRef::Node,
        title: "Node".to_string(),
        note: vec![note],
        fields: vec![
            field("naming", "Name and tags", FieldKind::Heading, ""),
            helped(
                text("display_name", "Display name"),
                "How this node is named across the desktop.",
            ),
            helped(tags_field(node), "Optional discovery labels."),
            helped(
                switch("enabled", "Enabled", enabled),
                "A disabled node accepts no requests.",
            ),
            field("details", "Node details", FieldKind::Heading, ""),
            helped(default, "Used when a session does not choose one."),
            helped(
                text("max_request_hop", "Maximum request hop"),
                "Limits how far requests can be chained. Leave blank for no limit; 0 permits only root requests.",
            ),
            field("identity", "Identity", FieldKind::Heading, ""),
            helped(
                field("node_did", "Node DID", FieldKind::Fixed, did),
                "The node's cryptographic identity. Permissions and audit are keyed by it.",
            ),
            field("created_at", "Created", FieldKind::Fixed, &created),
        ],
        actions: Vec::new(),
    }
}

/// The node `values` save over `current`, as the desktop's node form writes and checks it
/// (`AgentPanel.tsx:45-70`).
pub(super) fn document(current: &Value, values: &[(String, String)]) -> Result<Node, FormError> {
    let name = value(values, "display_name").trim();
    if name.is_empty() {
        return Err(problem("display_name", "Display name is required"));
    }
    let fields = json!({
        "display_name": name,
        "default_agent_did": optional(value(values, "default_agent_did")),
        "enabled": value(values, "enabled") == "on",
        "max_request_hop": hop(value(values, "max_request_hop"))?,
        "tags": tags(Some(current), values),
    });
    merged(Some(current), fields).map_err(refused)
}

/// Digits up to 2147483647, or blank for no limit, as the desktop checks the hop as typed.
fn hop(typed: &str) -> Result<Option<u32>, FormError> {
    if typed.trim().is_empty() {
        return Ok(None);
    }
    let digits = typed.bytes().all(|byte| byte.is_ascii_digit());
    match typed.parse() {
        Ok(hop) if digits && hop <= 2_147_483_647 => Ok(Some(hop)),
        _ => Err(problem(
            "max_request_hop",
            "Enter a whole number from 0 to 2147483647, or leave blank.",
        )),
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Local};

    use super::*;
    use crate::commands::shell::testing::{agent, keys_of, options, sent, shown_values};

    #[test]
    fn node_form_mirrors_the_desktop() {
        let node = json!({
            "_docID": "bae-node",
            "node_did": "did:key:zNode",
            "display_name": "Studio",
            "avatar": "7",
            "default_agent_did": "engineer",
            "enabled": false,
            "created_at": "2026-01-01T12:00:00Z",
            "created_by": "did:key:zMaker",
            "max_request_hop": 3,
            "tags": ["gents:pack:home", "desk"],
        });
        let agents = [
            agent("engineer", "Engineer", true, false),
            agent("scout", "Scout", false, false),
        ];

        let shown = form(&node, &agents);

        assert_eq!(shown.form, FormRef::Node);
        assert_eq!(shown.title, "Node");
        assert_eq!(shown.note, ["Studio   Disabled"]);
        let created = DateTime::parse_from_rfc3339("2026-01-01T12:00:00Z").unwrap();
        let created = created.with_timezone(&Local).format("%Y-%m-%d").to_string();
        assert_eq!(
            shown_values(&shown),
            [
                ("naming", ""),
                ("display_name", "Studio"),
                ("tags", "desk"),
                ("enabled", "off"),
                ("details", ""),
                ("default_agent_did", "engineer"),
                ("max_request_hop", "3"),
                ("identity", ""),
                ("node_did", "did:key:zNode"),
                ("created_at", created.as_str()),
            ]
        );
        let fixed = ["node_did", "created_at"];
        assert_eq!(keys_of(&shown, &FieldKind::Fixed), fixed);
        assert!(keys_of(&shown, &FieldKind::Lines).is_empty());
        let agents = [
            ("engineer", "Engineer"),
            ("scout", "Scout · disabled, enabled when saved as default"),
        ];
        assert_eq!(options(&shown, "default_agent_did"), agents);
        assert_eq!(shown.actions, []);

        let bare = json!({ "node_did": "did:key:zNode", "default_agent_did": "engineer" });
        let bare = form(&bare, &[]);
        assert_eq!(bare.note, ["did:key:zNode"]);
        let enabled = bare.fields.iter().find(|field| field.key == "enabled");
        assert_eq!(enabled.unwrap().value, "on");
    }

    #[test]
    fn a_request_hop_is_whole_or_blank() {
        let node = json!({ "node_did": "did:key:zNode", "default_agent_did": "engineer" });
        let typed = |name: &str, hop: &str| {
            sent(&[
                ("display_name", name),
                ("enabled", "on"),
                ("default_agent_did", "engineer"),
                ("max_request_hop", hop),
            ])
        };
        let hop = |typed_hop: &str| {
            document(&node, &typed("Studio", typed_hop)).map(|node| node.max_request_hop)
        };

        let refused = Err(FormError {
            field: Some("max_request_hop".to_string()),
            message: "Enter a whole number from 0 to 2147483647, or leave blank.".to_string(),
        });
        assert_eq!(hop(""), Ok(None));
        assert_eq!(hop("  "), Ok(None));
        assert_eq!(hop("0"), Ok(Some(0)));
        assert_eq!(hop("2147483647"), Ok(Some(2_147_483_647)));
        assert_eq!(hop("2147483648"), refused);
        assert_eq!(hop("-1"), refused);
        assert_eq!(hop("+1"), refused);
        assert_eq!(hop("1.5"), refused);
        let unnamed = document(&node, &typed("  ", "")).map(|node| node.display_name);
        let required = FormError {
            field: Some("display_name".to_string()),
            message: "Display name is required".to_string(),
        };
        assert_eq!(unnamed, Err(required));
        let named = document(&node, &typed(" Studio ", "")).map(|node| node.display_name);
        assert_eq!(named, Ok(Some("Studio".to_string())));
    }
}
