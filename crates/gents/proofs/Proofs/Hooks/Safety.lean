import Proofs.Hooks.MustCommit
import Proofs.Hooks.Prepared

namespace Hooks.Safety

/-- The part of a running request a monitor can affect. `actor` is the DID the
request executes under; `interrupted` is the request's interrupt latch
(`interrupt_requested_at`) and `stopped` the session stop latch, which the
existing control model distinguishes (`SessionQueue.Control`). The counters
stand for work: provider turns started, tool calls dispatched, later requests
of the session claimed. -/
structure Run where
  request : Nat
  actor : Actor
  interrupted : Bool
  stopped : Bool
  turns : Nat
  dispatched : Nat
  claimed : Nat
  deriving DecidableEq, Repr

/-- The stop operations a plugin may request as an effect: interrupt the
request, or stop its session. There is no resume in this vocabulary, so a
plugin can ask for work to stop and cannot ask for stopped work to continue
(maintainer decision, gents-ai/gents#3656). Resume stays an operator control
that core alone performs. -/
inductive StopOp where
  | interrupt (request : Nat) (reason : String)
  | stop (request : Nat) (reason : String)
  deriving DecidableEq, Repr

def StopOp.request : StopOp → Nat
  | .interrupt request _ => request
  | .stop request _ => request

/-- Core performs a requested stop under its own rule (`mayStop`, document ACP
for the acting DID on the request) and only for the subject named.

Design choice, authority of a stop requested from a delivery. A delivery runs
after commit, so there is no caller's transaction. It runs under the actor
recorded with the owed delivery, which is the actor of the transaction that
owed it: here the DID the request executes under. The alternatives were a
plugin identity or the node's own identity; both would give a plugin authority
no caller handed it. With the recorded actor, a delivery can stop only what
that actor could already stop, and core further limits it to the subject the
delivery is about. Stopping is a narrowing: it removes future actions of the
request and grants none. -/
def perform (mayStop : Actor → Run → Bool) (actor : Actor) (op : StopOp) (run : Run) :
    Option Run :=
  if op.request = run.request ∧ mayStop actor run = true then
    match op with
    | .interrupt _ _ => some { run with interrupted := true }
    | .stop _ _ => some { run with interrupted := true, stopped := true }
  else none

/-- Core's next-action transitions. Each re-reads the latch in its own
transaction: a new provider turn, a tool-call dispatch (the existing fence on
`interrupt_requested_at` in `tools/call/lifecycle/delivery/dispatch.rs`,
expressed as the cancellation input of `Hooks.admit`), and the claim of a
later request of a stopped session. -/
def startTurn (run : Run) : Option Run :=
  if run.interrupted then none else some { run with turns := run.turns + 1 }

def dispatch {Subject Payload : Type} [DecidableEq Subject] (authorized : Bool)
    (current : Binding Subject) (prepared : Option (Prepared Subject Payload)) (run : Run) :
    Option Run :=
  match admit authorized run.interrupted current prepared with
  | .ok _ => some { run with dispatched := run.dispatched + 1 }
  | .error _ => none

def claimNext (run : Run) : Option Run :=
  if run.stopped then none else some { run with claimed := run.claimed + 1 }

inductive Decision where
  | allow
  | stop (reason : String)
  deriving DecidableEq, Repr

/-- A safety monitor: a decision model that watches what an agent produced.
Its policy is abstract. The model call is an external effect, so the verdict
is prepared outside any transaction; the delivery transaction only applies
it. `wholeSession` selects the session stop over the request interrupt.

How it is expressed in the contract:
* it observes at `request.output.recorded`, a delivery owed by the transaction
  that durably records a message or a tool-call terminal. It reads durable
  output and is never inside the owned loop, which stays the sole
  provider-input boundary;
* it prevents the next tool call with a prepared decision at
  `tool_call.dispatch.decide`, where a missing or unavailable verdict never
  admits;
* it stops running work by requesting `StopOp` from its delivery.

What the contract cannot give a monitor:
* it acts at boundaries. Tokens already streamed to a client, and the rest of
  a turn in progress when the output was recorded, are not recalled;
* there is no refuse-to-continue point for the next provider turn. Between an
  output being recorded and the monitor's interrupt committing, the loop may
  start another turn; only tool calls are held, by the dispatch decision. A
  turn gate would be a new prepared-decision point and is not modelled;
* a tool call already running when the interrupt commits is ended by core's
  existing interrupt handling, not by this contract;
* delivery is at least once and can be late or failing. While it fails, tool
  dispatch is closed (no verdict) but turns are open;
* an operator resume is outside the plugin's control by design. -/
structure Monitor (Observation : Type) where
  verdict : Observation → Decision
  wholeSession : Bool

variable {Observation : Type}

def manifest : Manifest := { name := "safety", owns := [], bound := 1 }

/-- The delivery's event is built by the host: the subject request and what
was recorded. -/
def plugin (monitor : Monitor Observation) :
    Plugin Unit (Nat × Observation) Unit Unit Unit StopOp :=
  { manifest := manifest
    hook :=
      { reads := fun _ => []
        decide := fun event _ =>
          match monitor.verdict event.2 with
          | .allow => .proceed () []
          | .stop reason =>
              .proceed () [.core (if monitor.wholeSession then .stop event.1 reason
                else .interrupt event.1 reason)] } }

def host (mayStop : Actor → Run → Bool) : Host Run Unit Unit Unit StopOp :=
  { select := fun _ _ _ => [], perform := perform mayStop, allowed := fun _ _ _ => false }

/-- The monitor's delivery: its own transaction, containing only the monitor,
under the actor recorded for the subject. Another plugin's delivery is another
transaction, so no other plugin's refusal can abort this one. -/
def observe (mayStop : Actor → Run → Bool) (monitor : Monitor Observation)
    (observation : Observation) (state : State Run Unit Unit) :
    Except Refusal (State Run Unit Unit × List Unit) :=
  stage (host mayStop) [plugin monitor] state.core.actor (state.core.request, observation) state

theorem stop_request_is_limited_to_its_subject (mayStop : Actor → Run → Bool) (actor : Actor)
    (op : StopOp) (run : Run) (other : op.request ≠ run.request) :
    perform mayStop actor op run = none := by
  simp [perform, other]

theorem stop_request_needs_the_actors_own_authority (mayStop : Actor → Run → Bool)
    (actor : Actor) (op : StopOp) (run : Run) (denied : mayStop actor run = false) :
    perform mayStop actor op run = none := by
  simp [perform, denied]

private theorem perform_only_stops (mayStop : Actor → Run → Bool) (actor : Actor) (op : StopOp)
    (run next : Run) (performed : perform mayStop actor op run = some next) :
    next.interrupted = true ∧ (run.stopped = true → next.stopped = true) ∧
      next.turns = run.turns ∧ next.dispatched = run.dispatched ∧
      next.claimed = run.claimed := by
  unfold perform at performed
  split at performed
  · cases op <;> simp only [Option.some.injEq] at performed <;> subst performed <;> simp
  · cases performed

private theorem reach_only_stops (mayStop : Actor → Run → Bool) (actor : Actor)
    (run final : Run) (reached : Reach (perform mayStop) actor run final) :
    (run.interrupted = true → final.interrupted = true) ∧
      (run.stopped = true → final.stopped = true) ∧
      final.turns = run.turns ∧ final.dispatched = run.dispatched ∧
      final.claimed = run.claimed := by
  induction reached with
  | refl => simp
  | step op performed _ ih =>
    obtain ⟨latched, stopped, turns, dispatched, claimed⟩ :=
      perform_only_stops mayStop actor op _ _ performed
    obtain ⟨keeps, keepsStop, sameTurns, sameDispatched, sameClaimed⟩ := ih
    exact ⟨fun _ => keeps latched, fun was => keepsStop (stopped was),
      sameTurns.trans turns, sameDispatched.trans dispatched, sameClaimed.trans claimed⟩

/-- If the monitor answers stop at a boundary, its delivery commits the
interrupt, given only that the request's own actor may interrupt it. -/
theorem stop_verdict_commits_the_interrupt (mayStop : Actor → Run → Bool)
    (monitor : Monitor Observation) (observation : Observation) (state : State Run Unit Unit)
    (reason : String) (answered : monitor.verdict observation = .stop reason)
    (permitted : mayStop state.core.actor state.core = true) :
    ∃ final, observe mayStop monitor observation state = .ok (final, [()]) ∧
      final.core.interrupted = true ∧
      (monitor.wholeSession = true → final.core.stopped = true) := by
  cases whole : monitor.wholeSession with
  | true =>
    refine ⟨{ state with core := { state.core with interrupted := true, stopped := true } },
      ?_, rfl, fun _ => rfl⟩
    simp [observe, stage, stageFrom, Hook.run, plugin, answered, whole, valid, manifest,
      Effect.permitted, applyEffects, applyEffect, host, perform, permitted, StopOp.request]
  | false =>
    refine ⟨{ state with core := { state.core with interrupted := true } }, ?_, rfl, ?_⟩
    · simp [observe, stage, stageFrom, Hook.run, plugin, answered, whole, valid, manifest,
        Effect.permitted, applyEffects, applyEffect, host, perform, permitted, StopOp.request]
    · intro impossible
      cases impossible

/-- Once the interrupt has committed, no later turn of the request starts. -/
theorem interrupted_request_starts_no_turn (run : Run) (latched : run.interrupted = true) :
    startTurn run = none := by
  simp [startTurn, latched]

/-- Once the interrupt has committed, no later tool call of the request is
dispatched, whatever any plugin prepared and whatever core authorized. -/
theorem interrupted_request_dispatches_nothing {Subject Payload : Type} [DecidableEq Subject]
    (authorized : Bool) (current : Binding Subject)
    (prepared : Option (Prepared Subject Payload)) (run : Run)
    (latched : run.interrupted = true) : dispatch authorized current prepared run = none := by
  cases authorized <;> simp [dispatch, admit, latched]

theorem stopped_session_claims_nothing (run : Run) (latched : run.stopped = true) :
    claimNext run = none := by
  simp [claimNext, latched]

/-- Tool calls are closed until the monitor has answered for the binding the
dispatch observes: no verdict is not permission. -/
theorem undecided_dispatch_is_closed {Subject Payload : Type} [DecidableEq Subject]
    (authorized : Bool) (current : Binding Subject) (run : Run) :
    dispatch authorized current (none : Option (Prepared Subject Payload)) run = none := by
  cases authorized <;> cases latched : run.interrupted <;> simp [dispatch, admit, latched]

/-- No other plugin can undo a stop, at a point where refusal is allowed:
whatever any plugins stage, the latches stay set and no work is added. -/
theorem staging_cannot_undo_a_stop {Q Ev V Doc Val : Type} [DecidableEq Doc]
    (mayStop : Actor → Run → Bool) (select : Run → Store Doc Val → Q → List (Row Doc Val))
    (allowed : Acp Doc) (plugins : List (Plugin Q Ev V Doc Val StopOp)) (actor : Actor)
    (event : Ev) (state final : State Run Doc Val) (values : List V)
    (ok : stage ⟨select, perform mayStop, allowed⟩ plugins actor event state =
      .ok (final, values)) :
    (state.core.interrupted = true → final.core.interrupted = true) ∧
      (state.core.stopped = true → final.core.stopped = true) ∧
      final.core.turns = state.core.turns ∧ final.core.dispatched = state.core.dispatched ∧
      final.core.claimed = state.core.claimed :=
  reach_only_stops mayStop actor _ _
    (staging_moves_core_only_through_perform ⟨select, perform mayStop, allowed⟩ plugins actor
      event state final values ok)

/-- No other plugin can prevent a stop: the interrupt is a must-commit
transition, so it commits with the latch set whatever the listeners at that
point answer, refuse, or fail to read. -/
theorem no_plugin_can_prevent_the_interrupt {Q Ev V Doc Val : Type} [DecidableEq Doc]
    (mayStop : Actor → Run → Bool) (select : Run → Store Doc Val → Q → List (Row Doc Val))
    (allowed : Acp Doc) (plugins : List (Plugin Q Ev V Doc Val StopOp)) (actor : Actor)
    (event : Run → Run → Ev) (readFailed : String → Bool) (state : State Run Doc Val) :
    ∃ final dropped,
      mustCommit ⟨select, perform mayStop, allowed⟩ plugins actor
        (fun run => some { run with interrupted := true }) event readFailed state =
        some (final, dropped) ∧
      final.core.interrupted = true := by
  obtain ⟨final, dropped, committed⟩ := plugins_cannot_block_a_must_commit_transition
    ⟨select, perform mayStop, allowed⟩ plugins actor
    (fun run => some { run with interrupted := true }) event readFailed state _ rfl
  obtain ⟨next, transition, reached⟩ := must_commit_keeps_core_transition _ plugins actor _
    event readFailed state final dropped committed
  simp only [Option.some.injEq] at transition
  subst transition
  exact ⟨final, dropped, committed,
    (reach_only_stops mayStop actor _ _ reached).1 rfl⟩

/-- The safety property: a stop verdict at a boundary commits; from then on
no turn starts and no tool call is dispatched for that request. -/
theorem stop_verdict_ends_the_requests_actions {Subject Payload : Type} [DecidableEq Subject]
    (mayStop : Actor → Run → Bool) (monitor : Monitor Observation) (observation : Observation)
    (state : State Run Unit Unit) (reason : String)
    (answered : monitor.verdict observation = .stop reason)
    (permitted : mayStop state.core.actor state.core = true) :
    ∃ final, observe mayStop monitor observation state = .ok (final, [()]) ∧
      startTurn final.core = none ∧
      ∀ (authorized : Bool) (current : Binding Subject)
        (prepared : Option (Prepared Subject Payload)),
        dispatch authorized current prepared final.core = none := by
  obtain ⟨final, committed, latched, _⟩ := stop_verdict_commits_the_interrupt mayStop monitor
    observation state reason answered permitted
  exact ⟨final, committed, interrupted_request_starts_no_turn _ latched,
    fun authorized current prepared =>
      interrupted_request_dispatches_nothing authorized current prepared _ latched⟩

end Hooks.Safety
