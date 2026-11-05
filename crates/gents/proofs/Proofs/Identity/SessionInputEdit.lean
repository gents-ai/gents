import Proofs.Identity.Household
import Proofs.Request.CrossActorAuthorization

namespace Identity.SessionInputEdit

/-- The signed command names physical targets. Creation and both native
relationships have one commit; a missing response does not justify replay or
restoring a relationship that the owner may have revoked after publication. -/
structure Publication where
  command : Bool := false
  parent : Bool := false
  workingAgent : Bool := false
  deriving BEq, DecidableEq, Repr

def publish (committed : Bool) : Publication :=
  if committed then ⟨true, true, true⟩ else ⟨false, false, false⟩

def revokeWorkingAgent (publication : Publication) : Publication :=
  { publication with workingAgent := false }

def observeExisting (publication : Publication) : Publication := publication

theorem abort_leaves_no_partial_command : publish false = ⟨false, false, false⟩ := by rfl

theorem duplicate_does_not_restore_revoked_working_agent :
    observeExisting (revokeWorkingAgent (publish true)) = ⟨true, true, false⟩ := by rfl

/-- This fixture uses the existing goal-controls policy. The command creator
owns it, its physical session is the parent, and only the selected working
Agent receives the direct relationship. -/
def fixture : Household.State :=
  { objects := [⟨"agents", "agent-doc", "user"⟩,
      ⟨"sessions", "session-doc", "user"⟩,
      ⟨"goal_controls", "edit-doc", "user"⟩],
    relations := [⟨"goal_controls", "edit-doc", "parent", .object "sessions" "session-doc"⟩,
      ⟨"goal_controls", "edit-doc", "working_agent", .actor "agent"⟩], enrolled := [] }

theorem command_is_visible_to_its_owner_and_working_agent :
    Household.allowed 5 fixture "user" "goal_controls" "edit-doc" "read" = true ∧
    Household.allowed 5 fixture "agent" "goal_controls" "edit-doc" "read" = true ∧
    Household.allowed 5 fixture "other" "goal_controls" "edit-doc" "read" = false := by native_decide

theorem working_agent_can_write_receipt_but_other_cannot :
    Household.allowed 5 fixture "agent" "goal_controls" "edit-doc" "update" = true ∧
    Household.allowed 5 fixture "other" "goal_controls" "edit-doc" "update" = false := by native_decide

/-- Discovery is scoped to held active Agent identities. A notification only
requests another current-state sweep; it does not supply an executing actor.
The signed selected Agent is checked again when the command is processed. -/
def discover (state : Household.State) (active : List String) (document : String) : List String :=
  active.filter fun actor => Household.allowed 5 state actor "goal_controls" document "read"

def selectedForEffect (discovered : List String) (selected : String) : List String :=
  discovered.filter (· == selected)

theorem discovery_requires_the_selected_held_agent :
    selectedForEffect (discover fixture ["runtime", "other"] "edit-doc") "agent" = [] ∧
    selectedForEffect (discover fixture ["runtime", "agent", "other"] "edit-doc") "agent" = ["agent"] := by native_decide

def shouldSweep (notification resync tick : Bool) : Bool := notification || resync || tick

theorem tick_recovers_a_missed_notification : shouldSweep false false true = true := by rfl

inductive Outcome where
  | pending | applied | rejected
  deriving BEq, DecidableEq, Repr

/-- The immediate authenticated author's session and Agent permissions are
observed in the same native transaction as the queue effect and receipt.
The existing cross-actor contract supplies those checks, including transaction
conflicts. A completed receipt is an observation, not a new claim, and does
not recheck or reapply after a later revocation. -/
def apply (prior : Outcome) (intent : Request.CrossActorAuthorization.Intent)
    (observation : Request.CrossActorAuthorization.Observation)
    (commit : Request.CrossActorAuthorization.NativeCommit) : Outcome :=
  if prior != .pending then prior else
  match Request.CrossActorAuthorization.decideClaim intent observation commit with
  | .claimed => .applied
  | .denied => .rejected
  | _ => .pending

theorem committed_edit_not_rechecked
    (intent : Request.CrossActorAuthorization.Intent)
    (observation : Request.CrossActorAuthorization.Observation)
    (commit : Request.CrossActorAuthorization.NativeCommit) :
    apply .applied intent observation commit = .applied := by rfl

theorem conflict_leaves_edit_pending :
    apply .pending Request.CrossActorAuthorization.exampleIntent
      Request.CrossActorAuthorization.permittedObservation ⟨false, true, true, true⟩ = .pending := by decide

theorem native_denial_rejects_without_effect :
    apply .pending Request.CrossActorAuthorization.exampleIntent
      Request.CrossActorAuthorization.deniedObservation ⟨false, true, true, true⟩ = .rejected := by decide

theorem matching_native_author_and_targets_can_apply :
    apply .pending Request.CrossActorAuthorization.exampleIntent
      Request.CrossActorAuthorization.permittedObservation ⟨true, true, true, true⟩ = .applied := by decide

end Identity.SessionInputEdit
