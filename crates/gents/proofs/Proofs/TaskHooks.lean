import Proofs.ConfigDefaults
import Proofs.ManagedExec.State
import Proofs.Request.State
import Proofs.Recovery.Sweeps.Requests

namespace TaskHooks
/-! # Task hook-phase semantics (spec PR #1430)
Executable model: phases `before`, `afterSuccess`, `afterFailure`, `finally`
around one agent execution. Literal argv hooks, optional positive timeout
(default 120, at most `maxHookTimeoutSecs`); nonpositive, negative and
over-maximum timeouts (`Option Int`) are rejected at admission, as are empty argv and duplicate occurrence IDs, before any hook or
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
structure TaskHook where
  hookId : String
  phase : HookPhase
  command : List String
  timeoutSecs : Option Int := none
  skipExitCodes : List Int := []
  deriving DecidableEq, Repr
def defaultHookTimeoutSecs : Nat := 120
/-- Largest admitted explicit timeout (one day). Admission bounds the timeout
from above so every admitted hook has a deadline the host can represent; the
executor then never refuses a hook at launch for its timeout. -/
def maxHookTimeoutSecs : Nat := 86400
def TaskHook.effectiveTimeout (h : TaskHook) : Nat :=
  match h.timeoutSecs with | none => defaultHookTimeoutSecs | some t => t.toNat
def hookAdmitted (h : TaskHook) : Bool :=
  !h.command.isEmpty &&
    (ConfigDefaults.resolveNat defaultHookTimeoutSecs 1 h.timeoutSecs).isSome &&
    h.timeoutSecs.all (fun (t : Int) => decide (t ≤ (maxHookTimeoutSecs : Int)))
theorem default_timeout_admitted (h : TaskHook) (hnone : h.timeoutSecs = none)
    (hc : h.command ≠ []) : hookAdmitted h = true := by
  simp [hookAdmitted, ConfigDefaults.resolveNat_positive, hnone, hc]
theorem explicit_timeout_valid (h : TaskHook) (t : Int)
    (hsome : h.timeoutSecs = some t) (ht : 0 < t) (hmax : t ≤ maxHookTimeoutSecs)
    (hc : h.command ≠ []) : hookAdmitted h = true := by
  simp [hookAdmitted, ConfigDefaults.resolveNat_positive, hsome, hc, ht, hmax]
theorem explicit_timeout_above_maximum_rejected (h : TaskHook) (t : Int)
    (hsome : h.timeoutSecs = some t) (ht : (maxHookTimeoutSecs : Int) < t) :
    hookAdmitted h = false := by
  simp [hookAdmitted, hsome, show ¬ (t ≤ maxHookTimeoutSecs) by omega]
theorem admitted_timeout_bounded (h : TaskHook) (ha : hookAdmitted h = true) :
    h.effectiveTimeout ≤ maxHookTimeoutSecs := by
  cases ht : h.timeoutSecs with
  | none => simp [TaskHook.effectiveTimeout, ht, defaultHookTimeoutSecs, maxHookTimeoutSecs]
  | some t =>
    have hle : t ≤ maxHookTimeoutSecs := by
      simp only [hookAdmitted, ht, Option.all, Bool.and_eq_true, decide_eq_true_eq] at ha
      exact ha.2
    simp only [TaskHook.effectiveTimeout, ht]
    omega
theorem explicit_timeout_nonpositive_rejected (h : TaskHook) (t : Int)
    (hsome : h.timeoutSecs = some t) (ht : t ≤ 0) : hookAdmitted h = false := by
  simp [hookAdmitted, ConfigDefaults.resolveNat_positive, hsome, show ¬ (0 < t) by omega]
theorem admitted_command_nonempty (h : TaskHook) (ha : hookAdmitted h = true) :
    h.command ≠ [] := by
  simp only [hookAdmitted, ConfigDefaults.resolveNat_positive, Bool.and_eq_true] at ha
  simpa using ha.1.1

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
def skipCodesAdmitted (h : TaskHook) : Bool :=
  h.skipExitCodes.isEmpty ||
    (h.phase == .before && h.skipExitCodes.all (fun code => decide (0 < code ∧ code ≤ 255)))

def admitHooks (hs : List TaskHook) : Option (List TaskHook) :=
  if hs.all hookAdmitted && hs.all skipCodesAdmitted && decide (hs.map TaskHook.hookId).Nodup then some hs else none

theorem admitHooks_some_valid {hs admitted : List TaskHook}
    (h : admitHooks hs = some admitted) (x : TaskHook) (hx : x ∈ admitted) :
    hookAdmitted x = true := by
  unfold admitHooks at h
  split at h
  · next ha =>
    have he := Option.some.inj h
    subst admitted
    simp only [Bool.and_eq_true] at ha
    exact (List.all_eq_true.mp ha.1.1) x hx
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
and is reported, never retried or blindly replayed. An ordinary-phase command
cancelled because its execution lost request ownership (a revocation such as
LatestOnly supersession) is `interrupted` too: the owner that revoked it has
written the terminal, which stands, and cleanup still runs. A command refused
before launch for a reason that cannot change (recovery found its root no
longer admitted) is
`launchFailed` and counts as attempted, so recovery never selects it again;
only a command shutdown kept from launching stays unattempted. -/
inductive CommandResult where | exited (code : Int) | launchFailed | timedOut | interrupted
  deriving DecidableEq, Repr
/-- Conservative mapping onto observed managed-process states, only where the
outcome is actually known. An interrupted attempt has no known state, so it
maps to `none` — no managed-termination claim is made. -/
def CommandResult.toState : CommandResult → Option ManagedExecState
  | .exited _ => some .exited | .launchFailed => some .spawnFailed
  | .timedOut => some .killed | .interrupted => none
theorem CommandResult.toState_terminal (r : CommandResult) (s : ManagedExecState)
    (h : r.toState = some s) : HasTerminal.isTerminal s := by
  cases r with
  | exited _ => simp [CommandResult.toState] at h; subst h; exact Or.inl rfl
  | launchFailed =>
    simp [CommandResult.toState] at h; subst h; exact Or.inr (Or.inr (Or.inl rfl))
  | timedOut => simp [CommandResult.toState] at h; subst h; exact Or.inr (Or.inl rfl)
  | interrupted => simp [CommandResult.toState] at h
def CommandResult.succeeded : CommandResult → Bool
  | .exited 0 => true | _ => false

/-- A before-hook may explicitly stop the agent without reporting failure.
Only configured nonzero exit codes have this meaning; process interruption,
timeout, launch failure and all other nonzero exits retain their normal outcome. -/
inductive BeforeDecision where | proceed | skip | failure | interrupted
  deriving DecidableEq, Repr

def beforeDecision (skipExitCodes : List Int) (result : CommandResult) : BeforeDecision :=
  match result with
  | .exited 0 => .proceed
  | .exited code => if code ∈ skipExitCodes then .skip else .failure
  | .interrupted => .interrupted
  | .launchFailed | .timedOut => .failure

theorem beforeDecision_zero_always_proceeds (codes : List Int) :
    beforeDecision codes (.exited 0) = .proceed := by rfl

theorem beforeDecision_skip_requires_configured_nonzero (codes : List Int) (code : Int)
    (h : beforeDecision codes (.exited code) = .skip) :
    code ≠ 0 ∧ code ∈ codes := by
  by_cases hz : code = 0
  · subst code; simp [beforeDecision] at h
  · simp [beforeDecision, hz] at h
    exact ⟨hz, h⟩

theorem beforeDecision_interruption_never_skips (codes : List Int) :
    beforeDecision codes .interrupted = .interrupted := by rfl

example : beforeDecision [10] (.exited 10) = .skip := by decide
example : beforeDecision [10] (.exited 1) = .failure := by decide
example : beforeDecision [10] .timedOut = .failure := by decide
/-- Observations for one execution attempt, not a promise that host commands
are repeatable. Cwd/env, launch, capture, timeout, and process termination stay
with the existing host owner; this function does not implement host execution. -/
abbrev HookExec := TaskHook → CommandResult

/-- Ordinary launches require a fresh observation from the request execution
owner, not the last renewal poll. Cleanup survives ownership loss. The host
cannot atomically combine a database read with process spawn: revocation after
this read is handled by the existing renewal cancellation signal. -/
def launchAllowed (phase : HookPhase) (ownsExecution : Bool) : Bool :=
  phase == .finally || ownsExecution

/-- Ownership is observed separately for each occurrence immediately before
launch. It does not introduce another lease or a task lifecycle. -/
def ownershipCheckedExec (owns : TaskHook → Bool) (exec : HookExec) : HookExec :=
  fun h => if launchAllowed h.phase (owns h) then exec h else .interrupted

theorem revoked_ordinary_launch_refused (h : TaskHook) (owns : TaskHook → Bool)
    (exec : HookExec) (hp : h.phase ≠ .finally) (ho : owns h = false) :
    ownershipCheckedExec owns exec h = .interrupted := by
  simp [ownershipCheckedExec, launchAllowed, hp, ho]

theorem cleanup_launch_survives_revocation (h : TaskHook) (owns : TaskHook → Bool)
    (exec : HookExec) (hp : h.phase = .finally) :
    ownershipCheckedExec owns exec h = exec h := by
  simp [ownershipCheckedExec, launchAllowed, hp]
/-- Outcome of a single agent execution; `interrupted` covers cancellation of
active work with unknown completion state. An execution that loses request
ownership mid-work is `interrupted` too: its result is unknown to this owner,
so ordinary outcome phases are skipped while cleanup still runs, and whatever
terminal the request's current owner records stands. -/
inductive AgentResult where | success | failure | cancelled | interrupted
  deriving DecidableEq, Repr
inductive PrimaryError where | hook (id : String) | agent
  deriving DecidableEq, Repr
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
attempt's result is. A user interrupt cancels ordinary-phase commands but never
a cleanup command: cleanup is what the interrupted execution still owes, so only
runtime shutdown may cancel it. A cleanup command that shutdown prevented from
launching was never observed, and so remains for `recoveryCleanup`. -/
def runFinally (hs : List TaskHook) (exec : HookExec) : List HookAttempt :=
  hs.map (fun h => ⟨h.hookId, exec h⟩)
theorem runFinally_all (hs : List TaskHook) (exec : HookExec) :
    (runFinally hs exec).map HookAttempt.hookId = hs.map TaskHook.hookId := by
  simp [runFinally, List.map_map, Function.comp_def]

def phaseOutcome (attempts : List HookAttempt) : TaskOutcome :=
  match attempts.find? (fun a => !a.result.succeeded) with
  | none => .success
  | some a => match a.result with
    | .interrupted => .interrupted
    | _ => .failure (.hook a.hookId)

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

theorem finalOutcome_primary_preserved (r : RunResult) (h : r.outcome ≠ .success) :
    r.finalOutcome = r.outcome := by
  cases ho : r.outcome <;> simp_all [finalOutcome]

theorem finalOutcome_cleanup (r : RunResult) (h : r.outcome = .success) :
    r.finalOutcome = phaseOutcome r.finallyAttempted := by simp [finalOutcome, h]

/-- The single orchestration: preparation selects whether the agent runs;
its outcome selects one ordinary after-phase, then every finally hook is attempted.
The agent result is an observation from the existing execution owner, just like
HookExec. This model does not implement or retry either external execution.
That owner reports `success` only after its own completion effects, including
workspace seal and integration, so `afterSuccess` gates the request's terminal
but never those effects; how hooks relate to workspace integration is an open
design question, and current workspace behavior is preserved until it is
decided. Hook time also counts against the request deadline fixed at claim. -/
def runTaskBase (hooks : List TaskHook) (exec : HookExec) (agent : AgentResult) : RunResult :=
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

/-- A configured skip is a successful terminal decision, not a successful
before phase: the agent and both ordinary after phases are not run. Cleanup
retains the same obligations and can still make the final outcome fail. -/
def runTask (hooks : List TaskHook) (exec : HookExec) (agent : AgentResult)
    : RunResult :=
  let before := runPhase (hooksOfPhase hooks .before) exec
  match before.find? (fun a => !a.result.succeeded) with
  | some attempt =>
    let codes := ((hooks.find? (fun h => h.hookId == attempt.hookId)).map
      TaskHook.skipExitCodes).getD []
    if beforeDecision codes attempt.result = .skip then
      { beforeAttempted := before,
        finallyAttempted := runFinally (hooksOfPhase hooks .finally) exec }
    else runTaskBase hooks exec agent
  | none => runTaskBase hooks exec agent

theorem configured_skip_omits_agent_and_after_phases
    (hooks : List TaskHook) (exec : HookExec) (agent : AgentResult)
    (attempt : HookAttempt)
    (found : (runPhase (hooksOfPhase hooks .before) exec).find?
      (fun a => !a.result.succeeded) = some attempt)
    (skip : beforeDecision (((hooks.find? (fun h => h.hookId == attempt.hookId)).map
      TaskHook.skipExitCodes).getD []) attempt.result = .skip) :
    (runTask hooks exec agent).agentResult = none ∧
    (runTask hooks exec agent).afterSuccessAttempted = [] ∧
    (runTask hooks exec agent).afterFailureAttempted = [] ∧
    (runTask hooks exec agent).finallyAttempted =
      runFinally (hooksOfPhase hooks .finally) exec := by
  simp [runTask, found, skip]

theorem runTaskBase_finally_all (hooks : List TaskHook) (exec : HookExec) (agent : AgentResult) :
    (runTaskBase hooks exec agent).finallyAttempted.map HookAttempt.hookId =
      (hooksOfPhase hooks .finally).map TaskHook.hookId := by
  cases h : phaseOutcome (runPhase (hooksOfPhase hooks .before) exec) <;>
    cases agent <;> simp [runTaskBase, h, runFinally_all]

theorem runTask_finally_all (hooks : List TaskHook) (exec : HookExec) (agent : AgentResult) :
    (runTask hooks exec agent).finallyAttempted.map HookAttempt.hookId =
      (hooksOfPhase hooks .finally).map TaskHook.hookId := by
  cases h : (runPhase (hooksOfPhase hooks .before) exec).find?
      (fun a => !a.result.succeeded) with
  | none => simp [runTask, h, runTaskBase_finally_all]
  | some attempt =>
    simp only [runTask, h]
    split <;> simp [runTaskBase_finally_all, runFinally_all]

theorem runTaskBase_agent_attempt_iff (hooks : List TaskHook) (exec : HookExec) (agent : AgentResult) :
    (runTaskBase hooks exec agent).agentResult = some agent ↔
      phaseOutcome (runPhase (hooksOfPhase hooks .before) exec) = .success := by
  cases h : phaseOutcome (runPhase (hooksOfPhase hooks .before) exec) <;>
    cases agent <;> simp [runTaskBase, h]

theorem runTaskBase_beforeFailure (hooks : List TaskHook) (exec : HookExec) (agent : AgentResult)
    (error : PrimaryError)
    (hb : phaseOutcome (runPhase (hooksOfPhase hooks .before) exec) = .failure error) :
    (runTaskBase hooks exec agent).outcome = .failure error ∧
    (runTaskBase hooks exec agent).agentResult = none ∧
    (runTaskBase hooks exec agent).afterSuccessAttempted = [] ∧
    (runTaskBase hooks exec agent).afterFailureAttempted =
      runPhase (hooksOfPhase hooks .afterFailure) exec := by simp [runTaskBase, hb]

theorem runTaskBase_agentSuccess (hooks : List TaskHook) (exec : HookExec)
    (hb : phaseOutcome (runPhase (hooksOfPhase hooks .before) exec) = .success) :
    (runTaskBase hooks exec .success).afterFailureAttempted = [] ∧
    (runTaskBase hooks exec .success).afterSuccessAttempted =
      runPhase (hooksOfPhase hooks .afterSuccess) exec := by simp [runTaskBase, hb]

theorem runTaskBase_agentFailure (hooks : List TaskHook) (exec : HookExec)
    (hb : phaseOutcome (runPhase (hooksOfPhase hooks .before) exec) = .success) :
    (runTaskBase hooks exec .failure).outcome = .failure .agent ∧
    (runTaskBase hooks exec .failure).afterSuccessAttempted = [] ∧
    (runTaskBase hooks exec .failure).afterFailureAttempted =
      runPhase (hooksOfPhase hooks .afterFailure) exec := by simp [runTaskBase, hb]

theorem runTaskBase_interrupted (hooks : List TaskHook) (exec : HookExec) (agent : AgentResult)
    (hb : phaseOutcome (runPhase (hooksOfPhase hooks .before) exec) = .success)
    (ha : agent = .cancelled ∨ agent = .interrupted) :
    (runTaskBase hooks exec agent).outcome = .interrupted ∧
    (runTaskBase hooks exec agent).afterSuccessAttempted = [] ∧
    (runTaskBase hooks exec agent).afterFailureAttempted = [] := by
  rcases ha with rfl | rfl <;> simp [runTaskBase, hb]

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
not start work. A crash before the first before-hook/agent step runs no cleanup.
The request's terminal state is not decided here: request recovery owns it
(`Recovery.recoveredRequestState`, failed unless an interrupt was requested) and
writes it before cleanup recovery runs. Reporting the owner's decision, rather
than always `interrupted`, keeps one terminal writer after a crash; cleanup
recovery cannot write a second, different outcome for the same request. -/
def recoverInterrupted (interruptRequested started : Bool) (hooks : List TaskHook)
    (observed : List HookAttempt) (exec : HookExec) : RequestState × List HookAttempt :=
  (Recovery.recoveredRequestState interruptRequested,
    if started then runFinally (recoveryCleanup hooks observed) exec else [])

theorem recovery_before_start_no_cleanup (interruptRequested : Bool) (hooks : List TaskHook)
    (observed : List HookAttempt) (exec : HookExec) :
    (recoverInterrupted interruptRequested false hooks observed exec).2 = [] := rfl

theorem recovery_only_cleanup (hooks : List TaskHook) (observed : List HookAttempt)
    (h : TaskHook) (hm : h ∈ recoveryCleanup hooks observed) : h.phase = .finally := by
  simp [recoveryCleanup] at hm
  exact hm.2.1

theorem recovery_excludes_observed (hooks : List TaskHook) (observed : List HookAttempt)
    (h : TaskHook) (hm : h ∈ recoveryCleanup hooks observed) :
    ∀ a ∈ observed, a.hookId ≠ h.hookId := by
  simp [recoveryCleanup] at hm
  exact hm.2.2

theorem recovery_does_not_repeat_attempt (interruptRequested started : Bool)
    (hooks : List TaskHook) (observed : List HookAttempt) (exec : HookExec)
    (attempt : HookAttempt)
    (hm : attempt ∈ (recoverInterrupted interruptRequested started hooks observed exec).2) :
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

theorem recovery_reports_request_owner_decision (interruptRequested started : Bool)
    (hooks : List TaskHook) (observed : List HookAttempt) (exec : HookExec) :
    (recoverInterrupted interruptRequested started hooks observed exec).1 =
      Recovery.recoveredRequestState interruptRequested := rfl

theorem recovery_without_interrupt_request_fails (started : Bool) (hooks : List TaskHook)
    (observed : List HookAttempt) (exec : HookExec) :
    (recoverInterrupted false started hooks observed exec).1 = .failed := rfl

theorem recovery_with_interrupt_request_interrupts (started : Bool) (hooks : List TaskHook)
    (observed : List HookAttempt) (exec : HookExec) :
    (recoverInterrupted true started hooks observed exec).1 = .interrupted := rfl

def sampleBefore : TaskHook := { hookId := "prepare", phase := .before, command := ["bin/prepare"] }
def sampleFinally : TaskHook :=
  { hookId := "cleanup", phase := HookPhase.finally, command := ["bin/cleanup"] }
def sampleHooks : List TaskHook := [sampleBefore, sampleFinally]
def cleanupOnlyHooks : List TaskHook := [sampleFinally]
def failExec : HookExec := fun _ => .exited 1
def okExec : HookExec := fun _ => .exited 0
theorem sample_before_failure :
    (runTaskBase sampleHooks failExec .success).outcome =
      TaskOutcome.failure (PrimaryError.hook "prepare") ∧
    (runTaskBase sampleHooks failExec .success).agentResult = none ∧
    (runTaskBase sampleHooks failExec .success).finallyAttempted.map
      HookAttempt.hookId = ["cleanup"] := by
  decide
theorem sample_cleanup_failure_fails :
    (runTaskBase cleanupOnlyHooks failExec .success).outcome = .success ∧
    (runTaskBase cleanupOnlyHooks failExec .success).cleanupErrors = ["cleanup"] ∧
    (runTaskBase cleanupOnlyHooks failExec .success).finalOutcome =
      .failure (PrimaryError.hook "cleanup") := by
  decide
theorem sample_hook_interrupted :
    (runTaskBase sampleHooks (fun _ => CommandResult.interrupted) .success).outcome = .interrupted ∧
    (runTaskBase sampleHooks (fun _ => CommandResult.interrupted) .success).agentResult = none ∧
    (runTaskBase sampleHooks (fun _ => CommandResult.interrupted) .success).finallyAttempted.map HookAttempt.hookId = ["cleanup"] := by
  decide
theorem sample_negative_timeout_rejected :
    admitHooks [{ hookId := "h", phase := .before, command := ["x"],
                  timeoutSecs := some (-1) }] = none := by
  decide
theorem sample_admit_ok : admitHooks sampleHooks = some sampleHooks := by
  decide
theorem sample_duplicate_id_rejected :
    admitHooks [sampleBefore, { sampleFinally with hookId := sampleBefore.hookId }] = none := by
  decide
theorem sample_empty_command_rejected :
    admitHooks [{ sampleBefore with command := [] }] = none := by
  decide
theorem sample_cleanup_interruption_reported :
    (runTaskBase cleanupOnlyHooks (fun _ => .interrupted) .success).finalOutcome = .interrupted := by
  decide
theorem sample_cleanup_interruption_preserves_failure :
    (runTaskBase cleanupOnlyHooks (fun _ => .interrupted) .failure).finalOutcome =
      .failure .agent := by
  decide
theorem sample_first_cleanup_failure_preserved :
    (runTaskBase [sampleFinally, { sampleFinally with hookId := "second" }]
      (fun h => if h.hookId == "cleanup" then .exited 1 else .interrupted) .success).finalOutcome =
      .failure (.hook "cleanup") := by
  decide
theorem sample_recovery_remaining_cleanup :
    (recoverInterrupted false true [sampleFinally, { sampleFinally with hookId := "second" }]
      [{ hookId := "cleanup", result := .interrupted }] okExec).2.map HookAttempt.hookId =
        ["second"] := by
  decide

end TaskHooks
