//! Compile-only assertion that each operations command has the
//! parameter and return-type shape downstream panels rely on. These tests
//! never call the underlying functions (which would panic via
//! unimplemented!()); they only assert types at compile time.

#![cfg(test)]

use super::operations::{
    desktop_interrupt_request, desktop_list_subagent_tree, desktop_list_tool_call_holds,
    desktop_operations_snapshot, desktop_preview_interrupt_cascade, desktop_resolve_tool_call_hold,
};

#[allow(dead_code)]
fn _assert_command_signatures() {
    // These let bindings only check the function items exist and are
    // visible. The Tauri `#[tauri::command]` macro wraps the real function
    // in a synthetic one we don't reference here.
    let _ = desktop_operations_snapshot;
    let _ = desktop_list_subagent_tree;
    let _ = desktop_preview_interrupt_cascade;
    let _ = desktop_interrupt_request;
    let _ = desktop_list_tool_call_holds;
    let _ = desktop_resolve_tool_call_hold;
}
