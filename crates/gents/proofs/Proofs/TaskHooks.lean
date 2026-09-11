import Proofs.ConfigDefaults
import Proofs.ManagedExec.State
import Proofs.Request.State

namespace TaskHooks
/-! # Task hook-phase semantics (spec PR #1430)
Executable model: phases `before`, `afterSuccess`, `afterFailure`, `finally`
around one agent execution. Literal argv hooks, optional positive timeout
(default 120); nonpositive/negative timeouts (`Option Int`) are rejected at
admission, as are empty argv and duplicate occurrence IDs, before any hook or
agent step. Ordinary phases stop at their first error; `runFinally` attempts every cleanup hook and records each attempt, so
cleanup errors are secondary while the primary error (hook id, agent, or
interruption) is preserved. Interruption is representable
(`CommandResult.interrupted`, `TaskOutcome.interrupted`, mapped to the
existing terminal `RequestState.interrupted`): ordinary outcome phases are
skipped, `finally` still runs, and no unknown-outcome command is re-executed.
Boundaries: `HookExec` and the agent result are observations; cwd/env/output capture and
process management stay with existing host execution and request completion
owners. Recovery uses observed attempts from those owners to select remaining
cleanup; this is not an exactly-once guarantee for unobserved host effects. No
persisted TaskRun, new action journal, or cwd/template schema. -/
inductive HookPhase where | before | afterSuccess | afterFailure | finally
  deriving DecidableEq, Repr
/-- Task hook: stable id, phase, literal argv, optional timeout in seconds. -/
structure TaskHook where
  hookId : String
  phase : HookPhase
  command : List String
  timeoutSecs : Option Int := none
  deriving DecidableEq, Repr
def defaultHookTimeoutSecs : Nat := 120
/-- Effective timeout: configured value, else the 120s executor default. -/
def TaskHook.effectiveTimeout (h : TaskHook) : Nat :=
  match h.timeoutSecs with | none => defaultHookTimeoutSecs | some t => t.toNat
/-- A command must contain an executable and its explicit timeout must be positive. -/
def hookAdmitted (h : TaskHook) : Bool :=
  !h.command.isEmpty &&
    (ConfigDefaults.resolveNat defaultHookTimeoutSecs 1 h.timeoutSecs).isSome
theorem default_timeout_admitted (h : TaskHook) (hnone : h.timeoutSecs = none)
    (hc : h.command ≠ []) : hookAdmitted h = true := by
  simp [hookAdmitted, ConfigDefaults.resolveNat_positive, hnone, hc]
theorem explicit_timeout_valid (h : TaskHook) (t : Int)
    (hsome : h.timeoutSecs = some t) (ht : 0 < t) (hc : h.command ≠ []) :
    hookAdmitted h = true := by
  simp [hookAdmitted, ConfigDefaults.resolveNat_positive, hsome, hc, ht]
theorem explicit_timeout_nonpositive_rejected (h : TaskHook) (t : Int)
    (hsome : h.timeoutSecs = some t) (ht : t ≤ 0) : hookAdmitted h = false := by
  simp [hookAdmitted, ConfigDefaults.resolveNat_positive, hsome, show ¬ (0 < t) by omega]
theorem admitted_command_nonempty (h : TaskHook) (ha : hookAdmitted h = true) :
    h.command ≠ [] := by
  simp only [hookAdmitted, ConfigDefaults.resolveNat_positive, Bool.and_eq_true] at ha
  simpa using ha.1

theorem admitted_timeout_positive (h : TaskHook) (ha : hookAdmitted h = true) :
    0 < h.effectiveTimeout := by
  cases ht : h.timeoutSecs with
  | none => simp [TaskHook.effectiveTimeout, ht, defaultHookTimeoutSecs]
  | some t =>
    have hp : 0 < t := by
      simp only [hookAdmitted, ConfigDefaults.resolveNat_positive, ht, Bool.and_eq_true, decide_eq_true_eq] at ha
      by_cases hp : 0 < t
      · exact hp
      · simp [hp] at ha
    simp only [TaskHook.effectiveTimeout, ht]
    omega

/-- Validate the whole task before execution. IDs identify occurrences across
all phases, so uniqueness is task-wide rather than per phase. -/
def admitHooks (hs : List TaskHook) : Option (List TaskHook) :=
  if hs.all hookAdmitted && decide (hs.map TaskHook.hookId).Nodup then some hs else none

theorem admitHooks_some_valid {hs admitted : List TaskHook}
    (h : admitHooks hs = some admitted) (x : TaskHook) (hx : x ∈ admitted) :
    hookAdmitted x = true := by
  unfold admitHooks at h
  split at h
  · next ha =>
    have he := Option.some.inj h
    subst admitted
    simp only [Bool.and_eq_true] at ha
    exact (List.all_eq_true.mp ha.1) x hx
  · simp at h

theorem admitHooks_unique_ids {hs admitted : List TaskHook}
    (h : admitHooks hs = some admitted) : (admitted.map TaskHook.hookId).Nodup := by
  unfold admitHooks at h
  split at h
  · next ha =>
    have he := Option.some.inj h
    subst admitted
    simp only [Bool.and_eq_true] at ha
    exact of_decide_eq_true ha.2
  · simp at h

/-- Result of one external command attempt. `interrupted` means the attempt
was cancelled or its outcome is unknown (e.g. after a crash); it is terminal
and is reported, never retried or blindly replayed. -/
inductive CommandResult where | exited (code : Int) | launchFailed | timedOut | interrupted
  deriving DecidableEq, Repr
/-- Conservative mapping onto observed managed-process states, only where the
outcome is actually known. An interrupted attempt has no known state, so it
maps to `none` — no managed-termination claim is made. -/
def CommandResult.toState : CommandResult → Option ManagedExecState
  | .exited _ => some .exited | .launchFailed => some .spawnFailed
  | .timedOut => some .killed | .interrupted => none
/-- Every known-outcome attempt lands in a terminal managed-exec state,
matching `HasTerminal.isTerminal` for `ManagedExecState`. -/
theorem CommandResult.toState_terminal (r : CommandResult) (s : ManagedExecState)
    (h : r.toState = some s) : HasTerminal.isTerminal s := by
  cases r with
  | exited _ => simp [CommandResult.toState] at h; subst h; exact Or.inl rfl
  | launchFailed =>
    simp [CommandResult.toState] at h; subst h; exact Or.inr (Or.inr (Or.inl rfl))
  | timedOut => simp [CommandResult.toState] at h; subst h; exact Or.inr (Or.inl rfl)
  | interrupted => simp [CommandResult.toState] at h
/-- A command attempt succeeds iff the child exited with status 0. -/
def CommandResult.succeeded : CommandResult → Bool
  | .exited 0 => true | _ => false
/-- Observations for one execution attempt, not a promise that host commands
are repeatable. Cwd/env, launch, capture, timeout, and process termination stay
with the existing host owner; this function does not implement host execution. -/
abbrev HookExec := TaskHook → CommandResult
/-- Outcome of a single agent execution; `interrupted` covers cancellation of
active work with unknown completion state. -/
inductive AgentResult where | success | failure | cancelled | interrupted
  deriving DecidableEq, Repr
/-- Primary error identity: a failing hook (by id) or the agent itself. -/
inductive PrimaryError where | hook (id : String) | agent
  deriving DecidableEq, Repr
/-- Primary task outcome before cleanup accounting. -/
inductive TaskOutcome where | success | failure (err : PrimaryError) | interrupted
  deriving DecidableEq, Repr
/-- Reuse of the existing request terminal-result abstraction: success
completes the request, failure fails it, interruption is the existing
terminal `RequestState.interrupted`. -/
def TaskOutcome.toRequestState : TaskOutcome → RequestState
  | .success => .completed | .failure _ => .failed | .interrupted => .interrupted
theorem TaskOutcome.toRequestState_terminal (o : TaskOutcome) :
    HasTerminal.isTerminal o.toRequestState := by
  cases o with
  | success => exact Or.inl rfl
  | failure _ => exact Or.inr (Or.inl rfl)
  | interrupted => exact Or.inr (Or.inr (Or.inr (Or.inr rfl)))
/-- One recorded hook attempt, in execution order. -/
structure HookAttempt where
  hookId : String
  result : CommandResult
  deriving DecidableEq, Repr
/-- Run one ordinary phase's hooks in declared list order, stopping at the
first failure; each attempt is recorded with its actual result. -/
def runPhase : List TaskHook → HookExec → List HookAttempt
  | [], _ => []
  | h :: rest, exec =>
      if (exec h).succeeded then
        { hookId := h.hookId, result := exec h } :: runPhase rest exec
      else [{ hookId := h.hookId, result := exec h }]
/-- Ordinary phases attempt a prefix in declared order, stopping at the first
error. The result trace cannot invent additional configured occurrences. -/
theorem runPhase_prefix (hs : List TaskHook) (exec : HookExec) :
    ∃ rest, hs.map TaskHook.hookId = (runPhase hs exec).map HookAttempt.hookId ++ rest := by
  induction hs with
  | nil => exact ⟨[], rfl⟩
  | cons h hs ih =>
    by_cases hx : (exec h).succeeded = true
    · obtain ⟨rest, hr⟩ := ih
      exact ⟨rest, by simp [runPhase, hx, hr]⟩
    · exact ⟨hs.map TaskHook.hookId, by simp [runPhase, hx]⟩

/-- Finally attempts every cleanup hook in declared order, whatever each
attempt's result is. -/
def runFinally (hs : List TaskHook) (exec : HookExec) : List HookAttempt :=
  hs.map (fun h => ⟨h.hookId, exec h⟩)
theorem runFinally_all (hs : List TaskHook) (exec : HookExec) :
    (runFinally hs exec).map HookAttempt.hookId = hs.map TaskHook.hookId := by
  simp [runFinally, List.map_map, Function.comp_def]

/-- One error reduction for ordinary phases and cleanup: preserve the first
failed occurrence and distinguish interrupted/unknown results from known failure. -/
def phaseOutcome (attempts : List HookAttempt) : TaskOutcome :=
  match attempts.find? (fun a => !a.result.succeeded) with
  | none => .success
  | some a => match a.result with
    | .interrupted => .interrupted
    | _ => .failure (.hook a.hookId)

/-- Hooks of a given phase, preserving the declared list order. -/
def hooksOfPhase (hs : List TaskHook) (p : HookPhase) : List TaskHook :=
  hs.filter (fun h => h.phase == p)
/-- Attempt trace and primary outcome. Agent absence records that preparation
prevented its execution; no synthetic counter or independent ran flag is needed. -/
structure RunResult where
  outcome : TaskOutcome := .success
  beforeAttempted : List HookAttempt := []
  afterSuccessAttempted : List HookAttempt := []
  afterFailureAttempted : List HookAttempt := []
  finallyAttempted : List HookAttempt := []
  agentResult : Option AgentResult := none
  deriving DecidableEq, Repr
/-- Secondary cleanup errors, recorded from the finally trace. -/
def phaseErrors (as : List HookAttempt) : List String :=
  (as.filter (fun a => !a.result.succeeded)).map HookAttempt.hookId
def RunResult.cleanupErrors (r : RunResult) : List String := phaseErrors r.finallyAttempted
open RunResult

theorem mem_cleanupErrors (r : RunResult) (id : String) : id ∈ r.cleanupErrors ↔
    ∃ a ∈ r.finallyAttempted, a.hookId = id ∧ a.result.succeeded = false := by
  simp [cleanupErrors, phaseErrors, List.mem_map, List.mem_filter, and_assoc, and_left_comm, and_comm]
/-- Final outcome: cleanup failure turns success into failure (naming the
first failing cleanup hook, or interruption for an unknown outcome), but never
erases a primary failure, agent error, or interruption — the primary identity is preserved. -/
def RunResult.finalOutcome (r : RunResult) : TaskOutcome :=
  match r.outcome with
  | .success => phaseOutcome r.finallyAttempted
  | other => other

/-- Cleanup cannot overwrite any prior failure or interruption. -/
theorem finalOutcome_primary_preserved (r : RunResult) (h : r.outcome ≠ .success) :
    r.finalOutcome = r.outcome := by
  cases ho : r.outcome <;> simp_all [finalOutcome]

/-- A previously successful run adopts the cleanup phase's first error. -/
theorem finalOutcome_cleanup (r : RunResult) (h : r.outcome = .success) :
    r.finalOutcome = phaseOutcome r.finallyAttempted := by simp [finalOutcome, h]

/-- The single orchestration: preparation selects whether the agent runs;
its outcome selects one ordinary after-phase, then every finally hook is attempted.
The agent result is an observation from the existing execution owner, just like
HookExec. This model does not implement or retry either external execution. -/
def runTask (hooks : List TaskHook) (exec : HookExec) (agent : AgentResult) : RunResult :=
  let before := runPhase (hooksOfPhase hooks .before) exec
  let base : RunResult :=
    { beforeAttempted := before, finallyAttempted := runFinally (hooksOfPhase hooks .finally) exec }
  match phaseOutcome before with
  | .interrupted => { base with outcome := .interrupted }
  | .failure error =>
    { base with
      outcome := .failure error,
      afterFailureAttempted := runPhase (hooksOfPhase hooks .afterFailure) exec }
  | .success =>
    let running := { base with agentResult := some agent }
    match agent with
    | .cancelled | .interrupted => { running with outcome := .interrupted }
    | .success =>
      let after := runPhase (hooksOfPhase hooks .afterSuccess) exec
      { running with outcome := phaseOutcome after, afterSuccessAttempted := after }
    | .failure =>
      { running with
        outcome := .failure .agent,
        afterFailureAttempted := runPhase (hooksOfPhase hooks .afterFailure) exec }

/-- Cleanup attempt order is independent of preparation and agent outcome. -/
theorem runTask_finally_all (hooks : List TaskHook) (exec : HookExec) (agent : AgentResult) :
    (runTask hooks exec agent).finallyAttempted.map HookAttempt.hookId =
      (hooksOfPhase hooks .finally).map TaskHook.hookId := by
  cases h : phaseOutcome (runPhase (hooksOfPhase hooks .before) exec) <;>
    cases agent <;> simp [runTask, h, runFinally_all]

/-- Exactly successful preparation records an agent attempt. -/
theorem runTask_agent_attempt_iff (hooks : List TaskHook) (exec : HookExec) (agent : AgentResult) :
    (runTask hooks exec agent).agentResult = some agent ↔
      phaseOutcome (runPhase (hooksOfPhase hooks .before) exec) = .success := by
  cases h : phaseOutcome (runPhase (hooksOfPhase hooks .before) exec) <;>
    cases agent <;> simp [runTask, h]

/-- Before failure prevents the agent and selects after_failure. -/
theorem runTask_beforeFailure (hooks : List TaskHook) (exec : HookExec) (agent : AgentResult)
    (error : PrimaryError)
    (hb : phaseOutcome (runPhase (hooksOfPhase hooks .before) exec) = .failure error) :
    (runTask hooks exec agent).outcome = .failure error ∧
    (runTask hooks exec agent).agentResult = none ∧
    (runTask hooks exec agent).afterSuccessAttempted = [] ∧
    (runTask hooks exec agent).afterFailureAttempted =
      runPhase (hooksOfPhase hooks .afterFailure) exec := by simp [runTask, hb]

/-- Success runs only after_success, even when that phase fails. -/
theorem runTask_agentSuccess (hooks : List TaskHook) (exec : HookExec)
    (hb : phaseOutcome (runPhase (hooksOfPhase hooks .before) exec) = .success) :
    (runTask hooks exec .success).afterFailureAttempted = [] ∧
    (runTask hooks exec .success).afterSuccessAttempted =
      runPhase (hooksOfPhase hooks .afterSuccess) exec := by simp [runTask, hb]

/-- Agent failure remains primary while after_failure is attempted. -/
theorem runTask_agentFailure (hooks : List TaskHook) (exec : HookExec)
    (hb : phaseOutcome (runPhase (hooksOfPhase hooks .before) exec) = .success) :
    (runTask hooks exec .failure).outcome = .failure .agent ∧
    (runTask hooks exec .failure).afterSuccessAttempted = [] ∧
    (runTask hooks exec .failure).afterFailureAttempted =
      runPhase (hooksOfPhase hooks .afterFailure) exec := by simp [runTask, hb]

/-- Both cancellation and unknown interruption skip ordinary outcome phases. -/
theorem runTask_interrupted (hooks : List TaskHook) (exec : HookExec) (agent : AgentResult)
    (hb : phaseOutcome (runPhase (hooksOfPhase hooks .before) exec) = .success)
    (ha : agent = .cancelled ∨ agent = .interrupted) :
    (runTask hooks exec agent).outcome = .interrupted ∧
    (runTask hooks exec agent).afterSuccessAttempted = [] ∧
    (runTask hooks exec agent).afterFailureAttempted = [] := by
  rcases ha with rfl | rfl <;> simp [runTask, hb]

/-- Admission is the sole entrance; rejected config produces no execution trace. -/
def runOrReject (hooks : List TaskHook) (exec : HookExec) (agent : AgentResult) : Option RunResult :=
  (admitHooks hooks).map (fun admitted => runTask admitted exec agent)

theorem runOrReject_rejected_no_hooks_no_agent (hooks : List TaskHook)
    (exec : HookExec) (agent : AgentResult) (h : admitHooks hooks = none) :
    runOrReject hooks exec agent = none := by simp [runOrReject, h]

/-! Recovery consumes observations from the existing execution owner. It does
not rerun ordinary phases, and never schedules an observed occurrence again,
including one whose external outcome is unknown. Missing/unreliable observations
cannot support an exactly-once claim about actual host effects. -/
def recoveryCleanup (hooks : List TaskHook) (observed : List HookAttempt) : List TaskHook :=
  hooks.filter fun h => h.phase == .finally &&
    !(observed.any fun a => a.hookId == h.hookId)

/-- `started` is observed by the existing execution owner: admission alone does
not start work. A crash before the first before-hook/agent step runs no cleanup. -/
def recoverInterrupted (started : Bool) (hooks : List TaskHook) (observed : List HookAttempt)
    (exec : HookExec) : RequestState × List HookAttempt :=
  (.interrupted, if started then runFinally (recoveryCleanup hooks observed) exec else [])

theorem recovery_before_start_no_cleanup (hooks : List TaskHook)
    (observed : List HookAttempt) (exec : HookExec) :
    (recoverInterrupted false hooks observed exec).2 = [] := rfl

theorem recovery_only_cleanup (hooks : List TaskHook) (observed : List HookAttempt)
    (h : TaskHook) (hm : h ∈ recoveryCleanup hooks observed) : h.phase = .finally := by
  simp [recoveryCleanup] at hm
  exact hm.2.1

theorem recovery_excludes_observed (hooks : List TaskHook) (observed : List HookAttempt)
    (h : TaskHook) (hm : h ∈ recoveryCleanup hooks observed) :
    ∀ a ∈ observed, a.hookId ≠ h.hookId := by
  simp [recoveryCleanup] at hm
  exact hm.2.2

theorem recovery_does_not_repeat_attempt (started : Bool) (hooks : List TaskHook)
    (observed : List HookAttempt) (exec : HookExec) (attempt : HookAttempt)
    (hm : attempt ∈ (recoverInterrupted started hooks observed exec).2) :
    ∀ old ∈ observed, old.hookId ≠ attempt.hookId := by
  cases started with
  | false => simp [recoverInterrupted] at hm
  | true =>
    simp only [recoverInterrupted, Bool.true_eq, ↓reduceIte] at hm
    have hi : attempt.hookId ∈ (recoveryCleanup hooks observed).map TaskHook.hookId := by
      rw [← runFinally_all (recoveryCleanup hooks observed) exec]
      exact List.mem_map.mpr ⟨attempt, hm, rfl⟩
    obtain ⟨h, hh, he⟩ := List.mem_map.mp hi
    intro old ho heq
    exact recovery_excludes_observed hooks observed h hh old ho (heq.trans he.symm)

theorem recovery_reports_interruption (started : Bool) (hooks : List TaskHook)
    (observed : List HookAttempt) (exec : HookExec) :
    (recoverInterrupted started hooks observed exec).1 = .interrupted := rfl

/-! ## Concrete checks -/
def sampleBefore : TaskHook := { hookId := "prepare", phase := .before, command := ["bin/prepare"] }
def sampleFinally : TaskHook :=
  { hookId := "cleanup", phase := HookPhase.finally, command := ["bin/cleanup"] }
def sampleHooks : List TaskHook := [sampleBefore, sampleFinally]
def cleanupOnlyHooks : List TaskHook := [sampleFinally]
def failExec : HookExec := fun _ => .exited 1
def okExec : HookExec := fun _ => .exited 0
/-- A failing first before-hook prevents the agent, fails with the hook's
identity, and still attempts every finally command in order. -/
theorem sample_before_failure :
    (runTask sampleHooks failExec .success).outcome =
      TaskOutcome.failure (PrimaryError.hook "prepare") ∧
    (runTask sampleHooks failExec .success).agentResult = none ∧
    (runTask sampleHooks failExec .success).finallyAttempted.map
      HookAttempt.hookId = ["cleanup"] := by
  decide
/-- Cleanup failure turns success into failure while the secondary error is
recorded. -/
theorem sample_cleanup_failure_fails :
    (runTask cleanupOnlyHooks failExec .success).outcome = .success ∧
    (runTask cleanupOnlyHooks failExec .success).cleanupErrors = ["cleanup"] ∧
    (runTask cleanupOnlyHooks failExec .success).finalOutcome =
      .failure (PrimaryError.hook "cleanup") := by
  decide
/-- Hook interruption: reported as interrupted, agent never ran, every finally
command still attempted. -/
theorem sample_hook_interrupted :
    (runTask sampleHooks (fun _ => CommandResult.interrupted) .success).outcome = .interrupted ∧
    (runTask sampleHooks (fun _ => CommandResult.interrupted) .success).agentResult = none ∧
    (runTask sampleHooks (fun _ => CommandResult.interrupted) .success).finallyAttempted.map HookAttempt.hookId = ["cleanup"] := by
  decide
/-- A negative configured timeout is rejected before admission: no hooks, no
agent step. -/
theorem sample_negative_timeout_rejected :
    admitHooks [{ hookId := "h", phase := .before, command := ["x"],
                  timeoutSecs := some (-1) }] = none := by
  decide
theorem sample_admit_ok : admitHooks sampleHooks = some sampleHooks := by
  decide

/-- Occurrences must remain distinguishable even across different phases. -/
theorem sample_duplicate_id_rejected :
    admitHooks [sampleBefore, { sampleFinally with hookId := sampleBefore.hookId }] = none := by
  decide

theorem sample_empty_command_rejected :
    admitHooks [{ sampleBefore with command := [] }] = none := by
  decide

theorem sample_cleanup_interruption_reported :
    (runTask cleanupOnlyHooks (fun _ => .interrupted) .success).finalOutcome = .interrupted := by
  decide

/-- Cleanup interruption does not overwrite an earlier agent failure. -/
theorem sample_cleanup_interruption_preserves_failure :
    (runTask cleanupOnlyHooks (fun _ => .interrupted) .failure).finalOutcome =
      .failure .agent := by
  decide

/-- A prior cleanup failure stays primary if a later cleanup is interrupted. -/
theorem sample_first_cleanup_failure_preserved :
    (runTask [sampleFinally, { sampleFinally with hookId := "second" }]
      (fun h => if h.hookId == "cleanup" then .exited 1 else .interrupted) .success).finalOutcome =
      .failure (.hook "cleanup") := by
  decide

/-- Started recovery skips an unknown observed command but attempts remaining cleanup. -/
theorem sample_recovery_remaining_cleanup :
    (recoverInterrupted true [sampleFinally, { sampleFinally with hookId := "second" }]
      [{ hookId := "cleanup", result := .interrupted }] okExec).2.map HookAttempt.hookId =
        ["second"] := by
  decide

end TaskHooks
