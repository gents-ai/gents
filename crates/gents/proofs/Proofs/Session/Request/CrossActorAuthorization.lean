import Lean
import Proofs.Session.CanonicalOutput.Terminal
import Proofs.Store.Identity.Household
import Proofs.Session.Request.CausalHop

namespace Request.CrossActorAuthorization

/-- The signed admission a claim retains, and the request it claims for the
selected Agent. `document` is the retained admission's request document,
`session` and `agentDocument` the physical targets it signed, `author` its
signer, `requester` the session requester it names and `content` its signed
payload with its signature. The claim transaction runs as `selectedAgent`, the
managed Agent the claimed request names, so no other executor reaches it. -/
structure Intent where
  document : String
  session : String
  agentDocument : String
  author : String
  selectedAgent : String
  requester : String
  content : String
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

/-- One read of a document by its id in the claim transaction. A document id
names one document, so the read finds no row (`absent`: missing, or not
readable by the selected Agent) or one row whose relationship marker is
still pending or complete (`ready`). -/
inductive RowRead where
  | absent | pending | ready
  deriving BEq, DecidableEq, Repr

/-- The request row the claim reads by the claimed request's document id.
`author` is `none` when the row's admission does not decode. -/
structure LiveRequest where
  document : String
  author : Option String
  content : String
  deriving Repr

/-- The session the store finds by the claimed request's node and session id. -/
structure NamedSession where
  document : String
  agent : String
  requester : String
  deriving Repr

/-- The Agent document the claim reads by the retained Agent document id. -/
structure AgentRow where
  document : String
  agent : String
  enabled : Bool
  deriving Repr

/-- What `ClientClaimAdmission::authorize_in_txn` observes in the claim
transaction, in its order: the request row and the retained session row with
their relationship markers, the live request's admission, the request's
owner, the session named by the request, the retained Agent document, and two
native access decisions for the signer. Each access decision is a named
document permission (`update` on the session, `invoke` on the Agent) that
DefraDB intersects with the signer's `DocumentUpdate` node permission, so a
signer without that node permission is denied both. A failed read is
`transientFailure`; it is an error the transaction owner sees, not a
decision. -/
structure Observation where
  request : RowRead
  session : RowRead
  live : LiveRequest
  owner : Option String
  namedSession : Option NamedSession
  agent : Option AgentRow
  sessionUpdate : Bool
  agentInvoke : Bool
  requesterNodePermission : Bool
  transientFailure : Bool := false
  deriving Repr

/-- `error` is a read that failed and propagates; `conflict` is the transaction
owner's report that every attempt lost its commit. -/
inductive Decision where
  | error | unavailable | denied | conflict | claimed
  deriving BEq, DecidableEq, Repr

/-- The live row is the retained admission's own document, carrying the
retained signer and the retained signed payload. Admission fields are
immutable, so on the retained document only an altered retained payload
breaks this; a second document with the same signed fields does not satisfy
it. -/
def bound (intent : Intent) (observation : Observation) : Bool :=
  observation.live.document == intent.document &&
    observation.live.author == some intent.author &&
    observation.live.content == intent.content

def sessionAccess (observation : Observation) : Bool :=
  observation.sessionUpdate && observation.requesterNodePermission

def agentAccess (observation : Observation) : Bool :=
  observation.agentInvoke && observation.requesterNodePermission

def sessionMatches (intent : Intent) (named : NamedSession) : Bool :=
  named.agent == intent.selectedAgent && named.requester == intent.requester

def agentMatches (intent : Intent) (agent : AgentRow) : Bool :=
  agent.document == intent.agentDocument && agent.agent == intent.selectedAgent && agent.enabled

/-- The claim decision over what the claim transaction observes. A missing or
not yet ready fact leaves the request pending (`unavailable`); a fact that
contradicts the retained admission, or a native denial, ends it (`denied`).
A request its own Agent signed needs only its binding, and an unbound one
stays pending. The session named by the request being another document than
the one it signed is a binding failure, not a missing fact. -/
def decideClaim (intent : Intent) (observation : Observation) : Decision :=
  if observation.transientFailure then .error
  else if observation.request != .ready || observation.session != .ready then .unavailable
  else if intent.author == intent.selectedAgent then
    if bound intent observation then .claimed else .unavailable
  else match observation.owner, observation.namedSession with
    | none, _ => .unavailable
    | _, none => .unavailable
    | some owner, some named =>
      if named.document != intent.session then .denied
      else match observation.agent with
        | none => .unavailable
        | some agent =>
          if bound intent observation && owner == intent.author && sessionMatches intent named &&
              agentMatches intent agent && sessionAccess observation && agentAccess observation then
            .claimed
          else .denied

/-- A claim or a terminal denial writes; only a writing decision can lose its
commit. -/
def Decision.writes : Decision → Bool
  | .claimed | .denied => true
  | _ => false

/-- The transaction owner's view of one attempt: a writing decision whose
commit fails reports the conflict instead. -/
def commitDecision (committed : Bool) (decision : Decision) : Decision :=
  if decision.writes && !committed then .conflict else decision

/-- Delivery selects the pending-work boundary; it does not replace the
native authorization of the immediate author when that work is claimed. -/
inductive ClientDelivery where
  | queue | steer
  deriving DecidableEq, Repr

def decideIdleClientClaim (_delivery : ClientDelivery) (intent : Intent)
    (observation : Observation) : Decision :=
  decideClaim intent observation

theorem idle_queue_and_steer_use_same_native_claim
    (intent : Intent) (observation : Observation) :
    decideIdleClientClaim .queue intent observation =
      decideIdleClientClaim .steer intent observation := by rfl

/-- A later steering input is a separate authored request. Its publication
and consumption require its own native claim decision at that transaction;
the earlier turn's requester is not reauthorized here. -/
def decideActiveSteer (intent : Intent) (observation : Observation) : Decision :=
  decideClaim intent observation

theorem active_steer_and_idle_use_same_claim (intent : Intent) (observation : Observation) :
    decideActiveSteer intent observation = decideIdleClientClaim .steer intent observation := by rfl

/-- An existing-session message is authored by the held calling Agent. The
source witness binds its actual parent request and tool call; descriptive IDs
alone do not establish that edge. The source is observed at publication and
again before claim; current session and Agent permissions are observed in the
claim transaction. The source observation is not modeled as a claim-time
transaction fence: in particular, the source tool call's session ID is not
declared immutable. Cross-runtime delivery requires the separate authenticated
HTTP and distributed receipt owner, which is not assumed here. -/
def decideLocalAgentMessage (isLocal sourceVerified sessionVisible : Bool) (intent : Intent)
    (observation : Observation) : Decision :=
  if !isLocal || !sourceVerified || !sessionVisible then .unavailable else decideClaim intent observation

theorem unverified_agent_message_cannot_claim (intent : Intent) (observation : Observation) :
    decideLocalAgentMessage true false true intent observation = .unavailable := by rfl

theorem hidden_existing_session_cannot_claim (intent : Intent) (observation : Observation) :
    decideLocalAgentMessage true true false intent observation = .unavailable := by rfl

theorem verified_local_message_uses_immediate_author_claim (intent : Intent)
    (observation : Observation) :
    decideLocalAgentMessage true true true intent observation = decideClaim intent observation := by rfl

theorem remote_agent_message_waits_for_http_owner (intent : Intent)
    (observation : Observation) (sourceVerified sessionVisible : Bool) :
    decideLocalAgentMessage false sourceVerified sessionVisible intent observation = .unavailable := by rfl

/-- A pending request changes only with the native commit. Rejection uses the
existing failed lifecycle and no-message terminal selection, and never exposes
which authorization fact was absent. Existing terminal work is unchanged. -/
structure Effect where
  lifecycle : RequestState := .pending
  committedBy : Option String := none
  terminalSelection : Option CanonicalOutput.TerminalSelection := none
  failureReason : Option String := none
  deriving Repr

def applyDecision (effect : Effect) (agent : String) : Decision → Effect
  | .claimed => if effect.lifecycle != .pending then effect else
      { effect with lifecycle := .claimed, committedBy := some agent }
  | .denied => if effect.lifecycle != .pending then effect else
      { effect with
        lifecycle := .failed,
        terminalSelection := some .noMessage,
        failureReason := some "request authorization denied" }
  | _ => effect

def applyClaim (effect : Effect) (intent : Intent) (observation : Observation) : Effect :=
  applyDecision effect intent.selectedAgent (decideClaim intent observation)

/-- A write another actor commits while a claim attempt is open. -/
inductive Write where
  | revokeSessionUpdate | grantSessionUpdate
  | revokeInvoke | grantInvoke
  | revokeNodePermission | grantNodePermission
  /-- An update to the Agent document that changes no fact the claim decides on. -/
  | touchAgent
  deriving BEq, DecidableEq, Repr

def Write.apply (observation : Observation) : Write → Observation
  | .revokeSessionUpdate => { observation with sessionUpdate := false }
  | .grantSessionUpdate => { observation with sessionUpdate := true }
  | .revokeInvoke => { observation with agentInvoke := false }
  | .grantInvoke => { observation with agentInvoke := true }
  | .revokeNodePermission => { observation with requesterNodePermission := false }
  | .grantNodePermission => { observation with requesterNodePermission := true }
  | .touchAgent => observation

def applyWrites (observation : Observation) (writes : List Write) : Observation :=
  writes.foldl Write.apply observation

/-- The claim transaction runs in the transaction owner's conflict-retry mode.
`race` commits after the attempt's first write and before its commit. Every
race write touches a fact the attempt read: a native relation or node
permission it checked, or the Agent document it read (external premise:
DefraDB's optimistic transactions conflict a committed write against an open
transaction's reads, native access checks included). The attempt is
discarded and the owner runs the claim again over the state the race left. A
decision that does not write commits nothing, so no race reaches it. -/
def claimRaced (intent : Intent) (observation : Observation) (race : List Write) : Decision :=
  let first := decideClaim intent observation
  if race.isEmpty || !first.writes then first
  else decideClaim intent (applyWrites observation race)

theorem failed_binding_cannot_claim (intent : Intent) (observation : Observation)
    (unbound : bound intent observation = false) :
    decideClaim intent observation ≠ .claimed := by
  unfold decideClaim
  repeat' split
  all_goals simp_all

theorem active_steer_denial_has_no_claim (intent : Intent) (observation : Observation)
    (unbound : bound intent observation = false) :
    decideActiveSteer intent observation ≠ .claimed :=
  failed_binding_cannot_claim intent observation unbound

/-- A failed read is not a decision: the request keeps its effect. -/
theorem transient_failure_leaves_effect_unchanged (effect : Effect) (intent : Intent)
    (observation : Observation) (failed : observation.transientFailure = true) :
    applyClaim effect intent observation = effect := by
  unfold applyClaim
  rw [show decideClaim intent observation = .error by simp [decideClaim, failed]]
  rfl

/-- The signer's node permission is part of both native access decisions, so
no claim proceeds for a signer without it. -/
theorem missing_node_permission_cannot_claim (intent : Intent) (observation : Observation)
    (crossActor : (intent.author == intent.selectedAgent) = false)
    (revoked : observation.requesterNodePermission = false) :
    decideClaim intent observation ≠ .claimed := by
  unfold decideClaim
  repeat' split
  all_goals simp_all [sessionAccess]

def exampleIntent : Intent :=
  { document := "request-doc", session := "session-doc", agentDocument := "agent-doc",
    author := "user", selectedAgent := "agent", requester := "user", content := "signed" }

def permittedObservation : Observation :=
  { request := .ready, session := .ready,
    live := { document := "request-doc", author := some "user", content := "signed" },
    owner := some "user",
    namedSession := some { document := "session-doc", agent := "agent", requester := "user" },
    agent := some { document := "agent-doc", agent := "agent", enabled := true },
    sessionUpdate := true, agentInvoke := true, requesterNodePermission := true }

def deniedObservation : Observation := { permittedObservation with agentInvoke := false }

/-- A collaborator signs into the user's session: its update on the session
comes from a relation the session's owner grants and revokes. -/
def collaboratorIntent : Intent := { exampleIntent with author := "collaborator" }

def collaboratorObservation : Observation :=
  { permittedObservation with
    live := { permittedObservation.live with author := some "collaborator" },
    owner := some "collaborator" }

theorem committed_claim_belongs_to_selected_agent :
    (applyClaim {} exampleIntent permittedObservation).committedBy =
      some exampleIntent.selectedAgent := by decide

/-- A row whose relationship marker is still pending, or that the selected
Agent cannot read, leaves the request pending. -/
theorem unready_rows_leave_request_pending :
    decideClaim exampleIntent { permittedObservation with request := .pending } = .unavailable ∧
    decideClaim exampleIntent { permittedObservation with request := .absent } = .unavailable ∧
    decideClaim exampleIntent { permittedObservation with session := .pending } = .unavailable := by
  decide

/-- A second document carrying the retained admission's signed fields is not
the retained admission. -/
theorem replayed_document_is_denied :
    decideClaim exampleIntent
      { permittedObservation with live := { permittedObservation.live with document := "replayed-doc" } } =
      .denied := by decide

/-- The live admission decodes to the retained signer on the retained
document, since admission fields are immutable; an admission that does not
decode, or names another signer, is a binding failure. -/
theorem unauthenticated_or_foreign_author_is_denied :
    decideClaim exampleIntent
      { permittedObservation with live := { permittedObservation.live with author := none } } =
      .denied ∧
    decideClaim exampleIntent
      { permittedObservation with live := { permittedObservation.live with author := some "other" } } =
      .denied := by decide

/-- The owner observation reads the request the transaction has just read, and
every AgentRequest is created by an authenticated writer that DefraDB records
as its owner, so an absent owner is a missing fact. -/
theorem missing_owner_leaves_request_pending :
    decideClaim exampleIntent { permittedObservation with owner := none } = .unavailable := by decide

/-- The session named by the request and its signed session document differ:
the signed target does not bind this request, so the claim ends. -/
theorem wrong_session_is_denied :
    decideClaim exampleIntent
      { permittedObservation with
        namedSession := some { document := "other-session-doc", agent := "agent", requester := "user" } } =
      .denied := by decide

theorem named_session_of_another_agent_or_requester_is_denied :
    decideClaim exampleIntent
      { permittedObservation with
        namedSession := some { document := "session-doc", agent := "other", requester := "user" } } =
      .denied ∧
    decideClaim exampleIntent
      { permittedObservation with
        namedSession := some { document := "session-doc", agent := "agent", requester := "other" } } =
      .denied := by decide

/-- A request its own Agent signed claims on its binding alone, and an unbound
one waits. -/
theorem self_signed_claim_needs_only_its_binding :
    decideClaim { exampleIntent with author := "agent" }
      { permittedObservation with
        live := { permittedObservation.live with author := some "agent" },
        sessionUpdate := false,
        agentInvoke := false } = .claimed ∧
    decideClaim { exampleIntent with author := "agent", content := "altered" }
      { permittedObservation with live := { permittedObservation.live with author := some "agent" } } =
      .unavailable := by decide

/-- A revoke the race commits before the claim's commit is decided by the
rerun; a revoke followed by a regrant leaves the claim to proceed. -/
theorem raced_revoke_is_decided_by_the_rerun :
    claimRaced exampleIntent permittedObservation [.revokeNodePermission] = .denied ∧
    claimRaced exampleIntent permittedObservation [.revokeInvoke] = .denied ∧
    claimRaced collaboratorIntent collaboratorObservation [.revokeSessionUpdate] = .denied ∧
    claimRaced exampleIntent permittedObservation [.revokeInvoke, .grantInvoke] = .claimed ∧
    claimRaced collaboratorIntent { collaboratorObservation with sessionUpdate := false }
      [.grantSessionUpdate] = .claimed := by decide

theorem committed_denial_publishes_terminal_rejection :
    let result := applyClaim {} exampleIntent deniedObservation
    result.lifecycle = .failed ∧ result.terminalSelection = some .noMessage ∧
      result.failureReason = some "request authorization denied" ∧ result.committedBy = none := by decide

theorem lost_commit_leaves_request_pending :
    let result := applyDecision {} "agent" (commitDecision false (decideClaim exampleIntent deniedObservation))
    result.lifecycle = .pending ∧ result.terminalSelection = none ∧
      result.failureReason = none ∧ result.committedBy = none := by decide

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
    (observation : Observation) (state : Identity.Household.State)
    (requester resource id permission : String) (pending : effect.lifecycle = .pending)
    (claimed : decideClaim intent observation = .claimed) :
    effectAllowed (applyClaim effect intent observation) state requester resource id permission =
      Identity.Household.allowed 5 state intent.selectedAgent resource id permission := by
  simp [effectAllowed, activeTurn, applyClaim, applyDecision, pending, claimed,
    Identity.Household.agentEffectAllowed]

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

def remoteClaimAllowed (intent : Intent) (observation : Observation)
    (nativeRemoteNodePermission : Bool) : Bool :=
  nativeRemoteNodePermission && decideClaim intent observation == .claimed

theorem remote_node_permission_is_required (intent : Intent) (observation : Observation) :
    remoteClaimAllowed intent observation false = false := by
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
      ⟨⟨document, session, agentDocument, author, target, author, document⟩,
        CausalHop.nextHop (.crossSession causeHop) targetHop⟩
  else none

theorem outward_author_is_the_immediate_parent_agent (parent : Effect)
    (agent document session agentDocument target : String) (causeHop targetHop : Nat)
    (active : parent.lifecycle = .claimed) (author : parent.committedBy = some agent) :
    outbound parent document session agentDocument target causeHop targetHop =
      some ⟨⟨document, session, agentDocument, agent, target, agent, document⟩,
        CausalHop.nextHop (.crossSession causeHop) targetHop⟩ := by
  simp [outbound, activeTurn, active, author]

theorem outward_hop_exceeds_its_cause (causeHop targetHop : Nat) :
    causeHop + 1 ≤ CausalHop.nextHop (.crossSession causeHop) targetHop :=
  CausalHop.cross_session_exceeds_cause causeHop targetHop

/-- One replayed claim: the retained `intent`, the facts the claim observes,
`before` (writes committed before the claim runs) and `race` (writes
committed while its first attempt is open), on a request whose effect is
`prior`. -/
structure Case where
  name : String
  intent : Intent := exampleIntent
  observation : Observation := permittedObservation
  before : List Write := []
  race : List Write := []
  prior : Effect := {}
  deriving Repr

def Case.checkedObservation (c : Case) : Observation := applyWrites c.observation c.before

def Case.decision (c : Case) : Decision := decideClaim c.intent c.checkedObservation

def Case.effect (c : Case) : Effect :=
  applyDecision c.prior c.intent.selectedAgent (claimRaced c.intent c.checkedObservation c.race)

private def withLive (live : LiveRequest) : Observation := { permittedObservation with live }

def cases : List Case :=
  [ { name := "committed_claim" }
  , { name := "invalid_signature", intent := { exampleIntent with content := "altered" } }
  , { name := "invoke_denied_at_check", observation := deniedObservation }
  , { name := "node_denied_at_check",
      observation := { permittedObservation with requesterNodePermission := false } }
  , { name := "session_denied_at_check", intent := collaboratorIntent,
      observation := { collaboratorObservation with sessionUpdate := false } }
  , { name := "request_relationships_pending",
      observation := { permittedObservation with request := .pending } }
  , { name := "session_relationships_pending",
      observation := { permittedObservation with session := .pending } }
  , { name := "hidden_request",
      observation := { withLive { permittedObservation.live with document := "replayed-doc" } with
        request := .absent } }
  , { name := "replayed_document",
      observation := withLive { permittedObservation.live with document := "replayed-doc" } }
  , { name := "missing_authenticated_author",
      observation := withLive { document := "replayed-doc", author := none, content := "signed" } }
  , { name := "foreign_owner", observation := { permittedObservation with owner := some "collaborator" } }
  , { name := "wrong_session",
      observation := { permittedObservation with
        namedSession := some { document := "session-doc", agent := "agent", requester := "user" } },
      intent := { exampleIntent with session := "other-session-doc" } }
  , { name := "wrong_agent_document",
      intent := { exampleIntent with agentDocument := "other-agent-doc" },
      observation := { permittedObservation with
        agent := some { document := "other-agent-doc", agent := "other-agent", enabled := true } } }
  , { name := "agent_disabled",
      observation := { permittedObservation with
        agent := some { document := "agent-doc", agent := "agent", enabled := false } } }
  , { name := "hidden_target", observation := { permittedObservation with agent := none } }
  , { name := "transient_check_failure",
      observation := { permittedObservation with transientFailure := true } }
  , { name := "regrant_before_observation", before := [.revokeNodePermission, .grantNodePermission] }
  , { name := "node_revoked_before_commit", race := [.revokeNodePermission] }
  , { name := "node_regrant_serializes_before_claim",
      race := [.revokeNodePermission, .grantNodePermission] }
  , { name := "invoke_revoked_before_commit", race := [.revokeInvoke] }
  , { name := "invoke_regrant_serializes_before_claim", race := [.revokeInvoke, .grantInvoke] }
  , { name := "session_revoked_before_commit", intent := collaboratorIntent,
      observation := collaboratorObservation, race := [.revokeSessionUpdate] }
  , { name := "session_regrant_serializes_before_claim", intent := collaboratorIntent,
      observation := collaboratorObservation, race := [.revokeSessionUpdate, .grantSessionUpdate] }
  , { name := "native_commit_conflict", race := [.touchAgent] }
  , { name := "denial_commit_conflict", observation := deniedObservation, race := [.touchAgent] }
  , { name := "denial_regrant_conflict", intent := collaboratorIntent,
      observation := { collaboratorObservation with sessionUpdate := false },
      race := [.grantSessionUpdate] }
  , { name := "existing_effect_unchanged",
      prior := { lifecycle := .claimed, committedBy := some "agent" } } ]

open Lean

def decisionString : Decision → String
  | .error => "error"
  | .unavailable => "unavailable"
  | .denied => "denied"
  | .conflict => "conflict"
  | .claimed => "claimed"

def rowReadString : RowRead → String
  | .absent => "absent"
  | .pending => "pending"
  | .ready => "ready"

def Write.wire : Write → String
  | .revokeSessionUpdate => "revoke_session_update"
  | .grantSessionUpdate => "grant_session_update"
  | .revokeInvoke => "revoke_invoke"
  | .grantInvoke => "grant_invoke"
  | .revokeNodePermission => "revoke_node_permission"
  | .grantNodePermission => "grant_node_permission"
  | .touchAgent => "touch_agent"

def terminalSelectionString : CanonicalOutput.TerminalSelection → String
  | .noMessage => "no_message"
  | .message _ => "message"


/-! Pending rejection: a request the runtime rejects before admission is
terminalized only through the runtime's own distinct managed Agent, and only
when the selected identity is that Agent. -/

inductive PendingRejectionOutcome where
  | invalidAgent
  | notMatched

structure PendingRejectionSelection where
  nodeIsLocal : Bool
  agentIsNode : Bool
  identityIsAgent : Bool

/-- The runtime must be the local node, the requested Agent must be distinct
from it, and the selected identity must be that Agent. The request document is
absent in this model, so an authorized rejection matches no row; a present
pending row is terminalized by the transaction, which is not decided here. -/
def pendingRejectionOutcome (s : PendingRejectionSelection) : PendingRejectionOutcome :=
  if s.nodeIsLocal && !s.agentIsNode && s.identityIsAgent then .notMatched else .invalidAgent

theorem pending_rejection_requires_the_selected_managed_agent
    (s : PendingRejectionSelection) (h : s.identityIsAgent = false) :
    pendingRejectionOutcome s = .invalidAgent := by
  simp [pendingRejectionOutcome, h]

end Request.CrossActorAuthorization
