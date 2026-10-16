#![cfg(test)]

use super::operations::{
    desktop_interrupt_request, desktop_operations_snapshot, desktop_session_provenance,
};

#[allow(dead_code)]
fn _assert_command_signatures() {
    let _ = desktop_operations_snapshot;
    let _ = desktop_session_provenance;
    let _ = desktop_interrupt_request;
}
