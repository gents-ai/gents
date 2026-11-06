import Lean
import Proofs.Identity.Household
import Proofs.Request.CrossActorAuthorization
import Proofs.Request.ClientSignedEditHandoff

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

def discoveryCasesJson : String :=
  (Lean.toJson (["agent", "other"].map fun actor => Lean.Json.mkObj
    [("actor", Lean.toJson actor),
     ("visible", Lean.toJson (!(discover fixture [actor] "edit-doc").isEmpty)),
     ("selected", Lean.toJson (!(selectedForEffect (discover fixture [actor] "edit-doc") "agent").isEmpty))])).compress

inductive Outcome where
  | pending | applied | rejected
  deriving BEq, DecidableEq, Repr

/-- The immediate authenticated author's session, Agent and selected-request
permissions are observed in the same native transaction as the queue effect and receipt.
The existing cross-actor contract supplies those checks, including transaction
conflicts. A completed receipt is an observation, not a new claim, and does
not recheck or reapply after a later revocation. -/
def apply (prior : Outcome) (intent : Request.CrossActorAuthorization.Intent)
    (observation : Request.CrossActorAuthorization.Observation)
    (commit : Request.CrossActorAuthorization.NativeCommit)
    (selectedObserved selectedAtCommit : Option (List Bool)) : Outcome :=
  if prior != .pending then prior else
  match Request.CrossActorAuthorization.decideClaim intent observation commit with
  | .claimed =>
      match selectedObserved, selectedAtCommit with
      | some observed, some current =>
          if observed != current then .pending
          else if !observed.isEmpty && observed.all id then .applied else .rejected
      | _, _ => .pending
  | .denied => .rejected
  | _ => .pending

theorem committed_edit_not_rechecked
    (intent : Request.CrossActorAuthorization.Intent)
    (observation : Request.CrossActorAuthorization.Observation)
    (commit : Request.CrossActorAuthorization.NativeCommit)
    (observed current : Option (List Bool)) :
    apply .applied intent observation commit observed current = .applied := by rfl

theorem conflict_leaves_edit_pending :
    apply .pending Request.CrossActorAuthorization.exampleIntent
      Request.CrossActorAuthorization.permittedObservation ⟨false, true, true, true⟩ (some [true]) (some [true]) = .pending := by decide

theorem native_denial_rejects_without_effect :
    apply .pending Request.CrossActorAuthorization.exampleIntent
      Request.CrossActorAuthorization.deniedObservation ⟨false, true, true, true⟩ (some [true]) (some [true]) = .rejected := by decide

theorem matching_native_author_and_targets_can_apply :
    apply .pending Request.CrossActorAuthorization.exampleIntent
      Request.CrossActorAuthorization.permittedObservation ⟨true, true, true, true⟩ (some [true]) (some [true]) = .applied := by decide

theorem selected_request_denial_rejects :
    apply .pending Request.CrossActorAuthorization.exampleIntent
      Request.CrossActorAuthorization.permittedObservation ⟨true, true, true, true⟩ (some [true, false]) (some [true, false]) = .rejected := by decide

theorem selected_request_revoke_conflicts :
    apply .pending Request.CrossActorAuthorization.exampleIntent
      Request.CrossActorAuthorization.permittedObservation ⟨true, true, true, true⟩ (some [true]) (some [false]) = .pending := by decide

theorem missing_selected_request_observation_stays_pending :
    apply .pending Request.CrossActorAuthorization.exampleIntent
      Request.CrossActorAuthorization.permittedObservation ⟨true, true, true, true⟩ none none = .pending := by decide

theorem empty_selection_cannot_apply :
    apply .pending Request.CrossActorAuthorization.exampleIntent
      Request.CrossActorAuthorization.permittedObservation ⟨true, true, true, true⟩ (some []) (some []) = .rejected := by decide

/-- An interrupted request without a replacement does not identify the edit
that interrupted it. If the command's verified receipt is unavailable, that
observation cannot prove either a new success or a new rejection. -/
def unprovenRemovalReplay (hasVerifiedReceipt matchesRemoval : Bool) : Bool :=
  !hasVerifiedReceipt && matchesRemoval

theorem unproven_removal_remains_pending : unprovenRemovalReplay false true = true := by rfl

theorem verified_receipt_needs_no_removal_inference : unprovenRemovalReplay true true = false := by rfl

def claimCasesJson : String :=
  (Lean.toJson ([("allowed", true), ("selected-denied", false)].map fun (name, selected) =>
    Lean.Json.mkObj [("name", Lean.toJson name), ("selectedAllowed", Lean.toJson selected),
      ("rollbackReplacement", Lean.toJson ((Request.ClientSignedEditHandoff.stage
        Request.CrossActorAuthorization.exampleIntent
        ⟨"user", "session-doc", "agent-doc", "input", "evidence", true⟩
        "input" "evidence" ⟨true, true, false⟩).replacement)),
      ("rollbackReceipt", Lean.toJson ((Request.ClientSignedEditHandoff.stage
        Request.CrossActorAuthorization.exampleIntent
        ⟨"user", "session-doc", "agent-doc", "input", "evidence", true⟩
        "input" "evidence" ⟨true, true, false⟩).receipt)),
      ("tamperedAccepted", Lean.toJson ((Request.ClientSignedEditHandoff.stage
        Request.CrossActorAuthorization.exampleIntent
        ⟨"user", "session-doc", "agent-doc", "input", "evidence", true⟩
        "input" "changed" ⟨true, true, true⟩).replacement)),
      ("noRegrant", Lean.toJson (Request.ClientSignedEditHandoff.recover true ⟨true, true, true⟩ == ⟨false, false, false⟩)),
      ("unprovenRemovalPending", Lean.toJson (unprovenRemovalReplay false true)),
      ("applied", Lean.toJson (apply .pending Request.CrossActorAuthorization.exampleIntent
        Request.CrossActorAuthorization.permittedObservation ⟨true, true, true, true⟩
        (some [selected]) (some [selected]) == .applied))])).compress

end Identity.SessionInputEdit
