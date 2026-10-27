import Proofs.PluginJob.Keys
import Proofs.Recovery.Sweeps.SessionMessage
import Proofs.Background.CompletionDelivery
import Proofs.Background.Interrupt
import Proofs.DurableLineage
import Proofs.Background.ProcessControl
import Proofs.ToolPolicy.PluginData

/-!
# Plugin-job controls

Stop, pause and the wall-clock deadline share one kill path: every live child
this chain caused gets `Recovery.killAction`, and the job row ends at once.
Unlike a session-message row, a job row never waits on a child's terminal.
A resume starts a new row and kills nothing. Control is local-only:
it runs on the node whose runtime drives the row, so no control has a
remote case.

A resume is validated under the prior row's process-control scope: the same
plugin and a terminal prior row. Its pin takes the handle, chain root and
`args_digest` from the chain root's pin, never from the resume verb's own
arguments, and every step of the new row echoes the resume block and the chain
root's call. `job_calls.result` reads a terminal row's stored result under the
same scope rule, and only tool-call invocations are offered it.
-/

namespace PluginJob

open Background.CompletionDelivery

inductive Control
  | stop
  | pause
  | deadline
  | resume (root prior : Job) (r : Resume)

/-- The row state a kill ends in; `none` for a resume. -/
def Control.ends : Control → Option RowState
  | .stop => some .cancelled
  | .pause => some .completed
  | .deadline => some .timedOut
  | .resume .. => none

/-- A paused row is `completed` with a paused note, so it stays resumable. -/
def Control.note : Control → TerminalNote
  | .pause => .paused (some .user)
  | _ => .none

/-- Children this chain caused that are still live. -/
def liveOwnChildren (job : Job) : List Child :=
  job.children.filter fun ch => ch.key.root == job.root && ch.state == .live

/-- The terminal compare-and-set on the job row: only a running row is
    terminalized, so the first terminalizer wins and every later one loses. -/
def terminalizeRow (job : Job) (s : RowState) : Job × TerminalizationResult :=
  if job.state = .running then ({ job with state := s }, .won) else (job, .lost)

/-- The row ends through `terminalizeRow`, so only the terminalizer that wins
    the compare-and-set kills children. -/
def Control.kill (job : Job) (c : Control) (obs : SpawnKey → Recovery.KillObservation) :
    Job × List (SpawnKey × Recovery.KillAction) :=
  match c.ends with
  | some s =>
    let r := terminalizeRow job s
    (r.1, if r.2 = .won then
      (liveOwnChildren job).map fun ch => (ch.key, Recovery.killAction (obs ch.key))
    else [])
  | none => (job, [])

def defaultMaxWallSecs : Nat := 7200
def maxWallSecsCap : Nat := 86400

/-- The configured `max_wall_secs`, or its default, capped. -/
def wallLimit (configured : Option Nat) : Nat :=
  Nat.min (configured.getD defaultMaxWallSecs) maxWallSecsCap

/-- The deadline is the job's own action: once the wall limit has elapsed the
    row takes the kill path, whatever its children are doing. -/
def deadlineTick (job : Job) (configured : Option Nat) (elapsed : Nat)
    (obs : SpawnKey → Recovery.KillObservation) : Job × List (SpawnKey × Recovery.KillAction) :=
  if wallLimit configured ≤ elapsed then Control.kill job .deadline obs else (job, [])

/-- A child's terminal updates the child view only. -/
def observeChild (job : Job) (k : SpawnKey) (s : ChildState) : Job :=
  { job with children := job.children.map fun ch => if ch.key = k then { ch with state := s } else ch }

/-- `JobStepInput.control`. -/
structure ControlEcho where
  kind : String
  payload : Option Resume
  deriving DecidableEq, Repr

/-- Kind precedence answer > engine_current > resume; the payload is the
    whole host-validated block. -/
def echo (r : Resume) : ControlEcho :=
  { kind := if r.answer.isSome then "answer" else if r.engineCurrent then "engine_current" else "resume"
  , payload := some r }

/-- One terminalizer wins the row; a later one loses and publishes nothing,
    and retrying the winner's publication publishes once. -/
theorem terminal_single_winner (job : Job) (a b : RowState) (h_a : a ≠ .running)
    (st : NotificationState) :
    (terminalizeRow (terminalizeRow job a).1 b) = ((terminalizeRow job a).1, .lost) ∧
      publishCandidate (terminalizeRow (terminalizeRow job a).1 b).2
          (publishCandidate (terminalizeRow job a).2 st) =
        publishCandidate (terminalizeRow job a).2 st ∧
      publishCandidate (terminalizeRow job a).2
          (publishCandidate (terminalizeRow job a).2 st) =
        publishCandidate (terminalizeRow job a).2 st := by
  by_cases h : job.state = .running
  · have h1 : terminalizeRow job a = ({ job with state := a }, .won) := by
      simp [terminalizeRow, h]
    have h2 : terminalizeRow { job with state := a } b = ({ job with state := a }, .lost) := by
      simp [terminalizeRow, h_a]
    rw [h1]
    simp only [h2]
    exact ⟨trivial, losing_terminalizer_does_not_publish _, winning_publication_is_idempotent st⟩
  · have h1 : terminalizeRow job a = (job, .lost) := by simp [terminalizeRow, h]
    have h2 : terminalizeRow job b = (job, .lost) := by simp [terminalizeRow, h]
    rw [h1]
    simp [h2, losing_terminalizer_does_not_publish]

/-- A terminal row is never terminalized again and no control reaches a child
    through it. -/
theorem terminal_admits_nothing (job : Job) (h : job.state ≠ .running) (s : RowState)
    (c : Control) (obs : SpawnKey → Recovery.KillObservation) :
    terminalizeRow job s = (job, .lost) ∧ Control.kill job c obs = (job, []) := by
  refine ⟨by simp [terminalizeRow, h], ?_⟩
  unfold Control.kill
  split <;> simp [h, terminalizeRow]

/-- A kill ends the row through the single-winner terminalizer, and a second
    kill on the row it ended loses and acts on nothing. -/
theorem kill_is_single_winner (job : Job) (c : Control) (s : RowState) (h_c : c.ends = some s)
    (h_s : s ≠ .running) (c' : Control) (obs : SpawnKey → Recovery.KillObservation) :
    (Control.kill job c obs).1 = (terminalizeRow job s).1 ∧
      Control.kill (Control.kill job c obs).1 c' obs = ((Control.kill job c obs).1, []) := by
  have h_end : ∀ j : Job, (Control.kill j c obs).1 = (terminalizeRow j s).1 := by
    intro j; simp [Control.kill, h_c]
  refine ⟨h_end job, ?_⟩
  refine (terminal_admits_nothing _ ?_ s c' obs).2
  rw [h_end]
  by_cases h : job.state = .running <;> simp [terminalizeRow, h, h_s]

/-- A kill acts only on live children this chain caused, and each child's
    session is interruptable by the job's calling session because its recorded
    origin cause (`originCause`, the session's stored `caused_by`) names that
    session. The kill owner reads the observed origin cause; a child whose
    session another session started fails `h_spawned` and is refused by
    `DurableLineage.non_spawner_interrupt_refused`. -/
theorem kill_reaches_only_own_children (job : Job) (c : Control)
    (obs : SpawnKey → Recovery.KillObservation) (originCause : String → Option String)
    (h_spawned : ∀ ch ∈ job.children, ch.key.sessionId ≠ job.callerSession →
      originCause ch.key.sessionId = some job.callerSession)
    (p : SpawnKey × Recovery.KillAction)
    (hp : p ∈ (Control.kill job c obs).2) :
    (∃ ch ∈ job.children, ch.key = p.1 ∧ ch.key.root = job.root ∧ ch.state = .live) ∧
      (p.1.sessionId ≠ job.callerSession →
        DurableLineage.interruptAllowed job.callerSession p.1.sessionId
          (originCause p.1.sessionId) = true) := by
  have h_own : ∃ ch ∈ job.children, ch.key = p.1 ∧ ch.key.root = job.root ∧ ch.state = .live := by
    unfold Control.kill at hp
    split at hp
    · dsimp only at hp
      split at hp
      · simp only [List.mem_map] at hp
        obtain ⟨ch, h_ch, rfl⟩ := hp
        simp only [liveOwnChildren, List.mem_filter, Bool.and_eq_true, beq_iff_eq] at h_ch
        exact ⟨ch, h_ch.1, rfl, h_ch.2.1, h_ch.2.2⟩
      · simp at hp
    · simp at hp
  refine ⟨h_own, fun h_ne => ?_⟩
  obtain ⟨ch, h_ch, h_key, _⟩ := h_own
  rw [← h_key] at h_ne ⊢
  rw [h_spawned ch h_ch h_ne]
  exact DurableLineage.spawner_may_interrupt _ _ h_ne

/-- Stop, pause and deadline end a running row at once, for every child
    observation, including a live local child whose own kill waits on its
    terminal (`kill_waits_only_on_a_local_terminal`). -/
theorem kill_ends_row_now (job : Job) (h : job.state = .running)
    (obs : SpawnKey → Recovery.KillObservation) :
    (Recovery.killAction .causedLiveLocal).endsRowNow = false ∧
      (Control.kill job .stop obs).1.state = .cancelled ∧
      JobState.of (Control.kill job .pause obs).1.state Control.pause.note = .paused ∧
      (Control.kill job .deadline obs).1.state = .timedOut := by
  refine ⟨(Recovery.kill_waits_only_on_a_local_terminal _).2 rfl, ?_⟩
  simp [Control.kill, Control.ends, Control.note, JobState.of, terminalizeRow, h]

/-- Once the capped wall limit (default 7200 s, cap 86400 s) has elapsed, a
    running row times out through the kill path, whatever its children
    observe. -/
theorem deadline_is_job_action (job : Job) (h : job.state = .running)
    (configured : Option Nat) (elapsed : Nat) (h_due : wallLimit configured ≤ elapsed)
    (obs : SpawnKey → Recovery.KillObservation) :
    (deadlineTick job configured elapsed obs).1.state = .timedOut ∧
      wallLimit none = defaultMaxWallSecs ∧ wallLimit configured ≤ maxWallSecsCap := by
  refine ⟨?_, rfl, Nat.min_le_right _ _⟩
  simp [deadlineTick, h_due, Control.kill, Control.ends, terminalizeRow, h]

/-- A child's terminal never ends the job row; only the job's driver does. -/
theorem child_terminal_never_terminalizes_job_row (job : Job) (k : SpawnKey) (s : ChildState) :
    (observeChild job k s).state = job.state := rfl

/-- The committed snapshot of a job kill includes its row and child interrupt
intents. Native `plugin_job.control.kill` must publish these in one existing
ConfigAccess transaction; child wakes cannot observe just the interrupts.
Completion notification delivery retains its separate recoverable owner. -/
structure KillSnapshot where
  job : Job
  interrupts : List (SpawnKey × Recovery.KillAction)

/-- Commit the existing kill decision and exactly its admitted interrupt intents. -/
def commitKill (snapshot : KillSnapshot) (c : Control)
    (obs : SpawnKey → Recovery.KillObservation) : KillSnapshot :=
  let killed := Control.kill snapshot.job c obs
  ⟨killed.1, snapshot.interrupts ++ killed.2⟩

/-- A failed storage transaction publishes neither the terminal row nor interrupts. -/
def killTransaction (snapshot : KillSnapshot) (c : Control)
    (obs : SpawnKey → Recovery.KillObservation) (commits : Bool) : KillSnapshot :=
  if commits then commitKill snapshot c obs else snapshot

/-- Rollback cannot leave interrupted children beside a still-running job. -/
theorem discarded_kill_preserves_snapshot (snapshot : KillSnapshot) (c : Control)
    (obs : SpawnKey → Recovery.KillObservation) :
    killTransaction snapshot c obs false = snapshot := rfl

/-- A competing driver wake after a committed kill loses the terminal compare;
it cannot replace cancellation with its guest's failure. -/
theorem committed_kill_blocks_driver (snapshot : KillSnapshot) (c : Control)
    (state driverState : RowState) (h_control : c.ends = some state)
    (h_terminal : state ≠ .running) (obs : SpawnKey → Recovery.KillObservation) :
    terminalizeRow (commitKill snapshot c obs).job driverState =
      ((commitKill snapshot c obs).job, .lost) := by
  have ended : (Control.kill snapshot.job c obs).1.state ≠ .running := by
    by_cases running : snapshot.job.state = .running
    · simp [Control.kill, h_control, terminalizeRow, running, h_terminal]
    · simp [Control.kill, h_control, terminalizeRow, running]
  exact (terminal_admits_nothing _ ended driverState .stop obs).1

/-- A driver that already won terminalization leaves a later kill no child effects. -/
theorem committed_kill_after_driver_is_inert (snapshot : KillSnapshot)
    (terminal : snapshot.job.state ≠ .running) (c : Control)
    (obs : SpawnKey → Recovery.KillObservation) :
    commitKill snapshot c obs = snapshot := by
  have unchanged := (terminal_admits_nothing snapshot.job terminal .failed c obs).2
  cases snapshot
  simp_all [commitKill]

/-- Interrupting the caller leaves the running job row as it was. -/
theorem caller_interrupt_retains_job (c : ToolExecution.ToolCallContext)
    (h_job : c.operation = .pluginJob) (h_running : c.state = .running)
    (h_background : c.awaitMode = .background) :
    Background.Interrupt.interruptTool c = c :=
  (Background.Interrupt.interrupt_retains_running_detached_job c h_job h_running h_background).1

/-- The guest supplies only the `GuestPin` fields; every other field is
a host fact, whatever the guest wrote. -/
def hostPin (g : GuestPin) (facts : HostFacts) : Pin :=
  { scriptId := g.scriptId, sourceDigest := g.sourceDigest, engineDigest := facts.engineDigest
  , pluginRef := facts.pluginRef, handle := g.handle, cwd := facts.cwd
  , argsDigest := facts.argsDigest, chainRoot := facts.chainRoot, origin := facts.origin
  , agentBudget := g.agentBudget, adoptedAt := facts.adoptedAt, resumeOf := facts.resumeOf
  , replayedFrom := facts.replayedFrom, ceilingAtPin := facts.ceilingAtPin }

/-- The pin of a resume row. The guest supplies the script, source and budget;
    the handle, chain root and `args_digest` come from the chain root's pin,
    and every other field is a host fact. The host sets `facts.resumeOf` to
    the validated resume block's `row_doc_id`, the prior row, and
    `resumeValid` checks that it does. -/
def resumePin (root _prior : Pin) (g : GuestPin) (facts : HostFacts) : Pin :=
  { scriptId := g.scriptId, sourceDigest := g.sourceDigest, engineDigest := facts.engineDigest
  , pluginRef := facts.pluginRef, handle := root.handle, cwd := facts.cwd
  , argsDigest := root.argsDigest, chainRoot := root.chainRoot, origin := facts.origin
  , agentBudget := g.agentBudget, adoptedAt := facts.adoptedAt, resumeOf := facts.resumeOf
  , replayedFrom := facts.replayedFrom, ceilingAtPin := facts.ceilingAtPin }

/-- `root_call`: the chain root's own call arguments. -/
def rootCallOf (root : Job) : RootCall :=
  { argv := root.call.argv, args := root.call.args, agentBudget := root.call.agentBudget }

/-- `job_calls.result` is offered to tool-call invocations, job steps and
    non-job calls alike. A callback has no caller session and so no
    process-control scope; catalog calls are not invocations and never see it. -/
def resultOffered : ToolPolicy.PluginData.Invocation → Bool
  | .toolCall => true
  | .jobStep => true
  | .callback => false

/-- The scope rule shared by `job_calls.result` and resume validation: the
    caller may manage the owner's background calls, the row belongs to the
    same plugin, and it is terminal. -/
def resultReadAuthorized (caller owner : Background.ProcessControl.Scope)
    (samePlugin terminal : Bool) : Bool :=
  Background.ProcessControl.authorized caller owner && samePlugin && terminal

/-- The host's validation of a step-0 `resume` block naming `prior`. Script
    and engine are compared against the prior pin only, and stay fixed unless
    the block asks for `engine: current`; the budget may only rise. The block
    and the pin's `resumeOf` both name `prior`. An unpinned row was never
    adopted and is no resume target. -/
def resumeValid (caller owner : Background.ProcessControl.Scope) (samePlugin : Bool)
    (prior : Job) (g : GuestPin) (facts : HostFacts) (r : Resume) : Bool :=
  resultReadAuthorized caller owner samePlugin (prior.state != .running) &&
    r.rowDoc == prior.rowDoc && facts.resumeOf == some prior.rowDoc &&
    match prior.pin with
    | some p =>
      (r.engineCurrent || (g.scriptId == p.scriptId && facts.engineDigest == p.engineDigest)) &&
        decide (p.agentBudget ≤ g.agentBudget)
    | none => false

/-- The row a validated resume adopts: it continues the chain under the chain
    root's id and handle, carries the prior row's children and spend (the
    prior is the chain head, which `resumeAdmitted` enforces), and records
    the chain root's call and the validated resume block. -/
def resumeRow (root prior row : Job) (r : Resume) : Job :=
  { row with
    root := root.root, handle := root.handle, children := prior.children,
    spent := prior.spent, rootCall := some (rootCallOf root), resume := some r }

/-- `JobStepInput.control` and `root_call`. -/
structure StepEcho where
  control : Option ControlEcho
  rootCall : Option RootCall
  deriving DecidableEq, Repr

/-- What a step of `job` echoes: the resume block and root call its row recorded. -/
def stepEcho (job : Job) : StepEcho :=
  { control := job.resume.map echo, rootCall := job.rootCall }

/-- The highest attempt of `slot` in this chain. -/
def latestAttempt (job : Job) (slot : String) : Option Child :=
  (job.children.filter fun ch => ch.key.root == job.root && ch.key.slot == slot).foldl
    (fun acc ch => match acc with
      | some a => if a.key.attempt < ch.key.attempt then some ch else acc
      | none => some ch)
    none

/-- Re-attempting `slot` on a resumed row: allowed only when its latest
    attempt ended interrupted; the next attempt is a new live child and is
    charged like any spawn. -/
def reattempt (job : Job) (slot : String) : Option (SpawnKey × Job) :=
  match latestAttempt job slot with
  | some ch =>
    if ch.state = .interrupted then
      let k := { ch.key with attempt := ch.key.attempt + 1 }
      some (k, { job with
        children := job.children ++ [{ ch with key := k, state := .live, messages := 0 }],
        spent := job.spent + 1 })
    else none
  | none => none

/-- A resume is admitted only under the prior row's process-control scope,
    for the same plugin, against a terminal prior row. -/
theorem resume_requires_scope_plugin_terminal_prior
    (caller owner : Background.ProcessControl.Scope) (samePlugin : Bool) (prior : Job)
    (g : GuestPin) (facts : HostFacts) (r : Resume)
    (h : resumeValid caller owner samePlugin prior g facts r = true) :
    Background.ProcessControl.authorized caller owner = true ∧ samePlugin = true ∧
      prior.state ≠ .running := by
  simp only [resumeValid, resultReadAuthorized, Bool.and_eq_true, bne_iff_ne] at h
  exact ⟨h.1.1.1.1.1, h.1.1.1.1.2, h.1.1.1.2⟩

/-- A validated resume block names the prior row, and the pin it produces
    records that row as `resumeOf`. -/
theorem resume_names_prior_row
    (caller owner : Background.ProcessControl.Scope) (samePlugin : Bool) (prior : Job)
    (root p : Pin) (g : GuestPin) (facts : HostFacts) (r : Resume)
    (h : resumeValid caller owner samePlugin prior g facts r = true) :
    r.rowDoc = prior.rowDoc ∧ (resumePin root p g facts).resumeOf = some prior.rowDoc := by
  simp only [resumeValid, Bool.and_eq_true, beq_iff_eq] at h
  exact ⟨h.1.1.2, h.1.2⟩

/-- Without `engine: current`, a validated resume pins the prior script and
    engine digest. -/
theorem resume_script_engine_immutable_unless_engine_current
    (caller owner : Background.ProcessControl.Scope) (samePlugin : Bool) (prior : Job)
    (root p : Pin) (g : GuestPin) (facts : HostFacts) (r : Resume)
    (h : resumeValid caller owner samePlugin prior g facts r = true)
    (h_pin : prior.pin = some p) (h_engine : r.engineCurrent = false) :
    (resumePin root p g facts).scriptId = p.scriptId ∧
      (resumePin root p g facts).engineDigest = p.engineDigest := by
  simp only [resumeValid, h_pin, h_engine, Bool.false_or, Bool.and_eq_true, beq_iff_eq] at h
  exact ⟨h.2.1.1, h.2.1.2⟩

/-- A validated resume never lowers the agent budget. -/
theorem resume_budget_only_rises
    (caller owner : Background.ProcessControl.Scope) (samePlugin : Bool) (prior : Job)
    (root p : Pin) (g : GuestPin) (facts : HostFacts) (r : Resume)
    (h : resumeValid caller owner samePlugin prior g facts r = true)
    (h_pin : prior.pin = some p) :
    p.agentBudget ≤ (resumePin root p g facts).agentBudget := by
  simp only [resumeValid, h_pin, Bool.and_eq_true, decide_eq_true_eq] at h
  exact h.2.2

/-- Spend carries over to the resumed row and only grows on a re-attempt. -/
theorem resume_spent_monotone (root prior row : Job) (r : Resume) (slot : String) (k : SpawnKey)
    (job : Job) (h : reattempt (resumeRow root prior row r) slot = some (k, job)) :
    prior.spent = (resumeRow root prior row r).spent ∧ prior.spent ≤ job.spent := by
  refine ⟨rfl, ?_⟩
  unfold reattempt at h
  split at h
  · split at h
    · simp only [Option.some.injEq, Prod.mk.injEq] at h
      rw [← h.2]
      exact Nat.le_succ _
    · simp at h
  · simp at h

/-- A re-attempt is charged one spawn. -/
theorem reattempt_charges (job : Job) (slot : String) (k : SpawnKey) (job' : Job)
    (h : reattempt job slot = some (k, job')) : job'.spent = job.spent + 1 := by
  unfold reattempt at h
  split at h
  · split at h
    · simp only [Option.some.injEq, Prod.mk.injEq] at h
      rw [← h.2]
    · simp at h
  · simp at h

/-- The resume row's pin and row keep the chain root's handle and chain root. -/
theorem resume_inherits_handle_and_chain_root (root prior : Pin) (g : GuestPin)
    (facts : HostFacts) (rootJob priorJob row : Job) (r : Resume) :
    (resumePin root prior g facts).handle = root.handle ∧
      (resumePin root prior g facts).chainRoot = root.chainRoot ∧
      (resumeRow rootJob priorJob row r).handle = rootJob.handle ∧
      (resumeRow rootJob priorJob row r).root = rootJob.root :=
  ⟨rfl, rfl, rfl, rfl⟩

/-- Whatever digest the resume verb's own arguments have, the pin keeps the
    chain root's `args_digest`, and the step echo keeps the chain root's call. -/
theorem resume_args_digest_from_chain_root (root prior : Pin) (g : GuestPin) (facts : HostFacts)
    (d : String) (rootJob priorJob row : Job) (r : Resume) (args : CallArgs) :
    (resumePin root prior g { facts with argsDigest := d }).argsDigest = root.argsDigest ∧
      (resumeRow rootJob priorJob { row with call := args } r).rootCall = some (rootCallOf rootJob) :=
  ⟨rfl, rfl⟩

/-- A slot is re-attempted only when its latest attempt in this chain ended
    interrupted, and the new key is the next attempt of that slot. -/
theorem resume_reattempts_only_terminal_slots (job : Job) (slot : String) (k : SpawnKey)
    (job' : Job) (h : reattempt job slot = some (k, job')) :
    ∃ ch, latestAttempt job slot = some ch ∧ ch.state = .interrupted ∧
      k = { ch.key with attempt := ch.key.attempt + 1 } := by
  unfold reattempt at h
  split at h
  · rename_i ch h_latest
    split at h
    · rename_i h_int
      simp only [Option.some.injEq, Prod.mk.injEq] at h
      exact ⟨ch, h_latest, h_int, h.1.symm⟩
    · simp at h
  · simp at h

/-- Every step of a resumed row echoes the resume block and the chain root's
    call: the row records both at adoption, and child observations,
    re-attempts and kills keep them. -/
theorem resume_echoed_every_step (root prior row : Job) (r : Resume) :
    stepEcho (resumeRow root prior row r) =
        { control := some (echo r), rootCall := some (rootCallOf root) } ∧
      (∀ (job : Job) k s, stepEcho (observeChild job k s) = stepEcho job) ∧
      (∀ (job : Job) slot k job', reattempt job slot = some (k, job') →
        stepEcho job' = stepEcho job) ∧
      (∀ (job : Job) c obs, stepEcho (Control.kill job c obs).1 = stepEcho job) := by
  refine ⟨rfl, fun _ _ _ => rfl, ?_, ?_⟩
  · intro job slot k job' h
    unfold reattempt at h
    split at h
    · split at h
      · simp only [Option.some.injEq, Prod.mk.injEq] at h
        rw [← h.2]
        rfl
      · simp at h
    · simp at h
  · intro job c obs
    unfold Control.kill
    split
    · unfold terminalizeRow
      split <;> rfl
    · rfl

/-- `job_calls.result` needs the caller's process-control scope over the row,
    the same plugin, and a terminal row. -/
theorem result_read_requires_scope_plugin_terminal
    (caller owner : Background.ProcessControl.Scope) (samePlugin terminal : Bool) :
    resultReadAuthorized caller owner samePlugin terminal = true ↔
      Background.ProcessControl.authorized caller owner = true ∧ samePlugin = true ∧
        terminal = true := by
  simp [resultReadAuthorized, Bool.and_assoc]

/-- Only tool-call invocations are offered `job_calls.result`. -/
theorem result_offered_only_on_tool_calls (inv : ToolPolicy.PluginData.Invocation) :
    resultOffered inv = true ↔ inv ∈ [.toolCall, .jobStep] := by
  cases inv <;> simp [resultOffered]

end PluginJob
