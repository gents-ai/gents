//! Built-in permission preset templates and exact-match classifier
//! (directory persona catalog, PR 2 / Task 2).
//!
//! A "preset" is a named bundle of the permission fields the persona layer
//! CLASSIFIES on — enough to distinguish `readonly` from `write` from
//! hand-tuned ("custom") selections, not enough to fully MINT one. Root (a
//! filesystem dimension, not a permission) and `display_name` are excluded
//! on principle. Also deliberately excluded, though they DO vary across
//! init's packages: `command_execution_policy`, `backgroundable_tool_names`,
//! `enable_meta_tools`, and `enable_defra_query`. A materializer that mints
//! a `ToolSelectionDocument` from a preset name must source those
//! init-parity extras from `init.rs`'s package profiles separately —
//! `PresetFields` alone under-provisions a `write` selection (missing exec
//! policy + backgroundable bash). Conversely, a hand-tuned change to one of
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

/// The discriminating permission fields of a `ToolSelectionDocument` —
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

#[cfg(test)]
mod tests {
    use super::*;

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
