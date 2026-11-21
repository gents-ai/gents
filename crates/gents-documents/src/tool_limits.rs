/// A Tools document can lower the budget for each presented stream, but cannot
/// exceed this provider-output ceiling. Process capture has a separate owner.
pub(crate) const MAX_CONFIGURED_COMMAND_CHARS: usize = 1_000_000;

/// Limit a configured file read so one result cannot consume unbounded model
/// context or result storage.
pub(crate) const MAX_CONFIGURED_FILE_CHARS: usize = 1_000_000;
/// Limit configured list and search results so path collections cannot consume
/// unbounded model context or result storage.
pub(crate) const MAX_CONFIGURED_FILE_RESULTS: usize = 5_000;
/// Workspace instruction text placed in provider context when the Tools
/// document leaves `host.max_instruction_chars` unset, so instructions cannot
/// consume the context before the request is assembled.
pub const DEFAULT_MAX_INSTRUCTION_CHARS: usize = 32 * 1024;
/// Limit configured instruction text, and what a workspace freezes at
/// creation, so one repository cannot fill provider context or the workspace
/// document.
pub const MAX_CONFIGURED_INSTRUCTION_CHARS: usize = 256 * 1024;

/// A Tools document's `host.max_instruction_chars`; unset is
/// [`DEFAULT_MAX_INSTRUCTION_CHARS`], and a value validation would reject never
/// exceeds [`MAX_CONFIGURED_INSTRUCTION_CHARS`].
pub fn resolve_instruction_chars(configured: Option<i64>) -> usize {
    configured
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(DEFAULT_MAX_INSTRUCTION_CHARS)
        .min(MAX_CONFIGURED_INSTRUCTION_CHARS)
}
pub const DEFAULT_COMMAND_TIMEOUT_SECS: u64 = 120;

/// Default wait when neither the call nor the Tools group sets one; the
/// timeout reports the process as still running without cancelling it.
pub const DEFAULT_WAIT_PROCESS_TIMEOUT_SECS: u64 = 30;
/// Bound one foreground wait even when the background work continues; the
/// Tools group can set a lower maximum without stopping that work.
pub const MAX_WAIT_PROCESS_TIMEOUT_SECS: u64 = 600;

pub const DEFAULT_ACTION_TIMEOUT_SECS: u64 = 20;
/// Bound the time one language-server action can hold a runtime resource.
pub const MAX_ACTION_TIMEOUT_SECS: u64 = 300;
