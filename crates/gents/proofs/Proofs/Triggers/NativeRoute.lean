import Lean
import Proofs.Identity.Household

namespace Triggers.NativeRoute

/-- The handoff route an outcome-enabled fire copies into its FireOutcome:
`handoff_id`, `reply_session_id` and `attempt`. Authored collections declare
these fields themselves. Native workflow documents (CallbackResult,
IsolatedWorkspace, WorkspaceReceipt) carry the same three fields, stamped by
their producer when the document is written, because their consumer is often
another Agent that cannot read the producer's CallbackInvocation (an Agent
child). The route is opaque routing data; it grants no authority to the
session it names. -/
structure Route where
  handoffId : String
  replySessionId : Option String := none
  attempt : Option Int := none
  deriving DecidableEq, Repr

/-- The route fields as stored on a document or a frozen invocation input. -/
structure Fields where
  handoffId : Option String := none
  replySessionId : Option String := none
  attempt : Option Int := none
  deriving DecidableEq, Repr

def nonBlank : Option String → Option String
  | some value => if value.trim.isEmpty then none else some value
  | none => none

/-- At most one route: a blank handoff projects none; a blank reply session is
absent. -/
def projectRoute (fields : Fields) : Option Route :=
  (nonBlank fields.handoffId).map fun handoff =>
    { handoffId := handoff, replySessionId := nonBlank fields.replySessionId,
      attempt := fields.attempt }

/-- The producer writes the route it holds into the document it creates: the
creating callback stamps the workspace and callback result from its own frozen
invocation input; seal and integration stamp each receipt from its workspace. -/
def stamp : Option Route → Fields
  | some route => ⟨some route.handoffId, route.replySessionId, route.attempt⟩
  | none => {}

/-- Delivery derives the fire route from the observed document alone. -/
def deliveredRoute (observed : Fields) : Option Route := projectRoute observed

/-- An outcome-enabled fire is admitted only with a non-blank handoff. -/
def outcomeFireAdmitted (emitOutcome : Bool) (observed : Fields) : Bool :=
  !emitOutcome || (deliveredRoute observed).isSome

theorem stamped_route_is_delivered (input : Fields) :
    deliveredRoute (stamp (projectRoute input)) = projectRoute input := by
  rcases input with ⟨handoff, reply, attempt⟩
  cases handoff with
  | none => rfl
  | some value =>
    by_cases blank : value.trim.isEmpty
    · simp [projectRoute, nonBlank, blank, stamp, deliveredRoute]
    · cases reply with
      | none => simp [projectRoute, nonBlank, blank, stamp, deliveredRoute]
      | some reply =>
        by_cases replyBlank : reply.trim.isEmpty
        · simp [projectRoute, nonBlank, blank, replyBlank, stamp, deliveredRoute]
        · simp [projectRoute, nonBlank, blank, replyBlank, stamp, deliveredRoute]

/-- Copying the workspace's stamped route onto each of its receipts preserves
the invocation's route through any number of copies. -/
theorem copied_route_is_preserved (input : Fields) :
    deliveredRoute (stamp (deliveredRoute (stamp (projectRoute input)))) = projectRoute input := by
  rw [stamped_route_is_delivered, stamped_route_is_delivered]

theorem unstamped_document_refuses_outcome_fire :
    outcomeFireAdmitted true (stamp none) = false := rfl

theorem blank_handoff_refuses_outcome_fire (reply : Option String) (attempt : Option Int) :
    outcomeFireAdmitted true ⟨some "", reply, attempt⟩ = false := by
  have blank : "".trim.isEmpty = true := by native_decide
  simp [outcomeFireAdmitted, deliveredRoute, projectRoute, nonBlank, blank]

/-- A physical document as the native policy names it. -/
structure Document where
  resource : String
  id : String
  deriving DecidableEq, Repr

/-- The only documents delivery reads: the observed document. -/
def deliveryReads (observed : Document) : List Document := [observed]

/-- A consumer delivers an observed document when it can read that document
under its own native authority and the document admits the fire. -/
def deliver (state : Identity.Household.State) (consumer : String) (observed : Document)
    (fields : Fields) (emitOutcome : Bool) : Option (Option Route) :=
  if (deliveryReads observed).all (fun document =>
      Identity.Household.allowed 8 state consumer document.resource document.id "read") &&
      outcomeFireAdmitted emitOutcome fields then
    some (deliveredRoute fields)
  else none

/-- Delivery needs no cross-Agent read: no producer document other than the
observed one is read, so a producer's CallbackInvocation is never consulted. -/
theorem delivery_needs_no_cross_agent_read (observed producer : Document)
    (different : producer ≠ observed) : producer ∉ deliveryReads observed := by
  simp [deliveryReads, different]

/-- Delivery is decided by the consumer's read of the observed document and
the route fields that document carries; nothing else in the state matters. -/
theorem delivery_depends_only_on_observed_read (state : Identity.Household.State)
    (consumer : String) (observed : Document) (fields : Fields) (emitOutcome : Bool) :
    deliver state consumer observed fields emitOutcome =
      if Identity.Household.allowed 8 state consumer observed.resource observed.id "read" &&
          outcomeFireAdmitted emitOutcome fields then some (deliveredRoute fields) else none := by
  simp [deliver, deliveryReads]

/-- The change-unit shape: the writer Agent owns its invocation (an Agent
child) and its workspace; the reviewer is a workspace reader, so it reads the
writer receipt through the workspace parent but not the invocation. -/
def reviewFixture : Identity.Household.State :=
  { objects := [⟨"nodes", "node", "user"⟩, ⟨"agents", "writer-doc", "user"⟩,
      ⟨"agents", "reviewer-doc", "user"⟩,
      ⟨"agent_children", "invocation", "writer"⟩,
      ⟨"workspaces", "workspace", "writer"⟩,
      ⟨"workspace_children", "receipt", "writer"⟩],
    relations := [⟨"agents", "writer-doc", "parent", .object "nodes" "node"⟩,
      ⟨"agents", "writer-doc", "executor", .actor "writer"⟩,
      ⟨"agents", "reviewer-doc", "parent", .object "nodes" "node"⟩,
      ⟨"agents", "reviewer-doc", "executor", .actor "reviewer"⟩,
      ⟨"agent_children", "invocation", "parent", .object "agents" "writer-doc"⟩,
      ⟨"workspaces", "workspace", "parent", .object "agents" "writer-doc"⟩,
      ⟨"workspaces", "workspace", "reader", .actor "reviewer"⟩,
      ⟨"workspace_children", "receipt", "parent", .object "workspaces" "workspace"⟩],
    enrolled := [] }

def receipt : Document := ⟨"workspace_children", "receipt"⟩
def invocation : Document := ⟨"agent_children", "invocation"⟩

def changeUnitInput : Fields := ⟨some "change-unit:a", some "lead", some 1⟩

theorem reviewer_delivers_stamped_receipt_without_reading_invocation :
    Identity.Household.allowed 8 reviewFixture "reviewer" invocation.resource invocation.id "read" = false ∧
    Identity.Household.allowed 8 reviewFixture "reviewer" receipt.resource receipt.id "read" = true ∧
    deliver reviewFixture "reviewer" receipt (stamp (projectRoute changeUnitInput)) true =
      some (projectRoute changeUnitInput) ∧
    deliver reviewFixture "other" receipt (stamp (projectRoute changeUnitInput)) true = none := by
  native_decide

structure Case where
  name : String
  input : Fields
  emitOutcome : Bool
  deriving Repr

def cases : List Case :=
  [ ⟨"full-route", changeUnitInput, true⟩
  , ⟨"handoff-only", ⟨some "change-unit:b", none, none⟩, true⟩
  , ⟨"blank-reply-is-absent", ⟨some "change-unit:c", some "  ", some 2⟩, true⟩
  , ⟨"no-route-refuses-outcome", {}, true⟩
  , ⟨"blank-handoff-refuses-outcome", ⟨some " ", some "lead", some 1⟩, true⟩
  , ⟨"no-route-ordinary-fire", {}, false⟩ ]

def optionJson {α : Type} [Lean.ToJson α] : Option α → Lean.Json
  | some value => Lean.toJson value
  | none => Lean.Json.null

def fieldsJson (fields : Fields) : Lean.Json :=
  Lean.Json.mkObj [("handoff_id", optionJson fields.handoffId),
    ("reply_session_id", optionJson fields.replySessionId),
    ("attempt", optionJson fields.attempt)]

/-- Each case serializes the producer's invocation input, the fields it stamps
on the native document, and the route delivery derives from that document. -/
def casesJson : String :=
  (Lean.toJson (cases.map fun c =>
    let stamped := stamp (projectRoute c.input)
    Lean.Json.mkObj [("name", Lean.toJson c.name), ("input", fieldsJson c.input),
      ("emit_outcome", Lean.toJson c.emitOutcome),
      ("stamped", fieldsJson stamped),
      ("admitted", Lean.toJson (outcomeFireAdmitted c.emitOutcome stamped)),
      ("delivered", fieldsJson (stamp (deliveredRoute stamped)))])).compress

end Triggers.NativeRoute
