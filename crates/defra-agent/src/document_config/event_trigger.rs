use serde::{Deserialize, Serialize};

/// Placeholder for an event-driven trigger.
///
/// The real `EventTrigger` schema and fields land in PR 2 of the
/// event-driven-tasks series. Defining an empty struct here lets PR 1 extend
/// `DocumentRuntimeView` with a stub `event_triggers` map so that PR 2 can
/// populate it without a breaking-change diff to the view type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct EventTrigger {}
