//! Built-in permission preset templates and exact-match classifier
//! (directory persona catalog, PR 2 / Task 2).
//!
//! A "preset" is a named bundle of the permission fields the persona layer
//! CLASSIFIES on — enough to distinguish `readonly` from `write` from
//! hand-tuned ("custom") selections, not enough to fully MINT one. Root (a
//! filesystem dimension, not a permission) and `display_name` are excluded
//! on principle. Also deliberately excluded, though they DO vary across
//! init's packages: execution policy, background tool settings,
//! `enable_meta_tools`, and `enable_defra_query`. A materializer that mints
//! a canonical `Tools` document from a preset name must source those
//! init-parity extras from the package profile separately. `PresetFields`
//! alone under-provisions a write configuration. Conversely, a hand-tuned change to one of
//! the excluded fields keeps its preset badge: the classifier is a
//! permissions label, not a byte-identity check over the whole document.
//!
//! The template values here are copied **verbatim** from the authoritative
//! source in `crates/gents-cli/src/commands/init.rs`:
//! `tool_package_profile` (Readonly/Write arms, ~line 706-751) for
//! `enable_file_tools` / `file_tools_mode` / `enable_bash` / `bash_mode`, and
//! `default_command_execution_policy_for_init` (~line 689) confirms these
//! packages don't set argv prefixes or a custom read-only allowlist (both
//! empty). `init.rs` is deliberately left untouched by this change — see the
//! plan's deviation note.

pub const PRESET_READONLY: &str = "readonly";
pub const PRESET_WRITE: &str = "write";

/// All built-in preset names, in a stable order.
pub fn builtin_preset_names() -> &'static [&'static str] {
    &[PRESET_READONLY, PRESET_WRITE]
}

/// The discriminating permission fields of a canonical `Tools` document —
/// everything a preset determines. Root is deliberately absent (a
/// dimension, not a permission).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PresetFields {
    pub enable_file_tools: bool,
    pub file_tools_mode: String,
    pub enable_bash: bool,
    pub bash_mode: String,
    pub command_allowed_argv_prefixes: Vec<String>,
    pub command_forbidden_argv_prefixes: Vec<String>,
    pub read_only_command_allowlist: Vec<String>,
    pub enable_self_config: bool,
    pub write_tools: Vec<String>,
}

/// Template for a built-in preset; `None` for unknown names.
pub fn preset_fields(name: &str) -> Option<PresetFields> {
    match name {
        PRESET_READONLY => Some(PresetFields {
            enable_file_tools: true,
            file_tools_mode: "ReadOnly".to_string(),
            enable_bash: true,
            bash_mode: "ReadOnly".to_string(),
            command_allowed_argv_prefixes: Vec::new(),
            command_forbidden_argv_prefixes: Vec::new(),
            read_only_command_allowlist: Vec::new(),
            enable_self_config: false,
            write_tools: Vec::new(),
        }),
        PRESET_WRITE => Some(PresetFields {
            enable_file_tools: true,
            file_tools_mode: "ReadWrite".to_string(),
            enable_bash: true,
            bash_mode: "Unrestricted".to_string(),
            command_allowed_argv_prefixes: Vec::new(),
            command_forbidden_argv_prefixes: Vec::new(),
            read_only_command_allowlist: Vec::new(),
            enable_self_config: false,
            write_tools: Vec::new(),
        }),
        _ => None,
    }
}

/// `Some(name)` iff `fields` exactly equals a built-in template.
pub fn preset_name(fields: &PresetFields) -> Option<&'static str> {
    builtin_preset_names()
        .iter()
        .copied()
        .find(|name| preset_fields(name).as_ref() == Some(fields))
}

/// Classify canonical tools after the common datastore surface resolver has
/// expanded selected declarations. Callers keep document loading and scope checks.
pub fn classify_tools(
    tools: &crate::document_config::Tools,
    merged: &crate::document_config::MergedSurfaceTools,
) -> anyhow::Result<Option<&'static str>> {
    use crate::tool_surface::{BashMode, FileToolMode};
    let host = tools.host.as_ref();
    let files = host
        .and_then(|h| h.files.as_ref())
        .map(|f| f.mode)
        .unwrap_or_default();
    let bash = host.and_then(|h| h.bash.as_ref());
    let mode = bash.map(|b| b.mode).unwrap_or_default();
    let prefixes = |values: Option<&Vec<Vec<String>>>| -> anyhow::Result<Vec<String>> {
        values
            .into_iter()
            .flatten()
            .map(|v| serde_json::to_string(v).map_err(Into::into))
            .collect()
    };
    Ok(preset_name(&PresetFields {
        enable_file_tools: files != FileToolMode::Off,
        file_tools_mode: format!("{files:?}"),
        enable_bash: mode != BashMode::Off,
        bash_mode: format!("{mode:?}"),
        command_allowed_argv_prefixes: prefixes(
            bash.and_then(|b| b.allowed_argv_prefixes.as_ref()),
        )?,
        command_forbidden_argv_prefixes: prefixes(
            bash.and_then(|b| b.forbidden_argv_prefixes.as_ref()),
        )?,
        read_only_command_allowlist: bash
            .and_then(|b| b.read_only_commands.clone())
            .unwrap_or_default(),
        enable_self_config: tools
            .self_config
            .as_ref()
            .and_then(|c| c.enable_self_config)
            .unwrap_or(false),
        write_tools: merged
            .write_tools
            .iter()
            .map(|tool| tool.tool_name.clone())
            .collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_classifier_includes_expanded_datastore_writes() {
        let tools: crate::document_config::Tools = serde_json::from_value(serde_json::json!({
            "agent_did":"owner", "tools_id":"tools", "host":{
                "files":{"mode":"ReadWrite"}, "bash":{"mode":"Unrestricted"}
            }
        }))
        .unwrap();
        let mut merged = crate::document_config::MergedSurfaceTools::default();
        assert_eq!(classify_tools(&tools, &merged).unwrap(), Some(PRESET_WRITE));
        merged
            .write_tools
            .push(crate::document_config::WriteToolDecl {
                tool_name: "record_finding".into(),
                collection: "Finding".into(),
                description: "Record a finding".into(),
                fields: vec![],
                output_obligation: None,
            });
        assert_eq!(classify_tools(&tools, &merged).unwrap(), None);
    }

    #[test]
    fn readonly_mirrors_init_readonly_package() {
        let fields = preset_fields(PRESET_READONLY).expect("readonly preset should exist");
        assert_eq!(
            fields,
            PresetFields {
                enable_file_tools: true,
                file_tools_mode: "ReadOnly".to_string(),
                enable_bash: true,
                bash_mode: "ReadOnly".to_string(),
                command_allowed_argv_prefixes: Vec::new(),
                command_forbidden_argv_prefixes: Vec::new(),
                read_only_command_allowlist: Vec::new(),
                enable_self_config: false,
                write_tools: Vec::new(),
            }
        );
    }

    #[test]
    fn write_mirrors_init_write_package() {
        let fields = preset_fields(PRESET_WRITE).expect("write preset should exist");
        assert_eq!(
            fields,
            PresetFields {
                enable_file_tools: true,
                file_tools_mode: "ReadWrite".to_string(),
                enable_bash: true,
                bash_mode: "Unrestricted".to_string(),
                command_allowed_argv_prefixes: Vec::new(),
                command_forbidden_argv_prefixes: Vec::new(),
                read_only_command_allowlist: Vec::new(),
                enable_self_config: false,
                write_tools: Vec::new(),
            }
        );
    }

    #[test]
    fn unknown_name_returns_none() {
        assert_eq!(preset_fields("bogus"), None);
        assert_eq!(preset_fields(""), None);
    }

    #[test]
    fn round_trips_through_preset_name() {
        for name in builtin_preset_names() {
            let fields = preset_fields(name).expect("builtin preset must resolve");
            assert_eq!(preset_name(&fields), Some(*name));
        }
    }

    #[test]
    fn one_extra_argv_prefix_classifies_as_custom() {
        let mut fields = preset_fields(PRESET_READONLY).expect("readonly preset should exist");
        fields
            .command_allowed_argv_prefixes
            .push("git status".to_string());
        assert_eq!(preset_name(&fields), None);
    }

    #[test]
    fn one_flipped_field_classifies_as_custom() {
        let mut fields = preset_fields(PRESET_WRITE).expect("write preset should exist");
        fields.enable_self_config = true;
        assert_eq!(preset_name(&fields), None);
    }
}
