/-!
# Native spawn fallback

Every production child-process spawn tries posix_spawn first and takes
std's fork+exec path at most once, only after macOS reported the
posix_spawn descriptor bound. Rust owner:
`crates/gents/src/tools/exec/native_spawn.rs` (`spawn_with_fork_fallback`,
decision `fork_fallback_applies`).
-/

namespace NativeSpawn

/-- Host platform class: only macOS has the posix_spawn descriptor bound. -/
inductive Platform where
  | macos
  | other
  deriving DecidableEq, Repr

/-- Spawn error class as observed by the decision. -/
inductive SpawnError where
  /-- EBADF from the spawn call. -/
  | badDescriptor
  /-- Any other spawn error (for example ENOENT). -/
  | other
  deriving DecidableEq, Repr

/-- Result of one spawn attempt, supplied by the environment. -/
inductive Outcome where
  | ok
  | err (error : SpawnError)
  deriving DecidableEq, Repr

/-- Process-creation path of one attempt. -/
inductive SpawnPath where
  /-- posix_spawn: the default; creates no child when it fails. -/
  | posixSpawn
  /-- fork followed by exec, selected by installing a no-op pre-exec hook. -/
  | forkExec
  deriving DecidableEq, Repr

/-- External premise: on macOS, `posix_spawn_file_actions_adddup2` fails with
EBADF for a source descriptor at or above OPEN_MAX (10240). The CLI raises
RLIMIT_NOFILE above that bound for embedded DefraDB, so piped or null stdio on
a high descriptor fails before exec, while fork+exec has no such bound. No
other platform or error is known to be cured by fork+exec, so only this pair
admits the fallback. -/
def forkFallbackApplies : Platform → SpawnError → Bool
  | .macos, .badDescriptor => true
  | _, _ => false

/-- External premise: in a heavily threaded process with a large heap, fork can
stall in macOS malloc's fork handlers (`_malloc_fork_prepare` takes every zone
lock while other threads hold or wait on them), and the forked child retains
unrelated pipe writers until exec. Fork is therefore never the first path; this
is the regression an always-fork spawner exhibited. -/
def firstPath : SpawnPath := .posixSpawn

/-- One spawn: `first` is the outcome of the posix_spawn attempt and `retry` the
outcome the fork+exec attempt would have. Returns the attempted paths in order
and the result returned to the caller. Posix_spawn creates no child on failure,
so the retry cannot duplicate one. -/
def run (platform : Platform) (first retry : Outcome) : List SpawnPath × Outcome :=
  match first with
  | .err e =>
    if forkFallbackApplies platform e then ([firstPath, .forkExec], retry)
    else ([firstPath], first)
  | .ok => ([firstPath], first)

theorem at_most_two_attempts (p : Platform) (f r : Outcome) :
    (run p f r).1.length ≤ 2 := by
  cases p <;> cases f with
  | ok => simp [run]
  | err e => cases e <;> simp [run, forkFallbackApplies]

theorem first_attempt_is_posix_spawn (p : Platform) (f r : Outcome) :
    (run p f r).1.head? = some .posixSpawn := by
  cases p <;> cases f with
  | ok => simp [run, firstPath]
  | err e => cases e <;> simp [run, forkFallbackApplies, firstPath]

theorem fork_only_after_descriptor_bound (p : Platform) (f r : Outcome)
    (h : SpawnPath.forkExec ∈ (run p f r).1) :
    p = .macos ∧ f = .err .badDescriptor := by
  cases p <;> cases f with
  | ok => simp [run, firstPath] at h
  | err e => cases e <;> simp_all [run, forkFallbackApplies, firstPath]

theorem other_errors_return_without_retry (p : Platform) (e : SpawnError) (r : Outcome)
    (h : forkFallbackApplies p e = false) :
    run p (.err e) r = ([.posixSpawn], .err e) := by
  simp [run, h, firstPath]

theorem default_path_never_forks (p : Platform) (f r : Outcome)
    (h : f ≠ .err .badDescriptor) :
    (run p f r).1 = [.posixSpawn] := by
  cases f with
  | ok => simp [run, firstPath]
  | err e =>
    cases e with
    | badDescriptor => exact absurd rfl h
    | other => cases p <;> simp [run, forkFallbackApplies, firstPath]

theorem no_fallback_off_macos (f r : Outcome) :
    run .other f r = ([.posixSpawn], f) := by
  cases f with
  | ok => simp [run, firstPath]
  | err e => cases e <;> simp [run, forkFallbackApplies, firstPath]

theorem descriptor_bound_retries_on_fork_path (r : Outcome) :
    run .macos (.err .badDescriptor) r = ([.posixSpawn, .forkExec], r) := by
  simp [run, forkFallbackApplies, firstPath]

end NativeSpawn
