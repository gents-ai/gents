use gents_lean_vocab::families;
use gents_protocol::operations::document::{FieldAfter, FieldUpdate};
use serde_json::{json, Map, Value};

use super::decide_owner;
use super::mcp_service::{
    defaults_path, enabled_after_register, endpoint_storage, send_node_did_after_register,
};
use super::skill::decide_import;
use super::write::decide_write;
use crate::patch::{apply_patch, SelfConfigTarget};

#[test]
fn a_patched_field_matches_lean() {
    families::document_operations_field_after::replay(|i| {
        let mut stored = Map::new();
        if i.stored_present {
            stored.insert("name".to_owned(), json!("stored"));
        }
        let patch = match i.update {
            FieldUpdate::Omitted => Vec::new(),
            FieldUpdate::Clear => vec![("name".to_owned(), None)],
            FieldUpdate::Set => vec![("name".to_owned(), Some(json!("requested")))],
        };
        let merged = apply_patch(SelfConfigTarget::Skill, &stored, &patch);
        match merged.get("name").and_then(Value::as_str) {
            None => FieldAfter::Absent,
            Some("stored") => FieldAfter::Stored,
            Some(_) => FieldAfter::Requested,
        }
    });
}

#[test]
fn write_admission_matches_lean() {
    families::document_operations_write_outcome::replay(|i| decide_write(i.mode, i.stored));
}

#[test]
fn owner_resolution_matches_lean() {
    families::document_operations_owner_resolution::replay(|i| {
        decide_owner(i.owner_named, i.owners)
    });
}

#[test]
fn skill_import_admission_matches_lean() {
    families::document_operations_import_outcome::replay(|i| {
        decide_import(i.actor, i.inside_tool_root, i.stored, i.overwrite)
    });
}

#[test]
fn mcp_registration_matches_lean() {
    families::document_operations_endpoint_storage::replay(|i| endpoint_storage(i.scheme, i.host));
    families::document_operations_defaults_path::replay(|i| {
        defaults_path(i.scheme, i.path_is_root)
    });
    families::document_operations_enabled_after_register::replay(|i| {
        enabled_after_register(i.stored, i.requested)
    });
    families::document_operations_send_node_did_after_register::replay(|i| {
        send_node_did_after_register(i.stored, i.requested)
    });
}
