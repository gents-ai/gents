import Proofs.EventDelivery.Contract
import Proofs.EventDelivery.Properties
import Proofs.CanonicalOutput.Delegation

open EventDelivery

namespace EventDelivery.SubagentSource

structure ReservedChildBinding where
  child : Nat
  agent : Nat
  behavior : Nat
  parentRequest : Nat
  parentRequestDoc : Nat
  parentTool : Nat
  parentToolDoc : Nat
  payload : Nat
  depth : Nat
  workspace : Option Workspace.ChildStamp
  admission : Nat
  deriving DecidableEq, Repr

inductive MaterializationDecision where
  | created | replayed | conflict
  deriving DecidableEq, Repr

def ensureReservedChild (stored : List ReservedChildBinding)
    (candidate : ReservedChildBinding) : MaterializationDecision × List ReservedChildBinding :=
  match stored.filter (·.child == candidate.child) with
  | [] => (.created, stored ++ [candidate])
  | [existing] =>
      if existing == candidate then (.replayed, stored) else (.conflict, stored)
  | _ => (.conflict, stored)

/-- Physical identities are observations of the native SubagentSource owner.
The payload and child choice come from fallible SpawnArgs parsing followed by
the native host's workspace/placement/principal observations. The model does
not implement JSON parsing, filesystem access, or peer authentication. -/
structure HostChildFacts where
  child : Nat
  parentRequest : Nat
  parentRequestDoc : Nat
  parentTool : Nat
  admission : Nat
  deriving DecidableEq, Repr

/-- Receive the addressed accepted bridge and derive its complete reservation
from the actual decoded arguments and host observation. -/
def receivedChildCandidate?
    (authenticatedCoordinator host configuredBehavior : Nat)
    (row : CanonicalOutput.DelegatedCall) (facts : HostChildFacts)
    (observeInput : String → Option (Nat × Workspace.ChildChoice)) :
    Option ReservedChildBinding := do
  let (payload, choice) ← observeInput row.input.arguments
  let (depth, workspace) ← CanonicalOutput.receiveDelegatedChild
    authenticatedCoordinator host configuredBehavior row.coordinator host row choice
  pure
    { child := facts.child
    , agent := host
    , behavior := configuredBehavior
    , parentRequest := facts.parentRequest
    , parentRequestDoc := facts.parentRequestDoc
    , parentTool := facts.parentTool
    , parentToolDoc := row.call
    , payload := payload
    , depth := depth
    , workspace := workspace
    , admission := facts.admission }

theorem received_candidate_has_resolution
    (coordinator host behavior : Nat) (row : CanonicalOutput.DelegatedCall)
    (facts : HostChildFacts)
    (observeInput : String → Option (Nat × Workspace.ChildChoice))
    (candidate : ReservedChildBinding)
    (h : receivedChildCandidate? coordinator host behavior row facts observeInput =
      some candidate) :
    ∃ payload choice,
      observeInput row.input.arguments = some (payload, choice) ∧
      CanonicalOutput.receiveDelegatedChild coordinator host behavior
        row.coordinator host row choice = some (candidate.depth, candidate.workspace) ∧
      candidate.parentToolDoc = row.call ∧ candidate.payload = payload := by
  unfold receivedChildCandidate? at h
  cases hp : observeInput row.input.arguments with
  | none => simp [hp] at h
  | some observed =>
      rcases observed with ⟨payload, choice⟩
      cases hr : CanonicalOutput.receiveDelegatedChild coordinator host behavior
          row.coordinator host row choice with
      | none => simp [hp, hr] at h
      | some resolved =>
          rcases resolved with ⟨depth, workspace⟩
          simp [hp, hr] at h
          cases h
          exact ⟨payload, choice, rfl, by simpa using hr, rfl, rfl⟩

theorem received_candidate_depth_bounded
    (coordinator host behavior : Nat) (row : CanonicalOutput.DelegatedCall)
    (facts : HostChildFacts)
    (observeInput : String → Option (Nat × Workspace.ChildChoice))
    (candidate : ReservedChildBinding)
    (h : receivedChildCandidate? coordinator host behavior row facts observeInput =
      some candidate) :
    candidate.depth ≤ Subagent.maxSubagentDepth := by
  obtain ⟨_, choice, _, hr, _, _⟩ :=
    received_candidate_has_resolution coordinator host behavior row facts observeInput candidate h
  exact CanonicalOutput.received_child_depth_bounded coordinator host behavior
    row.coordinator host row choice candidate.depth candidate.workspace hr

theorem received_candidate_authority_bounded
    (coordinator host behavior : Nat) (row : CanonicalOutput.DelegatedCall)
    (facts : HostChildFacts)
    (observeInput : String → Option (Nat × Workspace.ChildChoice))
    (candidate : ReservedChildBinding)
    (parent child : Workspace.ChildStamp)
    (hp : row.workspace = some parent)
    (hc : candidate.workspace = some child)
    (h : receivedChildCandidate? coordinator host behavior row facts observeInput =
      some candidate) :
    Workspace.authorityRank child.authority ≤
      Workspace.authorityRank parent.authority := by
  obtain ⟨_, choice, _, hr, _, _⟩ :=
    received_candidate_has_resolution coordinator host behavior row facts observeInput candidate h
  rw [hc] at hr
  exact (CanonicalOutput.received_child_depth_and_authority_bounded coordinator host behavior
    row.coordinator host row choice parent child candidate.depth hp hr).2

theorem received_candidate_readonly_parent
    (coordinator host behavior : Nat) (row : CanonicalOutput.DelegatedCall)
    (facts : HostChildFacts)
    (observeInput : String → Option (Nat × Workspace.ChildChoice))
    (candidate : ReservedChildBinding)
    (parent child : Workspace.ChildStamp)
    (hp : row.workspace = some parent) (hreadonly : parent.authority = .readOnly)
    (hc : candidate.workspace = some child)
    (h : receivedChildCandidate? coordinator host behavior row facts observeInput =
      some candidate) : child.authority = .readOnly := by
  obtain ⟨_, choice, _, hr, _, _⟩ :=
    received_candidate_has_resolution coordinator host behavior row facts observeInput candidate h
  rw [hc] at hr
  exact CanonicalOutput.received_readonly_parent_cannot_escalate coordinator host behavior
    row.coordinator host row choice parent child candidate.depth hp hreadonly hr

/-- Workspace observations are resolved at creation, not revalidated at claim.
Rejected input never changes the durable reservation list. -/
def receiveAndReserveChild (stored : List ReservedChildBinding)
    (authenticatedCoordinator host configuredBehavior : Nat)
    (row : CanonicalOutput.DelegatedCall) (facts : HostChildFacts)
    (observeInput : String → Option (Nat × Workspace.ChildChoice)) :
    Option MaterializationDecision × List ReservedChildBinding :=
  match receivedChildCandidate? authenticatedCoordinator host configuredBehavior
      row facts observeInput with
  | none => (none, stored)
  | some candidate =>
      let result := ensureReservedChild stored candidate
      (some result.1, result.2)

theorem receive_rejection_preserves_stored (stored : List ReservedChildBinding)
    (coordinator host behavior : Nat) (row : CanonicalOutput.DelegatedCall)
    (facts : HostChildFacts) (observeInput : String → Option (Nat × Workspace.ChildChoice))
    (h : (receiveAndReserveChild stored coordinator host behavior row facts observeInput).1 = none) :
    (receiveAndReserveChild stored coordinator host behavior row facts observeInput).2 = stored := by
  cases hc : receivedChildCandidate? coordinator host behavior row facts observeInput with
  | none => simp [receiveAndReserveChild, hc]
  | some candidate =>
      simp [receiveAndReserveChild, hc] at h

theorem fresh_then_exact_replay_creates_once (stored : List ReservedChildBinding)
    (candidate : ReservedChildBinding)
    (habsent : stored.all (·.child != candidate.child) = true) :
    let first := ensureReservedChild stored candidate
    first.1 = .created ∧
      ensureReservedChild first.2 candidate = (.replayed, first.2) := by
  have hfilter : stored.filter (·.child == candidate.child) = [] := by
    induction stored with
    | nil => simp
    | cons value rest ih =>
        simp only [List.all_cons, Bool.and_eq_true] at habsent
        have hne : value.child ≠ candidate.child := by
          simpa using habsent.1
        simp [hne, ih habsent.2]
  simp [ensureReservedChild, hfilter]

theorem conflict_preserves_stored (stored : List ReservedChildBinding)
    (candidate : ReservedChildBinding)
    (hconflict : (ensureReservedChild stored candidate).1 = .conflict) :
    (ensureReservedChild stored candidate).2 = stored := by
  simp only [ensureReservedChild] at hconflict ⊢
  split <;> simp_all
  split <;> simp_all

theorem replay_preserves_stored (stored : List ReservedChildBinding)
    (candidate : ReservedChildBinding)
    (hreplay : (ensureReservedChild stored candidate).1 = .replayed) :
    (ensureReservedChild stored candidate).2 = stored := by
  simp only [ensureReservedChild] at hreplay ⊢
  split <;> simp_all
  split <;> simp_all

theorem received_replay_or_conflict_preserves_stored
    (stored : List ReservedChildBinding)
    (coordinator host behavior : Nat) (row : CanonicalOutput.DelegatedCall)
    (facts : HostChildFacts)
    (observeInput : String → Option (Nat × Workspace.ChildChoice))
    (decision : MaterializationDecision)
    (h : (receiveAndReserveChild stored coordinator host behavior row facts observeInput).1 =
      some decision)
    (hd : decision = .replayed ∨ decision = .conflict) :
    (receiveAndReserveChild stored coordinator host behavior row facts observeInput).2 =
      stored := by
  cases hc : receivedChildCandidate? coordinator host behavior row facts observeInput with
  | none => simp [receiveAndReserveChild, hc] at h
  | some candidate =>
      simp only [receiveAndReserveChild, hc] at h ⊢
      cases hd with
      | inl replay =>
          subst decision
          exact replay_preserves_stored stored candidate (by simpa using h)
      | inr conflict =>
          subst decision
          exact conflict_preserves_stored stored candidate (by simpa using h)

structure ReservedChildCase where
  name : String
  stored : List ReservedChildBinding
  candidate : ReservedChildBinding
  expectedDecision : MaterializationDecision
  expectedCount : Nat
  deriving Repr

def baseBinding : ReservedChildBinding :=
  ⟨42, 1, 2, 3, 30, 4, 40, 5, 1,
    some ⟨6, 1, some 60, .readOnly⟩, 7⟩

def alternateWorkspace : Workspace.ChildStamp :=
  ⟨9, 1, some 60, .readOnly⟩

def escalatedWorkspace : Workspace.ChildStamp :=
  ⟨6, 1, some 60, .readWrite⟩

def reservedChildCases : List ReservedChildCase :=
  [ ⟨"fresh_reserved_child", [], baseBinding, .created, 1⟩
  , ⟨"exact_reserved_child_replay", [baseBinding], baseBinding, .replayed, 1⟩
  , ⟨"conflicting_physical_lineage",
      [baseBinding], { baseBinding with parentToolDoc := 41 }, .conflict, 1⟩
  , ⟨"conflicting_payload",
      [baseBinding], { baseBinding with payload := 8 }, .conflict, 1⟩
  , ⟨"conflicting_workspace",
      [baseBinding], { baseBinding with workspace := some alternateWorkspace }, .conflict, 1⟩
  , ⟨"conflicting_workspace_authority",
      [baseBinding], { baseBinding with workspace := some escalatedWorkspace }, .conflict, 1⟩
  , ⟨"conflicting_depth",
      [baseBinding], { baseBinding with depth := 3 }, .conflict, 1⟩
  , ⟨"conflicting_admission",
      [baseBinding], { baseBinding with admission := 10 }, .conflict, 1⟩
  , ⟨"physical_twins_fail_closed", [baseBinding, baseBinding], baseBinding, .conflict, 2⟩
  ]

theorem reservedChildCases_derive : ∀ c ∈ reservedChildCases,
    (ensureReservedChild c.stored c.candidate).1 = c.expectedDecision ∧
    (ensureReservedChild c.stored c.candidate).2.length = c.expectedCount := by
  decide

def subagentSourceSrc : SourceInstance :=
  { name := "SubagentSource"
  , dedupePolicy := .monotoneOnce
  , rescanBoundedBy := 1
  }

theorem subagentSourceSrc_rescanBoundedBy_pos :
    0 < subagentSourceSrc.rescanBoundedBy := by
  decide

theorem O1_orphan_child_materialization
    (w₀ : World) (d : DocId)
    (h_persisted : d ∈ w₀.persistentSet)
    (h_unprocessed : d ∉ w₀.processedSet) :
    ∃ (actions : List Action) (w' : World),
      TraceOf w₀ actions w' ∧
      Fair subagentSourceSrc actions ∧
      d ∈ w'.handled :=
  D1_delivery_convergence
    subagentSourceSrc w₀ d h_persisted h_unprocessed
    subagentSourceSrc_rescanBoundedBy_pos

end EventDelivery.SubagentSource
