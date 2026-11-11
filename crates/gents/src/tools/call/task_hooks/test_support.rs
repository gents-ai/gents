use gents_lean_vocab::{LeanCommandResult, LeanTaskHook};

use super::HookCommandResult;
use crate::documents::{TaskHook, TaskHookPhase};

pub(crate) fn lean_hook_phase(name: &str) -> TaskHookPhase {
    match name {
        "before" => TaskHookPhase::Before,
        "after_success" => TaskHookPhase::AfterSuccess,
        "after_failure" => TaskHookPhase::AfterFailure,
        "finally" => TaskHookPhase::Finally,
        other => panic!("generated contract emitted an unknown hook phase {other:?}"),
    }
}

pub(crate) fn native_task_hook(generated: &LeanTaskHook) -> TaskHook {
    TaskHook {
        hook_id: generated.hook_id.clone(),
        phase: lean_hook_phase(&generated.phase),
        command: generated.command.clone(),
        timeout_secs: generated.timeout_secs,
        skip_exit_codes: generated.skip_exit_codes.clone(),
    }
}

pub(crate) fn native_command_result(generated: &LeanCommandResult) -> HookCommandResult {
    match generated {
        LeanCommandResult::Exited { code } => HookCommandResult::Exited { code: Some(*code) },
        LeanCommandResult::LaunchFailed => HookCommandResult::LaunchFailed,
        LeanCommandResult::TimedOut => HookCommandResult::TimedOut,
        LeanCommandResult::Interrupted => HookCommandResult::Interrupted,
    }
}
