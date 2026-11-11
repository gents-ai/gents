/// A Tools document can lower the budget for each presented stream, but cannot
/// exceed this provider-output ceiling. Process capture has a separate owner.
pub(crate) const MAX_CONFIGURED_COMMAND_CHARS: usize = 1_000_000;

/// Limit a configured file read so one result cannot consume unbounded model
/// context or result storage.
pub(crate) const MAX_CONFIGURED_FILE_CHARS: usize = 1_000_000;
/// Limit configured list and search results so path collections cannot consume
/// unbounded model context or result storage.
pub(crate) const MAX_CONFIGURED_FILE_RESULTS: usize = 5_000;
pub(crate) const DEFAULT_COMMAND_TIMEOUT_SECS: u64 = 120;

/// Default wait when neither the call nor the Tools group sets one; the
/// timeout reports the process as still running without cancelling it.
pub(crate) const DEFAULT_WAIT_PROCESS_TIMEOUT_SECS: u64 = 30;
/// Bound one foreground wait even when the background work continues; the
/// Tools group can set a lower maximum without stopping that work.
pub(crate) const MAX_WAIT_PROCESS_TIMEOUT_SECS: u64 = 600;

pub const DEFAULT_ACTION_TIMEOUT_SECS: u64 = 20;
/// Bound the time one language-server action can hold a runtime resource.
pub const MAX_ACTION_TIMEOUT_SECS: u64 = 300;
