use crate::agent::tool_selection_from_document;
use crate::document_config::Tools;
use crate::toolset::CommandExecutionMode;

fn tools_doc(bash_mode: &str) -> Tools {
    serde_json::from_value(serde_json::json!({
        "tools_id": "tools", "agent_did": "did:key:zAgent",
        "host": {"bash": {"mode": bash_mode}}
    }))
    .unwrap()
}

#[test]
fn unrestricted_bash_mode_defaults_to_unrestricted_command_policy() {
    let selection = tool_selection_from_document(&tools_doc("Unrestricted")).unwrap();

    assert_eq!(
        selection.command_policy.unwrap().mode,
        CommandExecutionMode::Unrestricted
    );
}

#[test]
fn unrestricted_bash_mode_can_request_workspace_write_command_policy() {
    let mut doc = tools_doc("Unrestricted");
    doc.host
        .as_mut()
        .unwrap()
        .bash
        .as_mut()
        .unwrap()
        .execution_mode = Some(CommandExecutionMode::WorkspaceWrite);

    let selection = tool_selection_from_document(&doc).unwrap();

    assert_eq!(
        selection.command_policy.unwrap().mode,
        CommandExecutionMode::WorkspaceWrite
    );
}

#[test]
fn readonly_bash_mode_uses_builder_default_policy() {
    let selection = tool_selection_from_document(&tools_doc("ReadOnly")).unwrap();

    assert!(selection.command_policy.is_none());
}

#[test]
fn readonly_bash_mode_custom_allowlist_flows_through_to_policy() {
    let mut doc = tools_doc("ReadOnly");
    doc.host
        .as_mut()
        .unwrap()
        .bash
        .as_mut()
        .unwrap()
        .read_only_commands = Some(vec!["cat".to_string(), "journalctl".to_string()]);

    let selection = tool_selection_from_document(&doc).unwrap();

    let policy = selection
        .command_policy
        .expect("a non-empty custom read-only allowlist must materialize a Some(command_policy)");
    assert_eq!(policy.mode, CommandExecutionMode::ReadOnly);
    assert_eq!(
        policy.read_only_allowlist(),
        ["cat".to_string(), "journalctl".to_string()]
    );
}

#[test]
fn readonly_bash_mode_empty_allowlist_falls_back_to_default() {
    // An explicitly-empty allowlist must behave identically to absent: no
    // override, so command_policy stays None and the builder applies the
    // hardcoded default_read_only_commands() list (never a deny-all surface).
    let mut doc = tools_doc("ReadOnly");
    doc.host
        .as_mut()
        .unwrap()
        .bash
        .as_mut()
        .unwrap()
        .read_only_commands = Some(Vec::new());

    let selection = tool_selection_from_document(&doc).unwrap();

    assert!(selection.command_policy.is_none());
}
