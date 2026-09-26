import Proofs.Basic
import Proofs.Request.State

/-!
# Canonical descendant graph

`AgentToolCall` is the parent-authored durable edge receipt.  A child request
may materialize later (and under another principal), so visibility is defined
from the receipt and the root ownership tuple.  Once a child is materialized,
the logical and physical request/tool-call links must corroborate the receipt.

Visibility and control are intentionally separate.  An ancestor can inspect a
verified descendant edge without thereby acquiring the direct parent's
steer/cancel authority.
-/

namespace DescendantGraph

abbrev PrincipalId := Nat
abbrev ToolCallId := Nat
abbrev LineageId := Nat
abbrev Cursor := ToolCallId × RequestId

inductive AwaitMode where
  | foreground
  | background
  deriving DecidableEq, Repr

inductive Materialization where
  | pending
  | local
  | replicated
  deriving DecidableEq, Repr

inductive Lifecycle where
  | pending
  | running
  | completed
  | failed
  | timedOut
  | cancelled
  deriving DecidableEq, Repr

inductive Scope where
  | direct
  | descendants
  deriving DecidableEq, Repr

structure Viewer where
  rootRequestId : RequestId
  rootPrincipal : PrincipalId
  rootSessionId : SessionId
  lineageId : LineageId
  deriving DecidableEq, Repr

/-- Conversation control is shared across requests, not across principals or
sessions. The spawning request and its physical bridge remain unchanged.
An absent requester is an exact anonymous scope, never a wildcard. -/
structure SessionOwner where
  sessionId : String
  agentDid : String
  requesterDid : Option String
  deriving DecidableEq, Repr

def sameSessionOwner (caller owner : SessionOwner) : Bool :=
  caller.sessionId.trim != "" && caller.agentDid.trim != "" &&
    caller.sessionId == owner.sessionId && caller.agentDid == owner.agentDid &&
    caller.requesterDid == owner.requesterDid

theorem cross_session_denied (caller owner : SessionOwner)
    (h : caller.sessionId ≠ owner.sessionId) :
    sameSessionOwner caller owner = false := by
  simp [sameSessionOwner, h]

theorem cross_principal_denied (caller owner : SessionOwner)
    (h : caller.agentDid ≠ owner.agentDid) :
    sameSessionOwner caller owner = false := by
  simp [sameSessionOwner, h]

theorem cross_requester_denied (caller owner : SessionOwner)
    (h : caller.requesterDid ≠ owner.requesterDid) :
    sameSessionOwner caller owner = false := by
  simp [sameSessionOwner, h]

theorem missing_agent_denied (caller owner : SessionOwner)
    (h : caller.agentDid = "") : sameSessionOwner caller owner = false := by
  have emptyTrim : ("" : String).trim = "" := by native_decide
  simp [sameSessionOwner, h, emptyTrim]

theorem same_owner_survives_new_request (owner : SessionOwner)
    (hs : owner.sessionId.trim ≠ "")
    (ha : owner.agentDid.trim ≠ "") :
    sameSessionOwner owner owner = true := by
  simp [sameSessionOwner, hs, ha]

structure Edge where
  rootRequestId : RequestId
  rootSessionId : SessionId
  parentRequestId : RequestId
  parentToolCallId : ToolCallId
  childRequestId : RequestId
  childSessionId : Option SessionId
  ownerPrincipal : PrincipalId
  controlPrincipal : PrincipalId
  childPrincipal : PrincipalId
  behaviorId : BehaviorId
  lineageId : LineageId
  awaitMode : AwaitMode
  materialization : Materialization
  lifecycle : Lifecycle
  bridgeDurable : Bool
  physicalCorroborated : Bool
  directFromRoot : Bool
  deriving DecidableEq, Repr

def materializationAuthorized (edge : Edge) : Bool :=
  match edge.materialization with
  | .pending => true
  | .local | .replicated => edge.physicalCorroborated

/- The durable bridge receipt is enumerable by its owning lineage even when a
   materialized child fails physical corroboration. That diagnostic visibility
   never grants access to the child document itself. -/
def visible (viewer : Viewer) (edge : Edge) : Bool :=
  edge.bridgeDurable &&
    edge.rootRequestId == viewer.rootRequestId &&
    edge.ownerPrincipal == viewer.rootPrincipal &&
    edge.rootSessionId == viewer.rootSessionId &&
    edge.lineageId == viewer.lineageId

def readable (viewer : Viewer) (edge : Edge) : Bool :=
  visible viewer edge &&
    materializationAuthorized edge &&
    edge.materialization != .pending

def terminal : Lifecycle → Bool
  | .pending | .running => false
  | .completed | .failed | .timedOut | .cancelled => true

/-- Only an absent child can converge through retry. A child that exists but
    rejects the bridge's physical lineage is a permanent authorization result.
    A terminal bridge cannot converge further even if no child materialized. -/
def retryable (viewer : Viewer) (edge : Edge) : Bool :=
  visible viewer edge && edge.materialization == .pending && !terminal edge.lifecycle

/-- The ordinary active-child view retains every owned, nonterminal bridge,
    including a rejected child diagnostic. -/
def listedByDefault (viewer : Viewer) (edge : Edge) : Bool :=
  visible viewer edge && !terminal edge.lifecycle

/-- Control is narrower than visibility: only the direct owning principal may
    steer/cancel through this edge. -/
def controllable (viewer : Viewer) (edge : Edge) : Bool :=
  readable viewer edge &&
    edge.directFromRoot &&
    edge.controlPrincipal == viewer.rootPrincipal

/-- A later user turn may select the original owner's graph only after the
session authority check; canonical readability/direct-parent checks still apply. -/
def sessionControllable (caller owner : SessionOwner) (viewer : Viewer) (edge : Edge) : Bool :=
  sameSessionOwner caller owner && controllable viewer edge

theorem session_access_never_grants_ancestor_control
    (caller owner : SessionOwner) (viewer : Viewer) (edge : Edge)
    (h : edge.directFromRoot = false) :
    sessionControllable caller owner viewer edge = false := by
  simp [sessionControllable, controllable, h]

theorem session_control_requires_canonical_control
    (caller owner : SessionOwner) (viewer : Viewer) (edge : Edge)
    (h : sessionControllable caller owner viewer edge = true) :
    controllable viewer edge = true := by
  simp [sessionControllable] at h
  exact h.2

/-- Terminality of a child-session `AgentRequest`, as a decision. -/
def childRequestTerminal : RequestState → Bool
  | .completed | .failed | .superseded | .dead | .interrupted => true
  | .workspaceBindingPending | .pending | .claimed | .processing => false

theorem childRequestTerminal_iff (state : RequestState) :
    childRequestTerminal state = true ↔ isTerminal state := by
  cases state <;> decide

/-- Durable facts one steer reads beside the edge.

`child` is the corroborated child request's lifecycle, `none` when its row is
not readable. `spawnUnclaimed` and `cancelIntent` are the bridge's
unclaimed-spawn failure class and `cancel_cascade_intent_at` (#1851). On a
settled bridge either records that a spawn fence decided the child must stop,
and a steer must not override that decision by queueing new work in the child
session. `stuck_since` is deliberately not evidence: a parent interrupt stops
only the parent's own thread and marks its running tools stuck, while its
children keep running and stay steerable after they finish. -/
structure SteerEvidence where
  child : Option RequestState
  spawnUnclaimed : Bool
  cancelIntent : Bool
  deriving DecidableEq, Repr

def fenced (evidence : SteerEvidence) : Bool :=
  evidence.spawnUnclaimed || evidence.cancelIntent

/-- Outcome of one explicit `steer_subagent` call against a bridge edge.

`append` queues a steering request in the child session, whether the child is
still running or already finished (completed, failed or timed out). A steer is
a message to that session: the new request runs after any active request of
the session, never beside it, and the earlier request and its bridge keep
their outcome. Nothing is admitted without an explicit steer from the
controlling parent. -/
inductive SteerAdmission where
  | notAuthorized
  | notBackgrounded
  | cancelled
  | terminal
  | awaitingMaterialization
  | fenced
  | append
  deriving DecidableEq, Repr

/-- Only the direct controlling parent may steer. An edge that parent cancelled
stays cancelled. A settled bridge whose child never materialized has no
session to append to; a pending one may still converge. A settled bridge that
the unclaimed-spawn fence marked is refused. A child row that is not
readable is refused as unauthorized. -/
def steerAdmission (viewer : Viewer) (edge : Edge) (evidence : SteerEvidence) :
    SteerAdmission :=
  if !(visible viewer edge && edge.directFromRoot) then .notAuthorized
  else if edge.awaitMode != .background then .notBackgrounded
  else if edge.lifecycle == .cancelled then .cancelled
  else if !readable viewer edge then
    if terminal edge.lifecycle then .terminal else .awaitingMaterialization
  else if !controllable viewer edge then .notAuthorized
  else if terminal edge.lifecycle && fenced evidence then .fenced
  else match evidence.child with
    | some _ => .append
    | none => .notAuthorized

/-- Explicit cancellation through an edge (`cancel_subagent`) stops every live
request of the child session, including requests a steer queued after the
original child request, and leaves settled requests unchanged. A request
terminal never cascades into its subagents (#1624), so this edge is the
supervisor of all work a steer admits: the controlling parent that appended it
can always stop it. -/
def cancelChildSession (session : List RequestState) : List RequestState :=
  session.map fun state => if childRequestTerminal state then state else .interrupted

theorem append_requires_direct_control
    (viewer : Viewer) (edge : Edge) (evidence : SteerEvidence)
    (h : steerAdmission viewer edge evidence = .append) :
    controllable viewer edge = true ∧ edge.awaitMode = .background ∧
      edge.lifecycle ≠ .cancelled ∧ evidence.child.isSome ∧
      (terminal edge.lifecycle = true → fenced evidence = false) := by
  unfold steerAdmission at h
  split at h
  · simp at h
  rename_i hVis
  split at h
  · simp at h
  rename_i hMode
  split at h
  · simp at h
  rename_i hCancel
  split at h
  · split at h <;> simp at h
  rename_i hRead
  split at h
  · simp at h
  rename_i hControl
  split at h
  · simp at h
  rename_i hFence
  split at h
  · rename_i state hChild
    simp only [bne_iff_ne, ne_eq, Decidable.not_not, Bool.not_eq_true] at hMode
    simp only [beq_iff_eq, Bool.not_eq_true] at hCancel
    simp only [Bool.not_eq_true', Bool.not_eq_false', Bool.not_eq_true] at hControl hRead
    simp only [Bool.and_eq_true, not_and, Bool.not_eq_true] at hFence
    refine ⟨?_, ?_, ?_, by simp [hChild], ?_⟩
    all_goals simp_all
  · simp at h

/-- Every admitted steer is supervised: its controlling parent can cancel the
edge, and that cancellation reaches the child session the steer appended to. -/
theorem steer_is_supervised
    (viewer : Viewer) (edge : Edge) (evidence : SteerEvidence)
    (h : steerAdmission viewer edge evidence = .append) :
    controllable viewer edge = true ∧ readable viewer edge = true := by
  have hControl := (append_requires_direct_control viewer edge evidence h).1
  refine ⟨hControl, ?_⟩
  simp only [controllable, Bool.and_eq_true] at hControl
  exact hControl.1.1

theorem steer_never_grants_ancestor_control
    (viewer : Viewer) (edge : Edge) (evidence : SteerEvidence)
    (h : edge.directFromRoot = false) :
    steerAdmission viewer edge evidence = .notAuthorized := by
  simp [steerAdmission, h]

theorem cancelled_edge_stays_cancelled
    (viewer : Viewer) (edge : Edge) (evidence : SteerEvidence)
    (h : edge.lifecycle = .cancelled) :
    steerAdmission viewer edge evidence ≠ .append := by
  intro hc
  exact (append_requires_direct_control viewer edge evidence hc).2.2.1 h

/-- A late child of an unclaimed-spawn fence, whose bridge failed as
`spawnUnclaimed` or recorded a cancel intent, is never given new work. -/
theorem fenced_settled_spawn_is_never_steered
    (viewer : Viewer) (edge : Edge) (evidence : SteerEvidence)
    (hTerminal : terminal edge.lifecycle = true) (hFenced : fenced evidence = true) :
    steerAdmission viewer edge evidence ≠ .append := by
  intro hc
  have := (append_requires_direct_control viewer edge evidence hc).2.2.2.2 hTerminal
  simp [hFenced] at this

theorem absent_child_row_never_appends
    (viewer : Viewer) (edge : Edge) (evidence : SteerEvidence)
    (h : evidence.child = none) :
    steerAdmission viewer edge evidence ≠ .append := by
  intro hc
  have := (append_requires_direct_control viewer edge evidence hc).2.2.2.1
  simp [h] at this

theorem unmaterialized_edge_never_appends
    (viewer : Viewer) (edge : Edge) (evidence : SteerEvidence)
    (h : edge.materialization = .pending) :
    steerAdmission viewer edge evidence ≠ .append := by
  intro hc
  have hRead := (steer_is_supervised viewer edge evidence hc).2
  simp [readable, h] at hRead

/-- A finished child whose own attempt ended (completed, failed or timed out)
without a fence continues in its session. -/
theorem finished_child_continues
    (viewer : Viewer) (edge : Edge) (evidence : SteerEvidence) (state : RequestState)
    (hVisible : visible viewer edge = true)
    (hDirect : edge.directFromRoot = true)
    (hMode : edge.awaitMode = .background)
    (hFinished : edge.lifecycle = .completed ∨ edge.lifecycle = .failed ∨
      edge.lifecycle = .timedOut)
    (hControl : controllable viewer edge = true)
    (hChild : evidence.child = some state)
    (hOpen : fenced evidence = false) :
    steerAdmission viewer edge evidence = .append := by
  have hRead : readable viewer edge = true := by
    simp only [controllable, Bool.and_eq_true] at hControl
    exact hControl.1.1
  rcases hFinished with h | h | h <;>
    simp [steerAdmission, hVisible, hDirect, hMode, h, hRead, hControl, hChild, hOpen]

theorem cancel_leaves_no_live_request (session : List RequestState) :
    (cancelChildSession session).all childRequestTerminal = true := by
  induction session with
  | nil => rfl
  | cons state rest ih =>
    simp only [cancelChildSession, List.map_cons, List.all_cons] at ih ⊢
    rw [ih]
    cases state <;> rfl

theorem cancel_keeps_settled_requests (session : List RequestState) (state : RequestState)
    (hMem : state ∈ session) (hSettled : childRequestTerminal state = true) :
    state ∈ cancelChildSession session := by
  simp only [cancelChildSession, List.mem_map]
  exact ⟨state, hMem, by simp [hSettled]⟩

def inScope (scope : Scope) (edge : Edge) : Bool :=
  match scope with
  | .direct => edge.directFromRoot
  | .descendants => true

/-- A page cursor is derived only from durable edge identity, never from the
    edge's mutable lifecycle/materialization projection. -/
def cursor (edge : Edge) : Cursor :=
  (edge.parentToolCallId, edge.childRequestId)

/-- Cursor lookup runs over the stable scoped edge sequence. Volatile filters
    such as `includeTerminal` are applied only to the returned suffix. -/
def afterCursor (target : Cursor) : List Edge → Option (List Edge)
  | [] => none
  | edge :: rest =>
      if cursor edge == target then some rest else afterCursor target rest

theorem behavior_change_preserves_visibility
    (viewer : Viewer) (edge : Edge) (behavior : BehaviorId) :
    visible viewer { edge with behaviorId := behavior } = visible viewer edge := by
  rfl

theorem await_change_preserves_visibility
    (viewer : Viewer) (edge : Edge) (mode : AwaitMode) :
    visible viewer { edge with awaitMode := mode } = visible viewer edge := by
  rfl

theorem lifecycle_change_preserves_cursor
    (edge : Edge) (lifecycle : Lifecycle) :
    cursor { edge with lifecycle := lifecycle } = cursor edge := by
  rfl

theorem terminal_transition_preserves_cursor_anchor
    (edge next : Edge) :
    afterCursor (cursor edge) [{ edge with lifecycle := .completed }, next] =
      some [next] := by
  simp [afterCursor, lifecycle_change_preserves_cursor]

/-- One page of the stable scoped edge sequence. `staleCursor` reports that the
    caller's anchor no longer names an edge in this scope. -/
structure Page where
  edges : List Edge
  staleCursor : Bool
  deriving DecidableEq, Repr

/-- A caller-held cursor is an observation of the graph, not state of the
    request that holds it (#1808). An anchor that no longer resolves degrades
    the lineage view: the page restarts at the head of the stable scoped
    sequence and reports the stale anchor. It is never an error, so it cannot
    fail the caller's control tool or terminate the caller's stream. -/
def page (after : Option Cursor) (edges : List Edge) : Page :=
  match after with
  | none => ⟨edges, false⟩
  | some target =>
      match afterCursor target edges with
      | some rest => ⟨rest, false⟩
      | none => ⟨edges, true⟩

/-- Failures a descendant control tool can meet while the parent stream is
    live. Only the descendant observation may degrade (#1808). Canonical parent
    publication and tool dispatch receipt errors (#1697) are the parent's own
    durable facts and still fail closed. -/
inductive ControlFault where
  | staleDescendantCursor
  | parentPublication
  | dispatchReceipt
  deriving DecidableEq, Repr

inductive ParentDisposition where
  | continueParent
  | terminateParent
  deriving DecidableEq, Repr

def controlFaultDisposition : ControlFault → ParentDisposition
  | .staleDescendantCursor => .continueParent
  | .parentPublication | .dispatchReceipt => .terminateParent

theorem only_stale_descendant_cursor_continues_parent (fault : ControlFault) :
    controlFaultDisposition fault = .continueParent ↔
      fault = .staleDescendantCursor := by
  cases fault <;> simp [controlFaultDisposition]

theorem page_without_anchor_is_whole_scope (edges : List Edge) :
    page none edges = ⟨edges, false⟩ := rfl

theorem stale_anchor_restarts_scope (target : Cursor) (edges : List Edge)
    (h : afterCursor target edges = none) :
    page (some target) edges = ⟨edges, true⟩ := by
  simp [page, h]

theorem resolved_anchor_is_not_stale (target : Cursor) (edges rest : List Edge)
    (h : afterCursor target edges = some rest) :
    page (some target) edges = ⟨rest, false⟩ := by
  simp [page, h]

theorem anchor_of_listed_edge_resolves (edge : Edge) (before after : List Edge) :
    (afterCursor (cursor edge) (before ++ edge :: after)).isSome = true := by
  induction before with
  | nil => simp [afterCursor]
  | cons head tail ih =>
      simp only [List.cons_append, afterCursor]
      split <;> simp_all

/-- Settling a bridge (an unclaimed-spawn failure, a delivered notification)
    rewrites only its lifecycle projection, so an anchor issued while the edge
    was running is never reported stale afterwards. -/
theorem settled_anchor_is_not_stale (edge : Edge) (lifecycle : Lifecycle)
    (before after : List Edge) :
    (page (some (cursor edge))
      (before ++ { edge with lifecycle := lifecycle } :: after)).staleCursor = false := by
  have hfound := anchor_of_listed_edge_resolves { edge with lifecycle := lifecycle } before after
  rw [lifecycle_change_preserves_cursor] at hfound
  cases h : afterCursor (cursor edge)
      (before ++ { edge with lifecycle := lifecycle } :: after) with
  | none => simp [h] at hfound
  | some rest => simp [page, h]

theorem pending_bridge_visible_without_child
    (viewer : Viewer) (edge : Edge)
    (hBridge : edge.bridgeDurable = true)
    (hRoot : edge.rootRequestId = viewer.rootRequestId)
    (hOwner : edge.ownerPrincipal = viewer.rootPrincipal)
    (hSession : edge.rootSessionId = viewer.rootSessionId)
    (hLineage : edge.lineageId = viewer.lineageId) :
    visible viewer { edge with
      materialization := .pending
      physicalCorroborated := false } = true := by
  simp [visible, materializationAuthorized, hBridge, hRoot, hOwner, hSession, hLineage]

theorem unrelated_principal_cannot_see
    (viewer : Viewer) (edge : Edge)
    (h : edge.ownerPrincipal ≠ viewer.rootPrincipal) :
    visible viewer edge = false := by
  simp [visible, h]

theorem unrelated_principal_cannot_read
    (viewer : Viewer) (edge : Edge)
    (h : edge.ownerPrincipal ≠ viewer.rootPrincipal) :
    readable viewer edge = false := by
  simp [readable, unrelated_principal_cannot_see viewer edge h]

theorem unrelated_root_request_cannot_see
    (viewer : Viewer) (edge : Edge)
    (h : edge.rootRequestId ≠ viewer.rootRequestId) :
    visible viewer edge = false := by
  simp [visible, h]

theorem unrelated_session_cannot_see
    (viewer : Viewer) (edge : Edge)
    (h : edge.rootSessionId ≠ viewer.rootSessionId) :
    visible viewer edge = false := by
  simp [visible, h]

theorem unrelated_lineage_cannot_see
    (viewer : Viewer) (edge : Edge)
    (h : edge.lineageId ≠ viewer.lineageId) :
    visible viewer edge = false := by
  simp [visible, h]

theorem uncorroborated_materialized_edge_is_visible_but_unreadable
    (viewer : Viewer) (edge : Edge)
    (hBridge : edge.bridgeDurable = true)
    (hRoot : edge.rootRequestId = viewer.rootRequestId)
    (hOwner : edge.ownerPrincipal = viewer.rootPrincipal)
    (hSession : edge.rootSessionId = viewer.rootSessionId)
    (hLineage : edge.lineageId = viewer.lineageId)
    (h : edge.materialization ≠ .pending)
    (hPhysical : edge.physicalCorroborated = false) :
    visible viewer edge = true ∧ readable viewer edge = false := by
  have hMaterialization : materializationAuthorized edge = false := by
    generalize hKind : edge.materialization = kind at h ⊢
    cases kind <;> simp_all [materializationAuthorized, hPhysical]
  constructor
  · simp [visible, hBridge, hRoot, hOwner, hSession, hLineage]
  · simp [readable, hMaterialization]

theorem rejected_materialization_is_not_retryable
    (viewer : Viewer) (edge : Edge)
    (h : edge.materialization ≠ .pending) :
    retryable viewer edge = false := by
  simp [retryable, h]

theorem terminal_pending_bridge_is_not_retryable
    (viewer : Viewer) (edge : Edge)
    (hTerminal : terminal edge.lifecycle = true) :
    retryable viewer { edge with materialization := .pending } = false := by
  simp [retryable, hTerminal]

theorem owned_running_rejection_remains_listed
    (viewer : Viewer) (edge : Edge)
    (hBridge : edge.bridgeDurable = true)
    (hRoot : edge.rootRequestId = viewer.rootRequestId)
    (hOwner : edge.ownerPrincipal = viewer.rootPrincipal)
    (hSession : edge.rootSessionId = viewer.rootSessionId)
    (hLineage : edge.lineageId = viewer.lineageId)
    (hRunning : edge.lifecycle = .running) :
    listedByDefault viewer edge = true := by
  simp [listedByDefault, terminal, visible, hBridge, hRoot, hOwner, hSession, hLineage,
    hRunning]

theorem replicated_materialization_preserves_authorization
    (viewer : Viewer) (edge : Edge) :
    visible viewer { edge with materialization := .replicated } =
      visible viewer { edge with materialization := .local } := by
  rfl

theorem visibility_does_not_grant_ancestor_control
    (viewer : Viewer) (edge : Edge)
    (_hVisible : visible viewer edge = true)
    (hNested : edge.directFromRoot = false) :
    controllable viewer edge = false := by
  simp [controllable, hNested]

theorem control_implies_visibility
    (viewer : Viewer) (edge : Edge)
    (hControl : controllable viewer edge = true) :
    visible viewer edge = true := by
  simp [controllable] at hControl
  have hReadable := hControl.1
  unfold readable at hReadable
  simp at hReadable
  exact hReadable.1.1.1

theorem direct_scope_excludes_nested
    (edge : Edge) (hNested : edge.directFromRoot = false) :
    inScope .direct edge = false := by
  simp [inScope, hNested]

theorem descendants_scope_includes_every_edge (edge : Edge) :
    inScope .descendants edge = true := by
  rfl

end DescendantGraph
