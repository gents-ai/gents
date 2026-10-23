import Proofs.ToolPolicy.Meet

/-!
# Plugin jobs: the executable source of steps, pins and admission

Decision record (AGENTS.md ownership amendment): a plugin job is an ordinary
background tool call; its pin and progress are fields on that call; guest notes
are pack data; it is not a TaskRun lifecycle or a new core action journal.

Sandbox note (#378, #835): "no interpreter inside the runtime" holds because
the workflow interpreter runs inside the plugin sandbox, never in core. The
guest is arbitrary; every core theorem quantifies over every step function.

`JobState` is the row-level vocabulary on the wire (`JobProgress.state`,
`JobStepInput.jobs[].state`, `JobView.state`, `JobResultResponse.state`).
`RowState` is the persisted lifecycle: a paused or awaiting-user row is
`completed` there, and `JobState.of` with its `TerminalNote` tells a resumable
suspension from a finished run. `ChildState` is the vocabulary of
`JobStepInput.children[].state` and `JobView.children[].state`.
-/

namespace PluginJob

open ToolPolicy

abbrev DocId := String

/-- AgentTarget id resolved by the host to `(target_node_did, agent_id)`. -/
abbrev Target := String × String

instance : DecidableEq (EndpointScope Target Unit)
  | .none, .none => isTrue rfl
  | .all, .all => isTrue rfl
  | .only ka va, .only kb vb =>
      if h : ka = kb then
        isTrue (by
          subst h
          have : va = vb := funext fun _ => rfl
          subst this
          rfl)
      else isFalse (fun e => h (by injection e))
  | .none, .all => isFalse (by intro e; cases e)
  | .none, .only _ _ => isFalse (by intro e; cases e)
  | .all, .none => isFalse (by intro e; cases e)
  | .all, .only _ _ => isFalse (by intro e; cases e)
  | .only _ _, .none => isFalse (by intro e; cases e)
  | .only _ _, .all => isFalse (by intro e; cases e)

/-- Scope inclusion: `a ≤ b` when every key `a` permits, `b` permits. -/
def scopeLe : EndpointScope Target Unit → EndpointScope Target Unit → Bool
  | .none, _ => true
  | _, .all => true
  | .only ka _, .only kb _ => decide (ka ⊆ kb)
  | _, _ => false

structure Ceiling where
  targets : EndpointScope Target Unit
  maxLive : Nat
  maxSpawns : Nat
  maxMessagesPerSlot : Nat
  resultBytes : Nat
  maxWallSecs : Nat
  deriving DecidableEq

/-- Absent block: no targets, every bound zero. -/
def Ceiling.empty : Ceiling :=
  { targets := .none, maxLive := 0, maxSpawns := 0, maxMessagesPerSlot := 0
  , resultBytes := 0, maxWallSecs := 0 }

def Ceiling.meet (a b : Ceiling) : Ceiling :=
  { targets := a.targets.meet unitVM b.targets
  , maxLive := Nat.min a.maxLive b.maxLive
  , maxSpawns := Nat.min a.maxSpawns b.maxSpawns
  , maxMessagesPerSlot := Nat.min a.maxMessagesPerSlot b.maxMessagesPerSlot
  , resultBytes := Nat.min a.resultBytes b.resultBytes
  , maxWallSecs := Nat.min a.maxWallSecs b.maxWallSecs }

/-- Fieldwise order; `JobGrant.narrows` compares stored ceilings with it. -/
def Ceiling.leB (a b : Ceiling) : Bool :=
  scopeLe a.targets b.targets && decide (a.maxLive ≤ b.maxLive)
    && decide (a.maxSpawns ≤ b.maxSpawns)
    && decide (a.maxMessagesPerSlot ≤ b.maxMessagesPerSlot)
    && decide (a.resultBytes ≤ b.resultBytes)
    && decide (a.maxWallSecs ≤ b.maxWallSecs)

instance : LE Ceiling := ⟨fun a b => a.leB b = true⟩

instance (a b : Ceiling) : Decidable (a ≤ b) :=
  inferInstanceAs (Decidable (a.leB b = true))

inductive Origin | installed | dynamic
  deriving DecidableEq, Repr

inductive DynamicMode | off | check | run
  deriving DecidableEq, Repr

structure Policy where
  mode : DynamicMode
  dynamicTargets : EndpointScope Target Unit
  trustedPacks : List String
  deriving DecidableEq

/-- The pin, field for field. -/
structure Pin where
  (scriptId sourceDigest engineDigest pluginRef handle cwd argsDigest chainRoot : String)
  origin : Origin
  (agentBudget adoptedAt : Nat)
  (resumeOf replayedFrom : Option DocId)
  ceilingAtPin : Ceiling
  deriving DecidableEq

/-- The only guest-supplied pin fields; the host overwrites the rest. -/
structure GuestPin where
  (scriptId sourceDigest handle : String)
  agentBudget : Nat
  deriving DecidableEq, Repr

/-- Every `Pin` field outside `GuestPin`. -/
structure HostFacts where
  (engineDigest pluginRef cwd argsDigest chainRoot : String)
  origin : Origin
  adoptedAt : Nat
  (resumeOf replayedFrom : Option DocId)
  ceilingAtPin : Ceiling
  deriving DecidableEq

/-- Every call argument, echoed verbatim on every step. -/
structure CallArgs where
  argv : List String
  (args script : Option String)
  agentBudget : Option Nat
  deriving DecidableEq, Repr

/-- `root_call`. -/
structure RootCall where
  argv : List String
  args : Option String
  agentBudget : Option Nat
  deriving DecidableEq, Repr

structure Resume where
  rowDoc : DocId
  engineCurrent : Bool
  answer : Option String
  budget : Option Nat
  deriving DecidableEq, Repr

inductive ControlKind | stop | pause
  deriving DecidableEq, Repr

/-- The one closed enum shared by `suspend.reason` and `JobProgress.pause_reason`. -/
inductive PauseReason | user | backOff | noProgress | verification | infra | budgetExceeded
  deriving DecidableEq, Repr

/-- Riders a terminal output may carry. -/
structure Extras where
  writes : Nat
  scratch : List String
  progress : Bool
  deriving DecidableEq, Repr

/-- `Keys.lean` adds the encoders. -/
structure SpawnKey where
  root : DocId
  slot : String
  attempt : Nat
  deriving DecidableEq, Repr

/-- The guest chooses `slot` as `"<seq>:<hash>"`, at most 128 bytes. -/
structure Spawn where
  slot : String
  attempt : Nat
  target : Target
  cwd : Option String
  deriving DecidableEq, Repr

structure Effects where
  pin : Option GuestPin
  resume : Option Resume
  spawns : List Spawn
  (messages interrupts : List String)
  extras : Extras
  deriving DecidableEq, Repr

inductive SuspendKind | pause | awaitUser
  deriving DecidableEq, Repr

inductive StepOutput
  /-- `control` only on non-job calls; `pin` only on step 0 (adopt and
  terminalize); `resume` only beside `pin`. -/
  | result (x : Extras) (control : List (DocId × ControlKind))
      (pin : Option GuestPin) (resume : Option Resume)
  /-- `x.scratch = []`: `fail` has no scratch. -/
  | fail (x : Extras)
  /-- `pin` and `resume` as on `result`. -/
  | suspend (k : SuspendKind) (reason : Option PauseReason) (x : Extras)
      (pin : Option GuestPin) (resume : Option Resume)
  | effects (e : Effects)
  deriving DecidableEq, Repr

inductive ChildState | live | completed | failed | interrupted | other
  deriving DecidableEq, Repr

structure Child where
  key : SpawnKey
  target : Target
  state : ChildState
  messages : Nat
  deriving DecidableEq, Repr

inductive RowState | running | completed | failed | cancelled | timedOut
  deriving DecidableEq, Repr

/-- The row's terminal output was a suspend (pause or await_user), or a pause control. -/
inductive TerminalNote
  | none
  | paused (r : Option PauseReason)
  | awaitingUser
  deriving DecidableEq, Repr

/-- `JobProgress.state`. -/
inductive JobState | running | paused | awaitingUser | completed | failed | cancelled | timedOut
  deriving DecidableEq, Repr

/-- completed + paused ↦ paused; completed + awaitingUser ↦ awaitingUser;
otherwise the row state. -/
def JobState.of : RowState → TerminalNote → JobState
  | .completed, .paused _ => .paused
  | .completed, .awaitingUser => .awaitingUser
  | .completed, .none => .completed
  | .running, _ => .running
  | .failed, _ => .failed
  | .cancelled, _ => .cancelled
  | .timedOut, _ => .timedOut

theorem job_state_distinguishes_suspensions (r : Option PauseReason) :
    JobState.of .completed (.paused r) ≠ JobState.of .completed .none
      ∧ JobState.of .completed .awaitingUser ≠ JobState.of .completed .none := by
  simp [JobState.of]

structure Job where
  (root rowDoc callerSession callerCwd handle : String)
  callerHop : Nat
  call : CallArgs
  /-- On a validated resume only. -/
  rootCall : Option RootCall
  /-- On a validated resume only: the step-0 resume block. -/
  resume : Option Resume
  pin : Option Pin
  children : List Child
  (spent observedTerminal consecutiveFailures : Nat)
  state : RowState
  deriving DecidableEq

end PluginJob
