import Proofs.Session.Hooks.Claim
import Proofs.Session.Hooks.History
import Proofs.Automation.Hooks.GoalTask

namespace Hooks

variable {Core Q Ev V Doc Val Op : Type}

/-- A returned-value point: every plugin answers on the transaction's
snapshot with a value core consumes and no effects; a result that carries any
is invalid, and a refusal is reported with the plugin's name. `Hooks.gate` is
this runner at `request.claim.gate` (`gate_is_answer`). -/
def answer (host : Host Core Q Doc Val Op) (event : Ev) (snapshot : State Core Doc Val) :
    List (Plugin Q Ev V Doc Val Op) → Except Refusal (List V)
  | [] => .ok []
  | plugin :: rest =>
      match plugin.hook.run host event snapshot with
      | .refuse reason => .error (.refused plugin.manifest.name reason)
      | .proceed value effects =>
          if effects.isEmpty then
            match answer host event snapshot rest with
            | .error refusal => .error refusal
            | .ok values => .ok (value :: values)
          else .error (.invalid plugin.manifest.name)

theorem gate_is_answer (host : Host Core Q Doc Val Op) (event : Ev)
    (snapshot : State Core Doc Val) (plugins : List (Plugin Q Ev ClaimVerdict Doc Val Op)) :
    gate host event snapshot plugins = answer host event snapshot plugins := by
  induction plugins with
  | nil => rfl
  | cons plugin rest ih =>
    simp only [gate, answer, ih]
    cases plugin.hook.run host event snapshot with
    | refuse reason => rfl
    | proceed value effects =>
      cases answer host event snapshot rest <;> rfl

namespace QueueScope

/-- What one plugin answers at `request.queue.scope` for one request core
asked about: no answer, an opaque partition key, or a failure to determine
one. A failure belongs to that request alone: core reports it only when it
consults that request (`partition_reads_only_its_position`). -/
inductive Answer (Key : Type) where
  | unanswered
  | keyed (key : Key)
  | failed (reason : String)
  deriving DecidableEq, Repr

/-- One plugin's answers, aligned with the requests core asked about; a
missing position is unanswered. -/
abbrev Answers (Key : Type) := List (Answer Key)

variable {Key Req : Type}

/-- Core's combination rule for the request at `index`: the keys plugins
answered, named by plugin, in the fixed plugin order, as `Hooks.Stamped.partition`
names stamped keys. Two requests share a queue partition exactly when these
lists are equal. No plugin answering is the empty list: the unscoped default.
The first failure in plugin order is the request's refusal. -/
def partition : List (String × Answers Key) → Nat → Except Refusal (List (String × Key))
  | [], _ => .ok []
  | (name, answers) :: rest, index =>
      match answers[index]?.getD .unanswered with
      | .failed reason => .error (.refused name reason)
      | .unanswered => partition rest index
      | .keyed key => (partition rest index).map fun tail => (name, key) :: tail

/-- `request.queue.scope`: core asks the bundled plugins for the queue
partition of requests it is about to compare (wake coalescing, the
background-completion claim snapshot, wake enqueue).

Decision 2026-11-11, pending maintainer review: the queue partition of a
request is a returned value, of the same shape as the creation stamp
(`Hooks.Stamp.partition`), answered from the plugin's declared reads at
claim and queue time. Core treats no plugin answering as the unscoped
default and compares keys without interpreting them. The creation-time stamp
for new rows stays as `Hooks.GoalTask.stampedScope` states it; rows created
untagged are answered from the rows the plugin reads at claim and queue time,
as today (`Hooks.GoalTask.untagged_scope_reads_goal_state` is why a stamp
alone does not cover them). A refusal or failed read keeps the claim or queue
transaction from committing, which is what a failed Goal read does today. -/
def scopes (host : Host Core Q Doc Val Op)
    (plugins : List (Plugin Q (List Req) (Answers Key) Doc Val Op)) (asked : List Req)
    (snapshot : State Core Doc Val) :
    Except Refusal (List (Except Refusal (List (String × Key)))) :=
  (answer host asked snapshot plugins).map fun answers =>
    (List.range asked.length).map (partition ((plugins.map (·.manifest.name)).zip answers))

/-- With no plugin answering for a request, core's scope for it is the
unscoped default. -/
theorem unanswered_is_unscoped (named : List (String × Answers Key)) (index : Nat)
    (silent : ∀ entry ∈ named, entry.2[index]?.getD .unanswered = .unanswered) :
    partition named index = .ok [] := by
  induction named with
  | nil => rfl
  | cons head rest ih =>
    obtain ⟨name, answers⟩ := head
    have quiet := silent (name, answers) (List.mem_cons_self _ _)
    simp only at quiet
    simp only [partition, quiet]
    exact ih fun entry member => silent entry (List.mem_cons_of_mem _ member)

theorem no_plugin_is_unscoped (host : Host Core Q Doc Val Op) (asked : List Req)
    (snapshot : State Core Doc Val) :
    scopes host ([] : List (Plugin Q (List Req) (Answers Key) Doc Val Op)) asked snapshot =
      .ok ((List.range asked.length).map fun _ => .ok []) := by
  simp [scopes, answer, partition, Except.map]

/-- A request's scope is a function of what each plugin answered for that
request alone: another request's key or failure cannot change it. -/
theorem partition_reads_only_its_position (named other : List (String × Answers Key))
    (index : Nat)
    (same : named.map (fun entry => (entry.1, entry.2[index]?.getD .unanswered)) =
      other.map (fun entry => (entry.1, entry.2[index]?.getD .unanswered))) :
    partition named index = partition other index := by
  induction named generalizing other with
  | nil =>
    cases other with
    | nil => rfl
    | cons _ _ => simp at same
  | cons head rest ih =>
    cases other with
    | nil => simp at same
    | cons head' rest' =>
      obtain ⟨name, answers⟩ := head
      obtain ⟨name', answers'⟩ := head'
      simp only [List.map_cons, List.cons.injEq, Prod.mk.injEq] at same
      obtain ⟨⟨rfl, entry⟩, tail⟩ := same
      simp only [partition, entry, ih rest' tail]

open GoalAutomation.RequestAssignment

/-- The Goals plugin's declared read at this point is the session's retained
assignments: in the native listener, the session's request rows core passes
in the event and the native revision history of the session's Goals, whose
frontier is `GoalAutomation.RequestAssignment.latestVersions`. -/
def assignmentRow (assignment : Assignment) : Row Assignment Unit :=
  ⟨⟨"Goal", assignment⟩, 0, ()⟩

def unauthenticated : String := "request receipt signature is invalid"

def goalsAnswer (assignments : List Assignment) (entry : Entry) : Answer QueueScope :=
  match queueScope assignments entry with
  | some scope => .keyed scope
  | none => .failed unauthenticated

def goals : Plugin Unit (List Entry) (Answers QueueScope) Assignment Unit Op :=
  { manifest := ⟨"goals", ["Goal"], 0⟩
  , hook :=
      { reads := fun _ => [()]
      , decide := fun asked rows =>
          .proceed (asked.map (goalsAnswer (rows.flatten.map (·.ref.doc)))) [] } }

theorem assignment_rows_decode (assignments : List Assignment) :
    (assignments.map assignmentRow).map (·.ref.doc) = assignments := by
  induction assignments with
  | nil => rfl
  | cons head rest ih => simp only [List.map_cons, ih]; rfl

/-- The scope core sees for one request when Goals is the only plugin. -/
def goalScope (assignments : List Assignment) (entry : Entry) :
    Except Refusal (List (String × QueueScope)) :=
  match queueScope assignments entry with
  | some scope => .ok [("goals", scope)]
  | none => .error (.refused "goals" unauthenticated)

/-- The Goals instantiation answers exactly `RequestAssignment.queueScope`
for every request core asks about, tagged and untagged, so the coalescing and
claim guarantees stated over `queueScope` are kept. -/
theorem goals_partition_is_queueScope (host : Host Core Unit Assignment Unit Op)
    (snapshot : State Core Assignment Unit) (assignments : List Assignment)
    (shown : host.select snapshot.core snapshot.docs () = assignments.map assignmentRow)
    (asked : List Entry) (index : Nat) (entry : Entry) (located : asked[index]? = some entry) :
    ∃ results, scopes host [goals] asked snapshot = .ok results ∧
      results[index]? = some (goalScope assignments entry) := by
  have decoded := assignment_rows_decode assignments
  have bounded : index < asked.length := by
    rcases List.getElem?_eq_some_iff.mp located with ⟨within, _⟩
    exact within
  refine ⟨(List.range asked.length).map
    (partition [("goals", asked.map (goalsAnswer assignments))]), ?_, ?_⟩
  · simp only [scopes, answer, Hook.run, goals, List.map_cons, List.map_nil, shown,
      List.flatten_cons, List.flatten_nil, List.append_nil, decoded, List.isEmpty_nil, if_true]
    rfl
  · simp only [List.getElem?_map, List.getElem?_range bounded, Option.map_some']
    congr 1
    simp only [partition, List.getElem?_map, located, Option.map_some', Option.getD_some,
      goalsAnswer, goalScope]
    cases queueScope assignments entry <;> rfl

theorem goals_partitions_agree_iff_queue_scopes_agree (assignments : List Assignment)
    (first second : Entry) (firstSigned : first.signatureValid = true)
    (secondSigned : second.signatureValid = true) :
    goalScope assignments first = goalScope assignments second ↔
      queueScope assignments first = queueScope assignments second := by
  have some_of : ∀ entry : Entry, entry.signatureValid = true →
      ∃ scope, queueScope assignments entry = some scope := by
    intro entry signed
    simp [queueScope, signed]
  obtain ⟨left, hl⟩ := some_of first firstSigned
  obtain ⟨right, hr⟩ := some_of second secondSigned
  simp [goalScope, hl, hr]

/-- A request created with its assignment reference gets, at this point, the
key its creation stamp names: the returned value and the stamp agree on every
tagged row. -/
theorem tagged_partition_is_creation_stamp (assignments : List Assignment) (entry : Entry)
    (named : Reference) (signed : entry.signatureValid = true)
    (tagged : entry.reference = some named) :
    ∃ scope, GoalTask.stampedScope entry.reference entry.doc = some scope ∧
      goalScope assignments entry = .ok [("goals", scope)] := by
  refine ⟨.referenced named.goal (named.root.getD entry.doc), ?_, ?_⟩
  · simp [GoalTask.stampedScope, tagged]
  · simp [goalScope, GoalTask.tagged_scope_is_stampable assignments entry named signed tagged,
      GoalTask.stampedScope, tagged]

/-- The Goals listener's declared read here is the revision history of the
session's Goal rows (`goal::history::assignment_history_read`), which Goals
owns and declares history for in its manifest. -/
def goalsHistory : Unit → Option (HistoryRead Assignment) := fun _ => some ⟨"Goal", none⟩

def goalsHistoryDeclared : String → List String := fun _ => ["Goal"]

/-- Goals' history read is within the bound, so the bound leaves its answers
(and `goals_partition_is_queueScope`) unchanged. -/
theorem goals_history_read_is_bounded {α : Type} (named : List (Ref Assignment))
    (asked : List Entry) (run : Except Refusal α) :
    bounded goalsHistory goalsHistoryDeclared named asked
      [(goals : Plugin Unit (List Entry) (Answers QueueScope) Assignment Unit Op)] run = run := by
  apply bounded_reads_run_unchanged
  intro plugin member
  simp only [List.mem_singleton] at member
  subst member
  simp [readsBounded, goals, goalsHistory, goalsHistoryDeclared, historyPermitted]

end QueueScope

end Hooks
