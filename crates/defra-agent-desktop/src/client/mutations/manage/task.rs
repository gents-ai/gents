//! Task and Schedule mutation stubs.
//!
//! Task 51 retargets the manage-list view to the new `Task` and
//! `Schedule` collections. Task 52 will replace these stubs with real
//! upsert mutations against those collections.
//!
//! Leaving the functions present (and returning a clear error) keeps
//! the call graph stable through `ClientCore::save_task` /
//! `ClientCore::save_schedule` while we land the list-view work in a
//! separate commit.

use anyhow::{bail, Result};
use defra_agent_protocol::row::{ScheduleRow, TaskRow};
use defra_node::EmbeddedNode;

pub async fn upsert_task(_node: &EmbeddedNode, _row: &TaskRow) -> Result<()> {
    bail!("Task mutations are not yet wired up in the desktop; landing in Task 52");
}

pub async fn upsert_schedule(_node: &EmbeddedNode, _row: &ScheduleRow) -> Result<()> {
    bail!("Schedule mutations are not yet wired up in the desktop; landing in Task 52");
}

pub async fn fire_schedule_now(_node: &EmbeddedNode, _row: &ScheduleRow) -> Result<()> {
    bail!("Schedule fire-now is not yet wired up in the desktop; landing in Task 52");
}
