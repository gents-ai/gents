import Proofs.Session.Hooks.Resume

namespace Hooks.GoalResume

open Hooks.Resume

inductive Status where
  | active
  | paused
  | complete
  deriving DecidableEq, Repr

/-- A Goal row as the Goals plugin is shown it: the Goal, its status and the
document of its assignment root, if it has one. -/
structure Goal where
  goal : Nat
  status : Status
  root : Option Nat
  deriving DecidableEq, Repr

/-- Rows the plugin is shown: its own Goal row, and core's request rows of the
session rendered read-only. -/
inductive Fact where
  | goal (goal : Goal)
  | request (request : Request)
  deriving DecidableEq, Repr

inductive Query where
  | goal
  | requests
  deriving DecidableEq, Repr

def collection : String := "Goal"

def manifest : Manifest := { name := "goals", owns := [collection], bound := 2 }

def render (request : Request) : Row Nat Fact := ⟨⟨"AgentRequest", request.id⟩, 0, .request request⟩

def select (core : Session) (docs : Store Nat Fact) : Query → List (Row Nat Fact)
  | .goal => docs
  | .requests => core.requests.map render

/-- Core's side for the instance: any query answered from the transaction,
every write permitted, and `request.create` performed under core's rule. -/
def host (mayCreate : Actor → CreateArgs → Session → Bool) :
    Host Session Query Nat Fact CreateArgs :=
  { select := select, perform := perform mayCreate, allowed := fun _ _ _ => true }

/-- What core tells the plugin at `control.session.resume`: the document the
continuation it may ask for will have, and the agent it runs as. -/
structure Resumed where
  next : Nat
  agent : Actor
  deriving DecidableEq, Repr

def requestsOf (rows : List (Row Nat Fact)) : List Request :=
  rows.filterMap fun row =>
    match row.value with
    | .request request => some request
    | .goal _ => none

/-- The continuation an idle resume asks core to create: it continues the
Goal and names the Goal's assignment as it stands (`continuation_assignment`). -/
def continuation (event : Resumed) (goal : Goal) : CreateArgs :=
  { id := event.next, agent := event.agent, trigger := some goal.goal,
    reference := some (goal.goal, goal.root) }

/-- `goal::lifecycle_hooks::RestageOnResume`: a paused Goal resumes with its
session. Busy, it only becomes active; idle, it becomes active and asks core
to create its continuation, which names the Goal and the Goal's assignment as
it stands (`continuation_assignment`). A continuation receipt that already
exists is replayed by `request_head::verify_goal_continuation_receipt`, which
this instance does not state. -/
def restage (event : Resumed) : List (List (Row Nat Fact)) → Outcome Unit Nat Fact CreateArgs
  | [[⟨ref, revision, .goal goal⟩], requests] =>
      if goal.status ≠ .paused then .proceed () []
      else
        let shown := requestsOf requests
        let active : Fact := .goal { goal with status := .active }
        if shown.any (fun request => !request.terminal) then
          .proceed () [.write (.update ref revision active)]
        else if shown.isEmpty then .refuse "goal has no authenticated predecessor to resume"
        else
          .proceed () [.write (.update ref revision active),
            .core (continuation event goal)]
  | [[], _] => .proceed () []
  | _ => .refuse "Goal resume was not shown its declared rows"

/-- `goal::lifecycle_hooks::BindResumedRoot` at `request.create.stage`, after
creation: the continuation core created for a Goal with no assignment root
becomes that root (decision 2026-11-11, maintainer-approved; the binding
`bind_goal_first_request_in_txn` makes for a Goal's first request). A request
that continues no Goal declares no reads. -/
def bind (created : Option Request) : List (List (Row Nat Fact)) →
    Outcome Unit Nat Fact CreateArgs
  | [] => .proceed () []
  | [[⟨ref, revision, .goal goal⟩]] =>
      match created with
      | some request =>
          if request.trigger = some goal.goal ∧ goal.root = none then
            if request.reference.all (fun named => named.1 == goal.goal && named.2.isNone) then
              .proceed () [.write (.update ref revision (.goal { goal with root := some request.id }))]
            else .refuse "Goal first request conflicts with its assignment binding"
          else .proceed () []
      | none => .proceed () []
  | [[]] => .proceed () []
  | _ => .refuse "Goal root binding was not shown its declared rows"

def bindReads (created : Option Request) : List Query :=
  match created with
  | some request => if request.trigger.isSome then [.goal] else []
  | none => []

def restagePlugin : Plugin Query Resumed Unit Nat Fact CreateArgs :=
  { manifest := manifest, hook := { reads := fun _ => [.goal, .requests], decide := restage } }

def bindPlugin : Plugin Query (Option Request) Unit Nat Fact CreateArgs :=
  { manifest := manifest, hook := { reads := bindReads, decide := bind } }

/-- The request a creation added, as core shows it to `request.create.stage`. -/
def created (before after : Session) : Option Request :=
  if before.requests.length < after.requests.length then after.requests.head? else none

/-- The resume of one session with the Goals plugin at both points. -/
def resumeGoal (mayCreate : Actor → CreateArgs → Session → Bool) (actor : Actor)
    (event : Resumed) (state : State Session Nat Fact) :
    Except Refusal (State Session Nat Fact × List Unit) :=
  resumeCreating (host mayCreate) [restagePlugin] [bindPlugin] created (fun _ => true) actor
    event state

/-- The root a request observes: its reference's root, or itself when its
reference names none (`GoalFence.observedRoot`). -/
def observedRoot (request : Request) : Nat :=
  ((request.reference.bind (·.2))).getD request.id

/-- Membership of a Goal's current assignment (`RequestAssignment.belongs`
for the fields this instance keeps): the request continues the Goal, names
it, and observes the Goal's root. -/
def member (goal : Goal) (request : Request) : Bool :=
  request.trigger == some goal.goal && request.reference.any (·.1 == goal.goal) &&
    goal.root == some (observedRoot request)

def goalRow (ref : Ref Nat) (revision : Nat) (goal : Goal) : Row Nat Fact :=
  ⟨ref, revision, .goal goal⟩

theorem requestsOf_render (requests : List Request) :
    requestsOf (requests.map render) = requests := by
  induction requests with
  | nil => rfl
  | cons request rest ih => simp [requestsOf, render] at ih ⊢; exact ih

private theorem settled_requests (requests : List Request)
    (idle : ∀ request ∈ requests, request.terminal = true) :
    (requests.any fun request => !request.terminal) = false := by
  simp only [List.any_eq_false, Bool.not_eq_true']
  intro request member
  simp [idle request member]

/-- A paused, idle Goal with no assignment root resumes: it is active, its
root is the created continuation's document, and that continuation, signed by
the transaction's actor, is a member of the assignment. -/
theorem rootless_idle_resume_binds_the_continuation (actor : Actor) (event : Resumed)
    (goal : Goal) (ref : Ref Nat) (revision : Nat) (requests : List Request)
    (paused : goal.status = .paused) (rootless : goal.root = none)
    (owned : ref.collection = collection)
    (idle : ∀ request ∈ requests, request.terminal = true) (predecessor : requests ≠ [])
    (fresh : ∀ request ∈ requests, request.id ≠ event.next) :
    resumeGoal (fun _ _ _ => true) actor event ⟨⟨true, requests⟩, [goalRow ref revision goal]⟩ =
      .ok (⟨⟨false, (continuation event goal).request actor :: requests⟩,
        [goalRow ref (revision + 2) { goal with status := .active, root := some event.next }]⟩,
        [()]) ∧
      member { goal with status := .active, root := some event.next }
        ((continuation event goal).request actor) = true := by
  have busy := settled_requests requests idle
  have nonempty : requests.isEmpty = false := by
    cases requests with
    | nil => exact absurd rfl predecessor
    | cons _ _ => rfl
  have absent : (requests.any fun request => request.id == event.next) = false := by
    simp only [List.any_eq_false, beq_iff_eq]
    exact fresh
  refine ⟨?_, ?_⟩
  · simp [resumeGoal, resumeCreating, resume, stage, stageFrom, Hook.run, restagePlugin,
      bindPlugin, host, select, restage, bind, bindReads, requestsOf_render, paused, rootless,
      busy, nonempty, valid, manifest, Effect.permitted, distinctWrites, Effect.written,
      Write.ref, Write.access, owned, applyEffects, applyEffect, applyWrite, held, goalRow,
      perform, absent, created, continuation, CreateArgs.request]
  · simp [member, observedRoot, continuation, CreateArgs.request, rootless]

/-- A bound, paused, idle Goal resumes as before: active, its root unchanged,
and the continuation it asks for a member of that assignment; the binding
listener writes nothing. -/
theorem bound_idle_resume_keeps_its_root (actor : Actor) (event : Resumed)
    (goal : Goal) (ref : Ref Nat) (revision : Nat) (requests : List Request) (root : Nat)
    (paused : goal.status = .paused) (bound : goal.root = some root)
    (owned : ref.collection = collection)
    (idle : ∀ request ∈ requests, request.terminal = true) (predecessor : requests ≠ [])
    (fresh : ∀ request ∈ requests, request.id ≠ event.next) :
    resumeGoal (fun _ _ _ => true) actor event ⟨⟨true, requests⟩, [goalRow ref revision goal]⟩ =
      .ok (⟨⟨false, (continuation event goal).request actor :: requests⟩,
        [goalRow ref (revision + 1) { goal with status := .active }]⟩, [()]) ∧
      member { goal with status := .active } ((continuation event goal).request actor) =
        true := by
  have busy := settled_requests requests idle
  have nonempty : requests.isEmpty = false := by
    cases requests with
    | nil => exact absurd rfl predecessor
    | cons _ _ => rfl
  have absent : (requests.any fun request => request.id == event.next) = false := by
    simp only [List.any_eq_false, beq_iff_eq]
    exact fresh
  refine ⟨?_, ?_⟩
  · simp [resumeGoal, resumeCreating, resume, stage, stageFrom, Hook.run, restagePlugin,
      bindPlugin, host, select, restage, bind, bindReads, requestsOf_render, paused, bound,
      busy, nonempty, valid, manifest, Effect.permitted, distinctWrites, Effect.written,
      Write.ref, Write.access, owned, applyEffects, applyEffect, applyWrite, held, goalRow,
      perform, absent, created, continuation, CreateArgs.request]
  · simp [member, observedRoot, continuation, CreateArgs.request, bound]

/-- A paused Goal whose session is busy resumes the work the session holds:
it becomes active, keeps its root, and no request is created. -/
theorem busy_resume_creates_nothing (actor : Actor) (event : Resumed) (goal : Goal)
    (ref : Ref Nat) (revision : Nat) (requests : List Request)
    (paused : goal.status = .paused) (owned : ref.collection = collection)
    (working : (requests.any fun request => !request.terminal) = true) :
    resumeGoal (fun _ _ _ => true) actor event ⟨⟨true, requests⟩, [goalRow ref revision goal]⟩ =
      .ok (⟨⟨false, requests⟩, [goalRow ref (revision + 1) { goal with status := .active }]⟩,
        [()]) := by
  simp [resumeGoal, resumeCreating, resume, stage, stageFrom, Hook.run, restagePlugin,
    bindPlugin, host, select, restage, bind, bindReads, requestsOf_render, paused, working,
    valid, manifest, Effect.permitted, distinctWrites, Effect.written, Write.ref,
    Write.access, owned, applyEffects, applyEffect, applyWrite, held, goalRow, created]

end Hooks.GoalResume
