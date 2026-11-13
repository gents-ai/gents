import Proofs.Session.Workspace.Properties
import Proofs.Session.Request.State

/-! Trigger fires whose context names an isolated workspace. A fire resolves
the workspace under the owner it names, or under the executing node when it
names none; it never infers a foreign owner. A resolved fire publishes its
request staged in `workspaceBindingPending`, stamped with the workspace's
owner, and only an admitted binding activates it to `pending`. -/

namespace WorkspaceFire

open IsolatedWorkspace

/-- A node as seen by the executing node: itself, or another node. -/
inductive FireNode where
  | executor
  | foreign
  deriving DecidableEq, Repr

def FireNode.did : FireNode → String
  | .executor => "did:executor"
  | .foreign => "did:foreign"

structure Fire where
  namedOwner : Option FireNode
  authority : BindingAuthority
  deriving DecidableEq, Repr

inductive FireOutcome where
  | published
  | workspaceNotFound
  | bindingDenied
  deriving DecidableEq, Repr

structure FireRow where
  fire : Nat
  node : FireNode
  workspaceOwner : FireNode
  authority : BindingAuthority
  state : RequestState
  returned : Bool
  deriving DecidableEq, Repr

structure FireState where
  outcomes : List FireOutcome
  rows : List FireRow
  bindings : List WorkspaceBinding
  deriving DecidableEq

def workspace (owner : FireNode) : IsolatedWorkspace :=
  { workspaceId := "ws-1"
  , workUnitId := "unit-1"
  , repositoryId := "repo-1"
  , baseSha := "base"
  , branch := "topic"
  , creationPolicy := .gitWorktreeDiff
  , ownerNodeDid := owner.did
  , sealHash := none
  , state := .ready }

/-- The owner a fire looks the workspace up under. -/
def lookupOwner (fire : Fire) : FireNode :=
  fire.namedOwner.getD .executor

/-- The request is always published by the executing node. -/
def publisher : FireNode := .executor

def resolves (owner : FireNode) (fire : Fire) : Bool :=
  decide ((workspace owner).ownerNodeDid = (lookupOwner fire).did)

def candidate (owner : FireNode) (index : Nat) (fire : Fire) : WorkspaceBinding :=
  { bindingId := s!"binding-{index}"
  , workspaceId := (workspace owner).workspaceId
  , requestId := s!"request-{index}"
  , authority := fire.authority
  , nodeDid := (workspace owner).ownerNodeDid
  , sealHash := none
  , state := .active }

/-- Binding admission; earlier requests in a trace stay live, so no prior
exclusive binding is released. -/
def admits (owner : FireNode) (existing : List WorkspaceBinding)
    (binding : WorkspaceBinding) : Bool :=
  let w := workspace owner
  decide (OwnerClaimable binding.nodeDid w) &&
    decide (ReadWriteOk w binding) &&
    decide (ReadOnlyOk w binding) &&
    decide (IntegrateOk w binding) &&
    decide (UniqueActiveReadWrite w.workspaceId (binding :: existing)) &&
    decide (UniqueActiveIntegrate w.workspaceId (binding :: existing))

def step (owner : FireNode) (state : FireState) (indexed : Nat × Fire) : FireState :=
  let (index, fire) := indexed
  if resolves owner fire then
    let binding := candidate owner index fire
    let admitted := admits owner state.bindings binding
    let row : FireRow :=
      { fire := index
      , node := publisher
      , workspaceOwner := owner
      , authority := fire.authority
      , state := if admitted then .pending else .workspaceBindingPending
      , returned := admitted }
    { outcomes := state.outcomes ++ [if admitted then .published else .bindingDenied]
    , rows := state.rows ++ [row]
    , bindings := if admitted then state.bindings ++ [binding] else state.bindings }
  else
    { state with outcomes := state.outcomes ++ [.workspaceNotFound] }

def run (owner : FireNode) (fires : List Fire) : FireState :=
  (List.range fires.length |>.zip fires).foldl (step owner) ⟨[], [], []⟩

def claimable (state : FireState) : Nat :=
  (state.rows.filter fun row => decide (row.state = .pending)).length

theorem denied_fire_leaves_no_claimable_row (owner : FireNode) (state : FireState)
    (index : Nat) (fire : Fire)
    (h : (step owner state (index, fire)).outcomes = state.outcomes ++ [.bindingDenied]) :
    claimable (step owner state (index, fire)) = claimable state := by
  unfold step at h ⊢
  by_cases hr : resolves owner fire = true <;>
    by_cases ha : admits owner state.bindings (candidate owner index fire) = true <;>
    simp_all [claimable]

theorem unresolved_fire_publishes_nothing (owner : FireNode) (state : FireState)
    (index : Nat) (fire : Fire) (h : resolves owner fire = false) :
    (step owner state (index, fire)).rows = state.rows := by
  simp [step, h]

theorem materializer_rejects_workspace_from_different_explicit_owner :
    run .foreign [⟨some .executor, .readWrite⟩] =
      ⟨[.workspaceNotFound], [], []⟩ := by
  native_decide

theorem materializer_resolves_missing_workspace_owner_from_canonical_workspace :
    (run .executor [⟨none, .readWrite⟩]).rows =
      [⟨0, .executor, .executor, .readWrite, .pending, true⟩] := by
  native_decide

theorem materializer_does_not_infer_an_owner_for_a_foreign_workspace :
    run .foreign [⟨none, .readOnly⟩] = ⟨[.workspaceNotFound], [], []⟩ := by
  native_decide

theorem materializer_preserves_explicit_workspace_owner_distinct_from_executor :
    (run .foreign [⟨some .foreign, .readWrite⟩]).rows =
      [⟨0, .executor, .foreign, .readWrite, .pending, true⟩] := by
  native_decide

theorem unique_read_write_denial_does_not_leave_claimable_request :
    let state := run .foreign [⟨some .foreign, .readWrite⟩, ⟨some .foreign, .readWrite⟩]
    state.outcomes = [.published, .bindingDenied] ∧
      claimable state = 1 ∧
      state.rows.map FireRow.state = [.pending, .workspaceBindingPending] := by
  native_decide

end WorkspaceFire
