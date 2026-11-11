import Proofs.Session.Hooks.Claim
import Proofs.Automation.Triggers.Durable
import Proofs.Automation.GoalAutomation.RequestAssignment

namespace Hooks.GoalTask

open Triggers.Durable

/-- The core-owned part of `Triggers.Durable.State`: fire receipts, requests
with their TriggerFire flags, and published outcomes. Triggers and Tasks are
core; the Goal bindings are the plugin's. -/
structure TaskCore where
  receipts : List Identity
  requests : List Request
  outcomes : List Identity
  deriving DecidableEq, Repr

def coreOf (state : Triggers.Durable.State) : TaskCore :=
  ⟨state.receipts, state.requests, state.outcomes⟩

def join (core : TaskCore) (goals : List GoalBinding) : Triggers.Durable.State :=
  { receipts := core.receipts, requests := core.requests, outcomes := core.outcomes,
    goals := goals }

abbrev GoalDoc := String × String × Identity

def collection : String := "Goal"

def key (goal : GoalBinding) : GoalDoc := (goal.owner, goal.session, goal.assignment)

def encode (goal : GoalBinding) : Row GoalDoc Goals.Status :=
  ⟨⟨collection, key goal⟩, 0, goal.status⟩

def decode (row : Row GoalDoc Goals.Status) : GoalBinding :=
  { owner := row.ref.doc.1, session := row.ref.doc.2.1, assignment := row.ref.doc.2.2,
    status := row.value }

/-- The core operations Task Goal application needs. Both are findings: they
were things Task Goal application did to core-owned rows, coupling that had
to move for the contract to suffice; the plugin is now
`goal::lifecycle_hooks::TaskGoalAssignment` at `request.claim.applied`.

`assignmentApplied` is the write of `TriggerFire.goal_assignment_applied` and
the replacement mark on earlier fires of the session. The flag is a triggers
field written by goal code (`goal/task_fire.rs`); core must own the write and
offer it as this operation, or the flag must move to a Goal-owned row.

`settleOutcomes` is FireOutcome publication. `Triggers.Durable.outcomeReason`
classifies a fire by reading Goal status (`boundGoal`), and the native
classifier reads Goal rows (`triggers/engine/durable.rs`): core reads plugin
state to make its own decision. Under the contract core cannot name the Goal
collection, so the plugin reports the bindings it holds and core classifies
against that report. The cleaner end state is for Goals to request an outcome
when a Goal ends, so core never needs the view; the existing model's
`recoverOutcomes` is a sweep over every request and does not allow that
narrowing without changing its statement.

Decision 2026-11-11, maintainer-approved: both are core operations registered
by triggers (`trigger.goal_assignment.apply`, `trigger.goal_outcomes.settle`).
The native plugin additionally asks core to supersede the session's pending
continuations the new assignment makes stale (`Hooks.supersedeMatching`), creates
the first Goal with its session-child grants (`Hooks.sessionChildGrants`), and
reads the session token-usage baseline as a named core read
(`Hooks.Read.core`). The Durable model has no pending continuations, request
ACP or usage, so this slice is unchanged by them. -/
inductive TaskOp where
  | settleOutcomes (goals : List GoalBinding)
  | assignmentApplied (fire : Identity)
  deriving DecidableEq, Repr

def perform (_ : Actor) : TaskOp → TaskCore → Option TaskCore
  | .settleOutcomes goals, core => some (coreOf (recoverOutcomes (join core goals)))
  | .assignmentApplied fire, core =>
      (core.requests.find? fun request => request.fire.identity == fire).map fun candidate =>
        coreOf (stageAssignment (join core []) candidate)

def sameSession (candidate : Request) (goal : GoalBinding) : Bool :=
  goal.owner == candidate.fire.identity.owner && goal.session == candidate.fire.session

def binding (candidate : Request) : GoalBinding :=
  { owner := candidate.fire.identity.owner, session := candidate.fire.session,
    assignment := candidate.fire.identity }

def assignmentEffects (candidate : Request) (shown : List (Row GoalDoc Goals.Status)) :
    List (Effect GoalDoc Goals.Status TaskOp) :=
  [.core (.settleOutcomes (shown.map decode))] ++
    ((shown.filter fun row => sameSession candidate (decode row)).map fun row =>
      .write (.delete row.ref row.revision)) ++
    [.write (.create (encode (binding candidate)).ref (binding candidate).status),
     .core (.assignmentApplied candidate.fire.identity),
     .core (.settleOutcomes
       (binding candidate :: (shown.map decode).filter fun goal => !sameSession candidate goal))]

/-- Task-assigned Goal application at `request.claim.applied` as a plugin
policy: replace the session's Goal binding with one for the claimed fire, and
ask core to mark the fire applied and settle outcomes before and after.

Finding: the existing model settles outcomes for every request against every
Goal, so the plugin declares all Goal rows. The Rust publishes only for the
session's previous Goal (`publish_goal_outcomes`). -/
def applyTaskGoal (claimed : Option Request) (rows : List (List (Row GoalDoc Goals.Status))) :
    Outcome Unit GoalDoc Goals.Status TaskOp :=
  match claimed, rows with
  | some candidate, [shown] =>
      if candidate.fire.goalBacked then .proceed () (assignmentEffects candidate shown)
      else .proceed () []
  | _, _ => .proceed () []

def manifest (bound : Nat) : Manifest := { name := "goals", owns := [collection], bound := bound }

def plugin (bound : Nat) : Plugin Unit (Option Request) Unit GoalDoc Goals.Status TaskOp :=
  { manifest := manifest bound, hook := { reads := fun _ => [()], decide := applyTaskGoal } }

/-- Core's side for the slice. Document ACP is not part of the existing
trigger model, so every write is permitted here; authority is covered by the
contract's own theorems. -/
def host : Host TaskCore Unit GoalDoc Goals.Status TaskOp :=
  { select := fun _ docs _ => docs, perform := perform, allowed := fun _ _ _ => true }

/-- Core's claim for the slice: the queue gate and the running mark of
`Triggers.Durable.claim`, without the Goal step. -/
def claimCore (fire : Identity) : ClaimCore TaskCore :=
  { claim := fun core =>
      if canClaim (join core []) fire then
        some { core with requests := core.requests.map fun request =>
          if request.fire.identity == fire then { request with running := true } else request }
      else none
    retire := fun _ _ => none
    reject := fun _ _ => none }

def embed (state : Triggers.Durable.State) : Hooks.State TaskCore GoalDoc Goals.Status :=
  ⟨coreOf state, state.goals.map encode⟩

def project (state : Hooks.State TaskCore GoalDoc Goals.Status) : Triggers.Durable.State :=
  join state.core (state.docs.map decode)

/-- The claim transaction of the slice, composed from core's claim and the
Goals plugin through the contract. -/
def composedClaim (bound : Nat) (state : Triggers.Durable.State) (fire : Identity) :
    Triggers.Durable.State :=
  project (settle (embed state)
    (claimStep host [] [plugin bound] "runtime" (fun _ => ())
      (fun _ next => next.requests.find? fun request => request.fire.identity == fire)
      (claimCore fire) (embed state)))

theorem decode_encode (goal : GoalBinding) : decode (encode goal) = goal := rfl

theorem decode_map_encode (goals : List GoalBinding) : (goals.map encode).map decode = goals := by
  induction goals with
  | nil => rfl
  | cons goal rest ih => simp [decode_encode, ih]

theorem project_embed (state : Triggers.Durable.State) : project (embed state) = state := by
  simp only [project, embed, decode_map_encode]
  rfl

private theorem remaining_rows (candidate : Request) (goals : List GoalBinding) :
    ((goals.map encode).filter fun row =>
        !(((goals.map encode).filter fun shown => sameSession candidate (decode shown)).map
          (·.ref)).contains row.ref) =
      (goals.filter fun goal => !sameSession candidate goal).map encode := by
  rw [List.filter_map]
  congr 1
  apply List.filter_congr
  intro goal member
  simp only [Function.comp]
  congr 1
  cases same : sameSession candidate goal with
  | true =>
    simp only [List.contains_iff_mem]
    apply List.mem_map_of_mem
    exact List.mem_filter.mpr ⟨List.mem_map_of_mem _ member, by simpa [decode_encode] using same⟩
  | false =>
    apply Bool.eq_false_iff.mpr
    intro present
    simp only [List.contains_iff_mem, List.mem_map, List.mem_filter] at present
    obtain ⟨row, ⟨⟨other, _, rfl⟩, matching⟩, sameRef⟩ := present
    have keys : key other = key goal := by
      simpa [encode] using sameRef
    simp only [key, Prod.mk.injEq] at keys
    simp only [decode_encode, sameSession] at matching
    simp [sameSession, ← keys.1, ← keys.2.1, matching] at same

private theorem binding_is_absent (candidate : Request) (goals : List GoalBinding) :
    ((goals.filter fun goal => !sameSession candidate goal).map encode).any
      (fun row => row.ref == (encode (binding candidate)).ref) = false := by
  apply Bool.eq_false_iff.mpr
  intro present
  simp only [List.any_eq_true, List.mem_map, List.mem_filter, beq_iff_eq] at present
  obtain ⟨row, ⟨goal, ⟨_, differs⟩, rfl⟩, sameRef⟩ := present
  have keys : key goal = key (binding candidate) := by simpa [encode] using sameRef
  simp only [key, binding, Prod.mk.injEq] at keys
  simp [sameSession, keys.1, keys.2.1] at differs

/-- The plugin's effects, applied by core, are the existing model's
`applyAssignment`: core's part of it on core state, the Goal binding
replacement on the plugin's rows. -/
theorem assignment_effects_are_applyAssignment (actor : Actor) (core : TaskCore)
    (goals : List GoalBinding) (candidate : Request) (distinct : (goals.map key).Nodup)
    (found : core.requests.find? (fun request =>
      request.fire.identity == candidate.fire.identity) = some candidate) :
    applyEffects perform actor (assignmentEffects candidate (goals.map encode))
        ⟨core, goals.map encode⟩ =
      some ⟨coreOf (applyAssignment (join core goals) candidate),
        (binding candidate :: goals.filter fun goal => !sameSession candidate goal).map
          encode⟩ := by
  have refs : (((goals.map encode).filter fun row => sameSession candidate (decode row)).map
      (·.ref)).Nodup := by
    have all : ((goals.map encode).map (·.ref)).Nodup := by
      have injective : Function.Injective (fun doc : GoalDoc => (⟨collection, doc⟩ : Ref GoalDoc)) := by
        intro left right same
        simpa using same
      have mapped := distinct.map injective
      simpa [List.map_map, encode, Function.comp] using mapped
    exact all.sublist ((List.filter_sublist _).map _)
  have deleted := deletes_remove_shown_rows perform actor
    ((goals.map encode).filter fun row => sameSession candidate (decode row))
    ⟨coreOf (recoverOutcomes (join core goals)), goals.map encode⟩ refs
    (fun row member => (List.mem_filter.mp member).1)
  rw [remaining_rows] at deleted
  simp only [assignmentEffects, applyEffects_append, decode_map_encode]
  have settled : applyEffects perform actor
      [(.core (.settleOutcomes goals) : Effect GoalDoc Goals.Status TaskOp)]
      ⟨core, goals.map encode⟩ =
      some ⟨coreOf (recoverOutcomes (join core goals)), goals.map encode⟩ := rfl
  simp only [settled, Option.some_bind, deleted]
  have absent := binding_is_absent candidate goals
  have located : (coreOf (recoverOutcomes (join core goals))).requests.find? (fun request =>
      request.fire.identity == candidate.fire.identity) = some candidate := found
  simp only [applyEffects, applyEffect, applyWrite, absent, Bool.false_eq_true, if_false,
    Option.map_some', perform, located]
  rfl

private theorem assignment_effects_are_valid (actor : Actor) (bound : Nat)
    (candidate : Request) (goals : List GoalBinding) (room : goals.length + 4 ≤ bound) :
    valid (manifest bound) host.allowed actor
      (assignmentEffects candidate (goals.map encode)) = true := by
  have shorter : ((goals.map encode).filter fun row =>
      sameSession candidate (decode row)).length ≤ goals.length := by
    have := List.length_filter_le (fun row => sameSession candidate (decode row))
      (goals.map encode)
    simpa using this
  simp only [valid, Bool.and_eq_true, decide_eq_true_eq, List.all_eq_true]
  refine ⟨?_, ?_⟩
  · simp only [assignmentEffects, List.length_append, List.length_map, List.length_cons,
      List.length_nil, manifest]
    omega
  · intro effect member
    simp only [assignmentEffects, List.mem_append, List.mem_cons, List.mem_map,
      List.mem_filter, List.not_mem_nil, or_false] at member
    rcases member with (rfl | ⟨row, ⟨⟨goal, _, rfl⟩, _⟩, rfl⟩) | rfl | rfl | rfl
    · rfl
    · simp [Effect.permitted, manifest, host, Write.ref, encode]
    · simp [Effect.permitted, manifest, host, Write.ref, encode]
    · rfl
    · rfl

/-- The slice is equal: core's claim composed with the Goals plugin through
the contract is `Triggers.Durable.claim`, for every state whose Goal documents
are distinct and every fire, given room for the effects under the bound. -/
theorem composed_claim_is_durable_claim (bound : Nat) (state : Triggers.Durable.State)
    (fire : Identity) (distinct : (state.goals.map key).Nodup)
    (room : state.goals.length + 4 ≤ bound) :
    composedClaim bound state fire = claim state fire := by
  unfold composedClaim claimStep
  simp only [gate, combine, List.foldl_nil]
  by_cases queued : canClaim state fire = true
  · have wins : (claimCore fire).claim (embed state).core =
        some { coreOf state with requests := state.requests.map fun request =>
          if request.fire.identity == fire then { request with running := true }
          else request } := by
      have same : canClaim (join (coreOf state) []) fire = true := queued
      simp only [claimCore, embed, same, if_true]
      rfl
    simp only [wins, claim, queued, if_true]
    cases located : (state.requests.map fun request =>
        if request.fire.identity == fire then { request with running := true }
        else request).find? (fun request => request.fire.identity == fire) with
    | none =>
      simp only [stage, stageFrom, Hook.run, plugin, host, applyTaskGoal, located, embed,
        List.map_cons, List.map_nil, valid, manifest, List.length_nil, Nat.zero_le,
        decide_true, List.all_nil, Bool.and_self, if_true, applyEffects, settle, project,
        decode_map_encode]
      rfl
    | some candidate =>
      have identity : candidate.fire.identity = fire := by
        have holds := List.find?_some located
        simpa using holds
      by_cases backed : candidate.fire.goalBacked = true
      · have accepted := assignment_effects_are_valid "runtime" bound candidate state.goals room
        have applied := assignment_effects_are_applyAssignment "runtime"
          { coreOf state with requests := state.requests.map fun request =>
            if request.fire.identity == fire then { request with running := true }
            else request } state.goals candidate distinct (by rw [identity]; exact located)
        simp only [stage, stageFrom, Hook.run, plugin, applyTaskGoal, located, embed,
          List.map_cons, List.map_nil, backed, if_true]
        have shown : host.select
            { coreOf state with requests := state.requests.map fun request =>
              if request.fire.identity == fire then { request with running := true }
              else request } (state.goals.map encode) () = state.goals.map encode := rfl
        simp only [shown, accepted, if_true]
        have performed : host.perform = perform := rfl
        rw [performed, applied]
        simp only [settle, project, decode_map_encode, backed, if_true]
        rfl
      · have plain : candidate.fire.goalBacked = false := by simpa using backed
        simp only [stage, stageFrom, Hook.run, plugin, host, applyTaskGoal, located, embed,
          List.map_cons, List.map_nil, plain, Bool.false_eq_true, if_false, valid, manifest,
          List.length_nil, Nat.zero_le, decide_true, List.all_nil, Bool.and_self, if_true,
          applyEffects, settle, project, decode_map_encode]
        rfl
  · have blocked : canClaim state fire = false := by simpa using queued
    have loses : (claimCore fire).claim (embed state).core = none := by
      have same : canClaim (join (coreOf state) []) fire = false := blocked
      simp only [claimCore, embed, same, Bool.false_eq_true, if_false]
    simp only [loses, settle, project_embed, claim, blocked, Bool.false_eq_true, if_false]

/-- What has to change in core for goal state to stop being a queue partition
key. A request whose signed input names its assignment has a scope that is a
function of the request alone, so it can be stamped at creation
(`Hooks.Stamp.partition`) and compared by the queue without reading a Goal. -/
def stampedScope (reference : Option GoalAutomation.RequestAssignment.Reference) (doc : Nat) :
    Option GoalAutomation.RequestAssignment.QueueScope :=
  reference.map fun named => .referenced named.goal (named.root.getD doc)

theorem tagged_scope_is_stampable
    (assignments : List GoalAutomation.RequestAssignment.Assignment)
    (entry : GoalAutomation.RequestAssignment.Entry)
    (named : GoalAutomation.RequestAssignment.Reference)
    (signed : entry.signatureValid = true) (tagged : entry.reference = some named) :
    GoalAutomation.RequestAssignment.queueScope assignments entry =
      stampedScope entry.reference entry.doc := by
  simp [GoalAutomation.RequestAssignment.queueScope, stampedScope, signed, tagged]

/-- Finding: an untagged request's scope is resolved from the Goals retained
at queue time (`RequestAssignment.observe`), so it is not a function of the
request and no stamp taken at creation reproduces it. For the stamp to
suffice, every request must carry its assignment reference when created (the
Goals plugin stamps it at `request.create.stage`), and the history-resolved
scope for older untagged rows has to be retired or backfilled once. -/
theorem untagged_scope_reads_goal_state :
    GoalAutomation.RequestAssignment.queueScope [] ⟨100, 1, 10, none, true⟩ ≠
      GoalAutomation.RequestAssignment.queueScope [⟨1, 10, 7, some 100⟩]
        ⟨100, 1, 10, none, true⟩ := by
  decide

end Hooks.GoalTask
