use gents::documents::ToolServiceRegistry;
use gents_desktop_core::tool_service::{test_tool_service, ToolServiceTarget};
use gents_shell::sample::Tone;
use gents_shell::source::{Doc, FieldKind, Form, FormAct, FormAction, FormError};
use serde_json::{json, Value};

use super::{
    delete, field, form_ref, helped, merged, optional, problem, refused, shown, switch, tags,
    tags_field, value, whole,
};
use crate::commands::shell::sections;

const PORT: &str = "MCP port";

/// The desktop's tool service editor (`ToolServicesPanel.tsx:23-210`) on `current`, or on the
/// stub a new service starts from (`:250-261`) with `id`. Its connection can be tested before it
/// is saved.
pub(super) fn form(current: Option<&Value>, id: &str) -> Form {
    let stub = json!({
        "service_id": id,
        "display_name": "New service",
        "hostname": "127.0.0.1",
        "mcp_port": 3333,
        "enabled": false,
    });
    let service = current.unwrap_or(&stub);
    let text = |key: &str, label: &str, kind| field(key, label, kind, &shown(&service[key]));
    let path = helped(
        text("mcp_path", "MCP path", FieldKind::Text),
        "Empty uses the endpoint root; otherwise start with /.",
    );
    let title = sections::title(Doc::RemoteTool, service);
    let mut actions = vec![FormAction {
        label: "Test connection".to_string(),
        act: FormAct::TestConnection,
    }];
    if current.is_some() {
        actions.push(delete(Doc::RemoteTool, title.clone()));
    }
    Form {
        form: form_ref(Doc::RemoteTool, current, id),
        title,
        note: Vec::new(),
        fields: vec![
            field("service_id", "Service ID", FieldKind::Fixed, id),
            text("display_name", "Display name", FieldKind::Text),
            text("description", "Description", FieldKind::Lines),
            text("hostname", "Hostname", FieldKind::Text),
            text("tailscale_ip", "Tailscale IP", FieldKind::Text),
            text("lan_ip", "LAN IP", FieldKind::Text),
            text("mcp_port", PORT, FieldKind::Text),
            path,
            switch(
                "send_node_did",
                "Send node DID",
                service["send_node_did"] == true,
            ),
            switch("enabled", "Enabled", service["enabled"] != false),
            tags_field(service),
        ],
        actions,
    }
}

/// The service `values` save as `id`, over `current`, as the desktop's editor writes it
/// (`ToolServicesPanel.tsx:69-90`).
pub(super) fn document(
    current: Option<&Value>,
    values: &[(String, String)],
    node_did: &str,
    id: &str,
) -> Result<ToolServiceRegistry, FormError> {
    let target = target(values, id)?;
    let fields = json!({
        "node_did": node_did,
        "service_id": id,
        "display_name": optional(value(values, "display_name")),
        "description": optional(value(values, "description")),
        "hostname": target.hostname,
        "tailscale_ip": target.tailscale_ip,
        "lan_ip": target.lan_ip,
        "mcp_port": target.mcp_port,
        "mcp_path": target.mcp_path,
        "send_node_did": value(values, "send_node_did") == "on",
        "enabled": value(values, "enabled") == "on",
        "tags": tags(current, values),
    });
    merged(current, fields).map_err(refused)
}

/// Tests the endpoint `values` name for the service `id`, with the desktop's toasts
/// (`ToolServicesPanel.tsx:91-111`); the bridge's status for a test that answers is "ok"
/// (`gents-desktop-bridge/src/commands/tool_service.rs:36`).
pub(super) async fn test(values: &[(String, String)], id: &str) -> (String, Tone) {
    let tested = match target(values, id) {
        Ok(target) => test_tool_service(&target)
            .await
            .map_err(|error| format!("{error:#}")),
        Err(problem) => Err(problem.message),
    };
    match tested {
        Ok(test) => (format!("ok · {} tools", test.tools.len()), Tone::Accent),
        Err(error) => (format!("Test failed: {error}"), Tone::Warn),
    }
}

/// The endpoint `values` name, as the desktop's `endpointOf` reads it, with the checks its owner
/// leaves to the form (`ToolServicesPanel.tsx:50-68`, `draft.ts:133-147`).
fn target(values: &[(String, String)], id: &str) -> Result<ToolServiceTarget, FormError> {
    let port = whole(values, "mcp_port", PORT)?;
    let bound = match port {
        Some(port) if port < 1 => Some("1 or more"),
        Some(port) if port > 65_535 => Some("65535 or less"),
        _ => None,
    };
    if let Some(bound) = bound {
        return Err(problem("mcp_port", &format!("{PORT} must be {bound}")));
    }
    let path = value(values, "mcp_path");
    if !path.is_empty() && !path.starts_with('/') {
        return Err(problem(
            "mcp_path",
            "MCP path must be empty or start with /",
        ));
    }
    let text = |key: &str| {
        let typed = Some(value(values, key).trim()).filter(|typed| !typed.is_empty());
        typed.map(str::to_string)
    };
    Ok(ToolServiceTarget {
        service_id: id.to_string(),
        hostname: text("hostname"),
        tailscale_ip: text("tailscale_ip"),
        lan_ip: text("lan_ip"),
        mcp_port: port,
        mcp_path: text("mcp_path"),
    })
}

#[cfg(test)]
mod tests {
    use gents_shell::source::FormRef;

    use super::*;
    use crate::commands::shell::testing::{keys_of, sent, shown_values};

    #[test]
    fn remote_tool_form_mirrors_the_desktop() {
        let files = json!({
            "_docID": "bae-files",
            "service_id": "files",
            "node_did": "owner",
            "display_name": "Files",
            "description": "Reads files",
            "hostname": "files.local",
            "tailscale_ip": "100.64.0.2",
            "lan_ip": null,
            "mcp_port": 8080,
            "mcp_path": "/mcp",
            "send_node_did": true,
            "enabled": null,
            "tags": ["gents:pack:fs", "local"],
        });

        let edit = form(Some(&files), "files");
        let new = form(None, "mcp-abc");

        assert_eq!(
            edit.form,
            FormRef::Edit(Doc::RemoteTool, "files".to_string())
        );
        assert_eq!(edit.title, "Files");
        assert_eq!(edit.note, Vec::<String>::new());
        assert_eq!(
            shown_values(&edit),
            [
                ("service_id", "files"),
                ("display_name", "Files"),
                ("description", "Reads files"),
                ("hostname", "files.local"),
                ("tailscale_ip", "100.64.0.2"),
                ("lan_ip", ""),
                ("mcp_port", "8080"),
                ("mcp_path", "/mcp"),
                ("send_node_did", "on"),
                ("enabled", "on"),
                ("tags", "local"),
            ]
        );
        assert_eq!(keys_of(&edit, &FieldKind::Fixed), ["service_id"]);
        assert_eq!(keys_of(&edit, &FieldKind::Lines), ["description"]);
        let test = FormAction {
            label: "Test connection".to_string(),
            act: FormAct::TestConnection,
        };
        let delete = FormAction {
            label: "Delete this tool service".to_string(),
            act: FormAct::Delete {
                label: "Files".to_string(),
                name: "Files".to_string(),
            },
        };
        assert_eq!(edit.actions, [test.clone(), delete]);
        let saved = document(Some(&files), &sent(&shown_values(&edit)), "owner", "files");
        let mut stored = files.clone();
        stored.as_object_mut().unwrap().remove("_docID");
        assert_eq!(saved, Ok(serde_json::from_value(stored).unwrap()));

        assert_eq!(new.form, FormRef::New(Doc::RemoteTool));
        assert_eq!(new.title, "New service");
        assert_eq!(
            shown_values(&new),
            [
                ("service_id", "mcp-abc"),
                ("display_name", "New service"),
                ("description", ""),
                ("hostname", "127.0.0.1"),
                ("tailscale_ip", ""),
                ("lan_ip", ""),
                ("mcp_port", "3333"),
                ("mcp_path", ""),
                ("send_node_did", "off"),
                ("enabled", "off"),
                ("tags", ""),
            ]
        );
        assert_eq!(new.actions, [test]);
    }

    #[test]
    fn an_mcp_path_starts_with_a_slash() {
        let at = |path: &str| {
            sent(&[
                ("hostname", "files.local"),
                ("mcp_port", "8080"),
                ("mcp_path", path),
            ])
        };
        let saved = |path: &str| document(None, &at(path), "owner", "mcp-a");

        let refused = Err(FormError {
            field: Some("mcp_path".to_string()),
            message: "MCP path must be empty or start with /".to_string(),
        });
        assert_eq!(saved("mcp"), refused);
        assert_eq!(
            saved(" /mcp"),
            refused,
            "the desktop checks the path as typed"
        );
        let path = |typed: &str| saved(typed).map(|service| service.mcp_path);
        assert_eq!(path("/mcp "), Ok(Some("/mcp".to_string())));
        assert_eq!(path(""), Ok(None));
    }

    #[test]
    fn a_port_above_65535_is_refused() {
        let on = |port: &str| sent(&[("hostname", "files.local"), ("mcp_port", port)]);
        let port = |typed: &str| {
            document(None, &on(typed), "owner", "mcp-a").map(|service| service.mcp_port)
        };

        let refused = |message: &str| {
            Err(FormError {
                field: Some("mcp_port".to_string()),
                message: message.to_string(),
            })
        };
        assert_eq!(port("65536"), refused("MCP port must be 65535 or less"));
        assert_eq!(port("0"), refused("MCP port must be 1 or more"));
        assert_eq!(port("http"), refused("MCP port must be a whole number"));
        assert_eq!(port(" 65535 "), Ok(Some(65535)));
        assert_eq!(port(""), Ok(None), "the owner says a port is required");
    }
}
