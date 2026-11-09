import Lean
import Proofs.CanonicalOutput.Terminal
import Proofs.Identity.Household
import Proofs.Request.CausalHop

namespace Request.CrossActorAuthorization

/-- The document ID and selected physical session and Agent are retained with
verified provenance. The native owner is an observation of the live document,
not a writable author field or a node identity fallback. -/
structure Intent where
  document : String
  session : String
  agentDocument : String
  author : String
  selectedAgent : String
  deriving Repr

/-- The client signs both physical target identifiers before publication. A
signature for either different document cannot authenticate this intent. -/
def signedTargetsMatch (signedSession signedAgent : String) (intent : Intent) : Bool :=
  signedSession == intent.session && signedAgent == intent.agentDocument

theorem changed_signed_session_rejected (intent : Intent) (session : String)
    (different : (session == intent.session) = false) :
    signedTargetsMatch session intent.agentDocument intent = false := by
  simp [signedTargetsMatch, different]

theorem changed_signed_agent_rejected (intent : Intent) (agent : String)
    (different : (agent == intent.agentDocument) = false) :
    signedTargetsMatch intent.session agent intent = false := by
  simp [signedTargetsMatch, different]

/-- A supported implementation must acquire these observations inside the
same native transaction that commits the selected agent's claim or command
effect. `owner` and `authenticatedAuthor` are independently authenticated
native facts. Signature verification authenticates the complete intent. -/
structure Observation where
  supported : Bool
  visible : Bool
  targetsVisible : Bool
  transientFailure : Bool
  document : String
  session : String
  agentDocument : String
  owner : Option String
  authenticatedAuthor : Option String
  executingAgent : String
  signatureValid : Bool
  sessionPermission : Bool
  agentInvoke : Bool
  nodePermissionEnforced : Bool
  requesterNodePermission : Bool
  deriving Repr

/-- These are native commit outcomes, not application-side authorization
checks. `sessionPermission`, `agentInvoke`, and, when enabled,
`requesterNodePermission` describe permissions for the authenticated requester
at the claim's serialization point. `committed` is false when the storage
commit fails or detects a conflicting native dependency. -/
structure NativeCommit where
  sessionPermission : Bool
  agentInvoke : Bool
  committed : Bool
  requesterNodePermission : Bool
  deriving Repr

inductive Decision where
  | blocked | unavailable | denied | conflict | claimed
  deriving BEq, DecidableEq, Repr

def bound (intent : Intent) (observation : Observation) : Bool :=
  observation.visible && observation.signatureValid &&
    observation.document == intent.document &&
    signedTargetsMatch observation.session observation.agentDocument intent &&
    observation.owner == some intent.author &&
    observation.authenticatedAuthor == some intent.author &&
    observation.executingAgent == intent.selectedAgent

/-- This conditional contract applies to a cross-actor request claim or queued
command. The native permission observation and a terminal rejection or claim
must commit in the same transaction. Missing authority facts, disabled native
checks and transient failures leave the work pending. A native dependency
change or commit failure rolls back both rejection and claim. -/
def decideClaim (intent : Intent) (observation : Observation) (commit : NativeCommit) : Decision :=
  if !observation.supported || !observation.nodePermissionEnforced then .blocked
  else if observation.transientFailure || !observation.visible || !observation.targetsVisible ||
      observation.owner.isNone || observation.authenticatedAuthor.isNone ||
      observation.document != intent.document || observation.executingAgent != intent.selectedAgent then
    .unavailable
  else if !commit.committed || observation.sessionPermission != commit.sessionPermission ||
      observation.agentInvoke != commit.agentInvoke ||
      observation.requesterNodePermission != commit.requesterNodePermission then .conflict
  else if !bound intent observation || !observation.sessionPermission || !observation.agentInvoke ||
      !observation.requesterNodePermission then .denied
  else .claimed

/-- Delivery selects the pending-work boundary; it does not replace the
native authorization of the immediate author when that work is claimed. -/
inductive ClientDelivery where
  | queue | steer
  deriving DecidableEq, Repr

def decideIdleClientClaim (_delivery : ClientDelivery) (intent : Intent)
    (observation : Observation) (commit : NativeCommit) : Decision :=
  decideClaim intent observation commit

theorem idle_queue_and_steer_use_same_native_claim
    (intent : Intent) (observation : Observation) (commit : NativeCommit) :
    decideIdleClientClaim .queue intent observation commit =
      decideIdleClientClaim .steer intent observation commit := by rfl

/-- A later steering input is a separate authored request. Its publication
and consumption require its own native claim decision at that transaction;
the earlier turn's requester is not reauthorized here. -/
def decideActiveSteer (intent : Intent) (observation : Observation)
    (commit : NativeCommit) : Decision :=
  decideClaim intent observation commit

theorem active_steer_and_idle_use_same_claim (intent : Intent) (observation : Observation)
    (commit : NativeCommit) :
    decideActiveSteer intent observation commit = decideIdleClientClaim .steer intent observation commit := by rfl

/-- An existing-session message is authored by the held calling Agent. The
source witness binds its actual parent request and tool call; descriptive IDs
alone do not establish that edge. The source is observed at publication and
again before claim; current session and Agent permissions are observed in the
claim transaction. The source observation is not modeled as a claim-time
transaction fence: in particular, the source tool call's session ID is not
declared immutable. Cross-runtime delivery requires the separate authenticated
HTTP and distributed receipt owner, which is not assumed here. -/
def decideLocalAgentMessage (isLocal sourceVerified sessionVisible : Bool) (intent : Intent)
    (observation : Observation) (commit : NativeCommit) : Decision :=
  if !isLocal || !sourceVerified || !sessionVisible then .unavailable else decideClaim intent observation commit

theorem unverified_agent_message_cannot_claim (intent : Intent) (observation : Observation)
    (commit : NativeCommit) :
    decideLocalAgentMessage true false true intent observation commit = .unavailable := by rfl

theorem hidden_existing_session_cannot_claim (intent : Intent) (observation : Observation)
    (commit : NativeCommit) :
    decideLocalAgentMessage true true false intent observation commit = .unavailable := by rfl

theorem verified_local_message_uses_immediate_author_claim (intent : Intent)
    (observation : Observation) (commit : NativeCommit) :
    decideLocalAgentMessage true true true intent observation commit = decideClaim intent observation commit := by rfl

theorem remote_agent_message_waits_for_http_owner (intent : Intent)
    (observation : Observation) (commit : NativeCommit) (sourceVerified sessionVisible : Bool) :
    decideLocalAgentMessage false sourceVerified sessionVisible intent observation commit = .unavailable := by rfl

/-- A pending request changes only with the native commit. Rejection uses the
existing failed lifecycle and no-message terminal selection, and never exposes
which authorization fact was absent. Existing terminal work is unchanged. -/
structure Effect where
  lifecycle : RequestState := .pending
  committedBy : Option String := none
  terminalSelection : Option CanonicalOutput.TerminalSelection := none
  failureReason : Option String := none
  deriving Repr

def applyClaim (effect : Effect) (intent : Intent) (observation : Observation)
    (commit : NativeCommit) : Effect :=
  if effect.lifecycle != .pending then effect else
  match decideClaim intent observation commit with
  | .claimed => { effect with lifecycle := .claimed, committedBy := some intent.selectedAgent }
  | .denied => { effect with lifecycle := .failed, terminalSelection := some .noMessage, failureReason := some "request authorization denied" }
  | _ => effect

theorem unsupported_cross_actor_is_blocked (intent : Intent) (observation : Observation)
    (commit : NativeCommit) (unsupported : observation.supported = false) :
    decideClaim intent observation commit = .blocked := by
  simp [decideClaim, unsupported]

theorem failed_binding_cannot_claim (intent : Intent) (observation : Observation)
    (commit : NativeCommit) (unbound : bound intent observation = false) :
    decideClaim intent observation commit ≠ .claimed := by
  simp [decideClaim, unbound]
  all_goals split <;> simp_all
  all_goals split <;> simp_all
  all_goals split <;> simp_all

theorem active_steer_denial_has_no_claim (intent : Intent) (observation : Observation)
    (commit : NativeCommit) (unbound : bound intent observation = false) :
    decideActiveSteer intent observation commit ≠ .claimed := by
  exact failed_binding_cannot_claim intent observation commit unbound

theorem uncommitted_agent_effect_cannot_claim (intent : Intent) (observation : Observation)
    (commit : NativeCommit) (failed : commit.committed = false) :
    decideClaim intent observation commit ≠ .claimed := by
  simp [decideClaim, failed]
  all_goals split <;> simp_all
  all_goals split <;> simp_all
  all_goals split <;> simp_all

theorem unavailable_check_leaves_effect_unchanged (effect : Effect) (intent : Intent)
    (observation : Observation) (commit : NativeCommit) (unsupported : observation.supported = false) :
    applyClaim effect intent observation commit = effect := by
  simp [applyClaim, unsupported_cross_actor_is_blocked intent observation commit unsupported]

def exampleIntent : Intent := ⟨"request-doc", "session-doc", "agent-doc", "user", "agent"⟩
def permittedObservation : Observation :=
  ⟨true, true, true, false, "request-doc", "session-doc", "agent-doc", some "user", some "user",
    "agent", true, true, true, true, true⟩

theorem revoke_before_commit_conflicts :
    decideClaim exampleIntent permittedObservation ⟨false, true, true, true⟩ = .conflict ∧
    decideClaim exampleIntent permittedObservation ⟨true, false, true, true⟩ = .conflict := by decide

theorem enforced_node_permission_revoked_before_commit_conflicts :
    decideClaim exampleIntent permittedObservation ⟨true, true, true, false⟩ = .conflict := by decide

theorem disabled_node_gate_remains_blocked :
    decideClaim exampleIntent
      { permittedObservation with nodePermissionEnforced := false, requesterNodePermission := false }
      ⟨true, true, true, false⟩ = .blocked := by decide

/-- A revoke followed by regrant of the same permission can serialize before
this claim. The model permits a successful commit when both permissions hold at
that serialization point, including the requester node permission when enabled;
it does not claim that an earlier revocation remains
an invalidating event. The native engine may still report a transaction conflict. -/
theorem regrant_can_serialize_before_claim :
    decideClaim exampleIntent permittedObservation ⟨true, true, true, true⟩ = .claimed ∧
    decideClaim exampleIntent permittedObservation ⟨true, true, false, true⟩ = .conflict := by decide

theorem committed_claim_belongs_to_selected_agent :
    (applyClaim {} exampleIntent permittedObservation ⟨true, true, true, true⟩).committedBy =
      some exampleIntent.selectedAgent := by decide

/-- Once claimed, the selected Agent acts as its own principal. The original
requester's permission is checked at claim, not intersected with the Agent's
later native ACP permissions. The ordinary Agent permission check still applies
to each effect, alongside the effect owner's non-authorization constraints. -/
def activeTurn : RequestState → Bool
  | .claimed | .processing => true
  | _ => false

def effectAllowed (effect : Effect) (state : Identity.Household.State)
    (_requester : String) (resource id permission : String) : Bool :=
  if activeTurn effect.lifecycle then
    match effect.committedBy with
    | some agent => Identity.Household.agentEffectAllowed state agent resource id permission
    | none => false
  else false

theorem claimed_effect_uses_full_selected_agent_authority (effect : Effect) (intent : Intent)
    (observation : Observation) (commit : NativeCommit) (state : Identity.Household.State)
    (requester resource id permission : String) (pending : effect.lifecycle = .pending)
    (claimed : decideClaim intent observation commit = .claimed) :
    effectAllowed (applyClaim effect intent observation commit) state requester resource id permission =
      Identity.Household.allowed 5 state intent.selectedAgent resource id permission := by
  simp [effectAllowed, activeTurn, applyClaim, pending, claimed, Identity.Household.agentEffectAllowed]

theorem changing_requester_does_not_narrow_claimed_effect (effect : Effect)
    (state : Identity.Household.State) (first second resource id permission : String) :
    effectAllowed effect state first resource id permission =
      effectAllowed effect state second resource id permission := rfl

/-- Requester authorization is checked at claim. Lease renewal and completion
remain subject to their native lease and completion owners, but they do not
recheck the requester or terminate a claimed turn solely for a later revoke. -/
def renewClaimedTurn (effect : Effect) (_requesterStillAllowed : Bool)
    (nativeLeaseAllows : Bool) : Bool :=
  activeTurn effect.lifecycle && nativeLeaseAllows

def completeClaimedTurn (effect : Effect) (_requesterStillAllowed : Bool)
    (nativeCompletionAllows : Bool) : Bool :=
  activeTurn effect.lifecycle && nativeCompletionAllows

theorem requester_revoke_does_not_change_lease_renewal (effect : Effect) (nativeAllows : Bool) :
    renewClaimedTurn effect true nativeAllows = renewClaimedTurn effect false nativeAllows := rfl

theorem requester_revoke_does_not_change_completion (effect : Effect) (nativeAllows : Bool) :
    completeClaimedTurn effect true nativeAllows = completeClaimedTurn effect false nativeAllows := rfl

theorem claimed_turn_can_finish_after_requester_revoke :
    completeClaimedTurn { lifecycle := .claimed, committedBy := some "agent" } false true = true := by decide

/-- Interactive users, parent Agents and remote Agents each authenticate the
immediate signed author and pass the same per-request transaction check.
Unattended work instead uses standing user-granted Agent ACP. Remote admission
also requires the native node permission for the remote actor. -/
inductive Origin where
  | interactive | unattended | parentAgent | remoteAgent
  deriving DecidableEq, Repr

def usesPerRequestCheck : Origin → Bool
  | .interactive | .parentAgent | .remoteAgent => true
  | .unattended => false

theorem per_request_edges_check_the_immediate_author :
    usesPerRequestCheck .interactive = true ∧ usesPerRequestCheck .parentAgent = true ∧
      usesPerRequestCheck .remoteAgent = true ∧ usesPerRequestCheck .unattended = false := by decide

def remoteClaimAllowed (intent : Intent) (observation : Observation) (commit : NativeCommit)
    (nativeRemoteNodePermission : Bool) : Bool :=
  nativeRemoteNodePermission && decideClaim intent observation commit == .claimed

theorem remote_node_permission_is_required (intent : Intent) (observation : Observation)
    (commit : NativeCommit) : remoteClaimAllowed intent observation commit false = false := by
  simp [remoteClaimAllowed]

structure Outbound where
  intent : Intent
  hop : Nat
  deriving Repr

/-- An outward delegation is authored by the current Agent, not by any
earlier requester. Its CausalHop tracks chain depth only; admission validates
the new immediate author's signature, native ownership and target permissions. -/
def outbound (parent : Effect) (document session agentDocument target : String)
    (causeHop targetHop : Nat) : Option Outbound :=
  if activeTurn parent.lifecycle then
    parent.committedBy.map fun author =>
      ⟨⟨document, session, agentDocument, author, target⟩,
        CausalHop.nextHop (.crossSession causeHop) targetHop⟩
  else none

theorem outward_author_is_the_immediate_parent_agent (parent : Effect)
    (agent document session agentDocument target : String) (causeHop targetHop : Nat)
    (active : parent.lifecycle = .claimed) (author : parent.committedBy = some agent) :
    outbound parent document session agentDocument target causeHop targetHop =
      some ⟨⟨document, session, agentDocument, agent, target⟩,
        CausalHop.nextHop (.crossSession causeHop) targetHop⟩ := by
  simp [outbound, activeTurn, active, author]

theorem outward_hop_exceeds_its_cause (causeHop targetHop : Nat) :
    causeHop + 1 ≤ CausalHop.nextHop (.crossSession causeHop) targetHop :=
  CausalHop.cross_session_exceeds_cause causeHop targetHop

def deniedObservation : Observation := { permittedObservation with sessionPermission := false }

theorem committed_denial_publishes_terminal_rejection :
    let result := applyClaim {} exampleIntent deniedObservation ⟨false, true, true, true⟩
    result.lifecycle = .failed ∧ result.terminalSelection = some .noMessage ∧
      result.failureReason = some "request authorization denied" ∧ result.committedBy = none := by decide

theorem denial_commit_conflict_leaves_request_pending :
    let result := applyClaim {} exampleIntent deniedObservation ⟨false, true, false, true⟩
    result.lifecycle = .pending ∧ result.terminalSelection = none ∧
      result.failureReason = none ∧ result.committedBy = none := by decide

theorem concurrent_regrant_rolls_back_denial :
    decideClaim exampleIntent deniedObservation ⟨true, true, true, true⟩ = .conflict := by decide

theorem hidden_or_transient_check_leaves_request_pending :
    (applyClaim {} exampleIntent { permittedObservation with targetsVisible := false }
      ⟨true, true, true, true⟩).lifecycle = .pending ∧
    (applyClaim {} exampleIntent { permittedObservation with transientFailure := true }
      ⟨true, true, true, true⟩).lifecycle = .pending := by decide

theorem signed_but_foreign_native_owner_is_denied :
    decideClaim exampleIntent { permittedObservation with owner := some "other" }
      ⟨true, true, true, true⟩ = .denied := by decide

theorem unauthenticated_author_is_unavailable :
    decideClaim exampleIntent { permittedObservation with authenticatedAuthor := none }
      ⟨true, true, true, true⟩ = .unavailable := by decide

theorem selected_agent_and_physical_scope_are_bound :
    decideClaim exampleIntent { permittedObservation with executingAgent := "other" }
      ⟨true, true, true, true⟩ = .unavailable ∧
    decideClaim exampleIntent { permittedObservation with session := "other-session" }
      ⟨true, true, true, true⟩ = .denied ∧
    decideClaim exampleIntent { permittedObservation with agentDocument := "other-agent" }
      ⟨true, true, true, true⟩ = .denied ∧
    decideClaim exampleIntent { permittedObservation with document := "other-request" }
      ⟨true, true, true, true⟩ = .unavailable := by decide


inductive PermissionChange where
  | revoke | grant
  deriving Repr

def applyPermissionChange (_current : Bool) : PermissionChange → Bool
  | .revoke => false
  | .grant => true

def permissionAtCommit (initial : Bool) (changes : List PermissionChange) : Bool :=
  changes.foldl applyPermissionChange initial

structure Case where
  name : String
  intent : Intent
  observation : Observation
  beforeSessionChanges : List PermissionChange
  sessionChanges : List PermissionChange
  invokeChanges : List PermissionChange
  nodeChanges : List PermissionChange
  nativeCommitted : Bool
  prior : Effect
  deriving Repr

def Case.checkedObservation (c : Case) : Observation :=
  { c.observation with sessionPermission :=
      permissionAtCommit c.observation.sessionPermission c.beforeSessionChanges }

def Case.nativeCommit (c : Case) : NativeCommit :=
  ⟨permissionAtCommit c.checkedObservation.sessionPermission c.sessionChanges,
   permissionAtCommit c.observation.agentInvoke c.invokeChanges, c.nativeCommitted,
   permissionAtCommit c.observation.requesterNodePermission c.nodeChanges⟩

def testCase (name : String) (observation : Observation)
    (sessionChanges : List PermissionChange := []) (invokeChanges : List PermissionChange := [])
    (nativeCommitted : Bool := true) (prior : Effect := {}) : Case :=
  ⟨name, exampleIntent, observation, [], sessionChanges, invokeChanges, [], nativeCommitted, prior⟩

def cases : List Case :=
  [ testCase "unsupported" { permittedObservation with supported := false }
  , testCase "hidden" { permittedObservation with visible := false }
  , testCase "invalid_signature" { permittedObservation with signatureValid := false }
  , testCase "wrong_document" { permittedObservation with document := "other-request" }
  , testCase "wrong_session" { permittedObservation with session := "other-session" }
  , testCase "wrong_agent_document" { permittedObservation with agentDocument := "other-agent" }
  , testCase "missing_owner" { permittedObservation with owner := none }
  , testCase "foreign_owner" { permittedObservation with owner := some "other" }
  , testCase "missing_authenticated_author" { permittedObservation with authenticatedAuthor := none }
  , testCase "foreign_authenticated_author" { permittedObservation with authenticatedAuthor := some "other" }
  , testCase "wrong_executor" { permittedObservation with executingAgent := "other" }
  , testCase "session_denied_at_check" { permittedObservation with sessionPermission := false }
  , testCase "invoke_denied_at_check" { permittedObservation with agentInvoke := false }
  , testCase "node_denied_at_check" { permittedObservation with requesterNodePermission := false }
  , testCase "node_enforcement_disabled" { permittedObservation with nodePermissionEnforced := false, requesterNodePermission := false }
  , testCase "transient_check_failure" { permittedObservation with transientFailure := true }
  , testCase "hidden_target" { permittedObservation with targetsVisible := false }
  , testCase "denial_commit_conflict" { permittedObservation with sessionPermission := false } [] [] false
  , testCase "denial_regrant_conflict" { permittedObservation with sessionPermission := false } [.grant]
  , { testCase "regrant_before_observation" permittedObservation with beforeSessionChanges := [.revoke, .grant] }
  , { testCase "node_revoked_before_commit" permittedObservation with nodeChanges := [.revoke] }
  , { testCase "node_regrant_serializes_before_claim" permittedObservation with nodeChanges := [.revoke, .grant] }
  , testCase "session_revoked_before_commit" permittedObservation [.revoke]
  , testCase "invoke_revoked_before_commit" permittedObservation [] [.revoke]
  , testCase "session_regrant_serializes_before_claim" permittedObservation [.revoke, .grant]
  , testCase "session_regrant_may_still_conflict" permittedObservation [.revoke, .grant] [] false
  , testCase "invoke_regrant_serializes_before_claim" permittedObservation [] [.revoke, .grant]
  , testCase "native_commit_conflict" permittedObservation [] [] false
  , testCase "committed_claim" permittedObservation
  , testCase "existing_effect_unchanged" permittedObservation [] [] true { lifecycle := .claimed, committedBy := some "prior-agent" } ]

open Lean

def decisionString : Decision → String
  | .blocked => "blocked"
  | .unavailable => "unavailable"
  | .denied => "denied"
  | .conflict => "conflict"
  | .claimed => "claimed"

def changeString : PermissionChange → String
  | .revoke => "revoke"
  | .grant => "grant"

def intentJson (intent : Intent) : Json := Json.mkObj
  [("document", toJson intent.document), ("session", toJson intent.session),
   ("agent_document", toJson intent.agentDocument), ("author", toJson intent.author),
   ("selected_agent", toJson intent.selectedAgent)]

def observationJson (observation : Observation) : Json := Json.mkObj
  [("supported", toJson observation.supported), ("visible", toJson observation.visible),
   ("targets_visible", toJson observation.targetsVisible),
   ("transient_failure", toJson observation.transientFailure),
   ("document", toJson observation.document), ("session", toJson observation.session),
   ("agent_document", toJson observation.agentDocument), ("owner", toJson observation.owner),
   ("authenticated_author", toJson observation.authenticatedAuthor),
   ("executing_agent", toJson observation.executingAgent),
   ("signature_valid", toJson observation.signatureValid),
   ("session_permission", toJson observation.sessionPermission),
   ("agent_invoke", toJson observation.agentInvoke),
   ("node_permission_enforced", toJson observation.nodePermissionEnforced),
   ("requester_node_permission", toJson observation.requesterNodePermission)]

def terminalSelectionString : CanonicalOutput.TerminalSelection → String
  | .noMessage => "no_message"
  | .message _ => "message"

def caseJson (c : Case) : Json := Json.mkObj
  [("name", toJson c.name), ("intent", intentJson c.intent),
   ("observation", observationJson c.checkedObservation),
   ("before_session_changes", toJson (c.beforeSessionChanges.map changeString)),
   ("session_changes", toJson (c.sessionChanges.map changeString)),
   ("invoke_changes", toJson (c.invokeChanges.map changeString)),
   ("node_changes", toJson (c.nodeChanges.map changeString)),
   ("native_committed", toJson c.nativeCommitted), ("prior_effect", toJson c.prior.committedBy),
   ("prior_lifecycle", toJson c.prior.lifecycle.toDefraDB),
   ("prior_terminal_output", toJson (c.prior.terminalSelection.map terminalSelectionString)),
   ("prior_failure_reason", toJson c.prior.failureReason),
   ("decision", toJson (decisionString (decideClaim c.intent c.checkedObservation c.nativeCommit))),
   ("effect", toJson (applyClaim c.prior c.intent c.checkedObservation c.nativeCommit).committedBy),
   ("lifecycle", toJson (applyClaim c.prior c.intent c.checkedObservation c.nativeCommit).lifecycle.toDefraDB),
   ("terminal_output", toJson ((applyClaim c.prior c.intent c.checkedObservation c.nativeCommit).terminalSelection.map terminalSelectionString)),
   ("failure_reason", toJson (applyClaim c.prior c.intent c.checkedObservation c.nativeCommit).failureReason)]

def casesJson : String := (toJson (cases.map caseJson)).compress

end Request.CrossActorAuthorization
