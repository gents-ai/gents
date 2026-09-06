import Proofs.GraphPipeline.LogicalInvocation
import Proofs.Workspace.Types

/-!
Derived workspace lineage for one pinned graph entry, not a new document owner.
Authentication/route/physical-source facts are checked adapter observations.
No cryptography, scan phantom protection or filesystem sealHash computation is proved.
Ready quickstart creation is NOT sealing; a sealHash can only be supplied by the
existing workspace owner. An absent sealHash remains absent under inheritance.
-/
namespace GraphPipeline.WorkspaceLineage

structure Identity where
  workspaceId : Nat
  owner : Nat
  sealHash : Option Nat
  deriving DecidableEq, Repr

structure Explicit where
  workspaceId : Option Nat := none
  owner : Option Nat := none
  sealHash : Option Nat := none
  authority : Option BindingAuthority := none
  deriving DecidableEq, Repr

structure Root where
  docId : Nat
  correlation : Nat
  revision : Nat
  entryRoute : Nat
  authenticatedTarget : Bool
  wellFormedTuple : Bool
  workspace : Option Identity
  deriving DecidableEq, Repr

structure Context where
  correlation : Nat
  revision : Nat
  entryRoute : Nat
  runAndPlanVerified : Bool
  destinationRouteVerified : Bool
  destinationAuthority : Option BindingAuthority
  deriving DecidableEq, Repr

inductive Source where
  | downstream (roots : List Root)
  /-- Controller's immutable input constraints and workspace-owner-stamped tuple.
  physicalSeedVerified includes selected entry route, source collection/docID and
  unique observed correlation; this does NOT assert a native predicate lock. -/
  | bootstrap (controllerInput : Explicit) (stamped : Option Identity)
      (physicalSeedVerified : Bool) (workspaceOwnerVerified : Bool)
  deriving DecidableEq, Repr

structure Resolved where
  workspace : Option Identity
  authority : Option BindingAuthority
  deriving DecidableEq, Repr

def optionalMatches {α : Type} [DecidableEq α] (provided : Option α) (expected : α) : Bool :=
  provided.all (fun value => value == expected)

def identityMatches (explicit : Explicit) : Option Identity → Bool
  | none => explicit.workspaceId.isNone && explicit.owner.isNone && explicit.sealHash.isNone
  | some expected => optionalMatches explicit.workspaceId expected.workspaceId &&
      optionalMatches explicit.owner expected.owner &&
      explicit.sealHash.all (fun sealHash => expected.sealHash == some sealHash)

def explicitMatches (context : Context) (explicit : Explicit) (identity : Option Identity) : Bool :=
  identityMatches explicit identity &&
    explicit.authority.all (fun authority => context.destinationAuthority == some authority)

/-- Bootstrap may add an owner-verified sealHash when omitted by the CLI, but
cannot introduce a workspace ID/owner absent from the controller's stored input. -/
def controllerMatches (context : Context) (input : Explicit) (stamped : Option Identity) : Bool :=
  input.workspaceId == stamped.map Identity.workspaceId &&
  input.owner == stamped.map Identity.owner && explicitMatches context input stamped

def rootMatches (context : Context) (root : Root) : Bool :=
  root.authenticatedTarget && root.correlation == context.correlation &&
  root.revision == context.revision && root.entryRoute == context.entryRoute

/-- Unauthenticated and unrelated hints grant nothing. Exactly one qualifying
physical entry receipt must remain; authenticated ambiguity is never sorted away. -/
def sourceIdentity (context : Context) : Source → Option (Option Identity)
  | .downstream roots =>
      match roots.filter (rootMatches context) with
      | [root] => if root.wellFormedTuple then some root.workspace else none
      | _ => none
  | .bootstrap input stamped seedVerified ownerVerified =>
      if seedVerified && ownerVerified && controllerMatches context input stamped
      then some stamped else none

/-- A destination with no workspace ceiling (including code-review triage)
receives no binding. Validate provenance/conflicting hints before attenuation. -/
def destinationWorkspace (context : Context) (identity : Option Identity) : Option Identity :=
  if context.destinationAuthority.isSome then identity else none

def resolve (context : Context) (source : Source) (explicit : Explicit) : Option Resolved :=
  if context.runAndPlanVerified && context.destinationRouteVerified then
    match sourceIdentity context source with
    | none => none
    | some identity =>
      if explicitMatches context explicit identity then
        some ⟨destinationWorkspace context identity, context.destinationAuthority⟩
      else none
  else none

theorem destination_authority_pinned (context : Context) (source : Source)
    (explicit : Explicit) (result : Resolved)
    (h : resolve context source explicit = some result) :
    result.authority = context.destinationAuthority := by
  unfold resolve at h
  split at h
  · cases hs : sourceIdentity context source <;> simp [hs] at h
    rcases h with ⟨_, rfl⟩
    rfl
  · contradiction

theorem accepted_source_projection (context : Context) (source : Source)
    (explicit : Explicit) (result : Resolved)
    (h : resolve context source explicit = some result) :
    ∃ identity, sourceIdentity context source = some identity ∧
      result.workspace = destinationWorkspace context identity ∧
      explicitMatches context explicit identity = true := by
  unfold resolve at h
  split at h
  · cases hs : sourceIdentity context source <;> simp [hs] at h
    rcases h with ⟨guard, rfl⟩
    exact ⟨_, rfl, rfl, guard⟩
  · contradiction

theorem accepted_source_exact (context : Context) (source : Source)
    (explicit : Explicit) (result : Resolved)
    (ha : context.destinationAuthority.isSome = true)
    (h : resolve context source explicit = some result) :
    sourceIdentity context source = some result.workspace := by
  obtain ⟨identity, hs, hw, _⟩ := accepted_source_projection context source explicit result h
  simpa [hw, destinationWorkspace, ha] using hs

theorem no_authority_omits_workspace (context : Context) (source : Source)
    (explicit : Explicit) (result : Resolved)
    (ha : context.destinationAuthority = none)
    (h : resolve context source explicit = some result) :
    result.workspace = none ∧ result.authority = none := by
  obtain ⟨identity, _, hw, _⟩ := accepted_source_projection context source explicit result h
  exact ⟨by simpa [destinationWorkspace, ha] using hw,
    (destination_authority_pinned context source explicit result h).trans ha⟩

theorem unique_authenticated_root_inherited (context : Context) (roots : List Root)
    (root : Root) (explicit : Explicit) (result : Resolved)
    (ha : context.destinationAuthority.isSome = true)
    (hroot : roots.filter (rootMatches context) = [root])
    (h : resolve context (.downstream roots) explicit = some result) :
    result.workspace = root.workspace := by
  have hs := accepted_source_exact context (.downstream roots) explicit result ha h
  simp only [sourceIdentity, hroot] at hs
  split at hs
  · simpa using hs.symm
  · contradiction

theorem missing_root_denied (context : Context) (roots : List Root) (explicit : Explicit)
    (hroot : roots.filter (rootMatches context) = []) :
    resolve context (.downstream roots) explicit = none := by
  simp [resolve, sourceIdentity, hroot]

theorem ambiguous_roots_denied (context : Context) (roots : List Root) (a b : Root)
    (rest : List Root) (explicit : Explicit)
    (hroot : roots.filter (rootMatches context) = a :: b :: rest) :
    resolve context (.downstream roots) explicit = none := by
  simp [resolve, sourceIdentity, hroot]

theorem explicit_conflict_denied (context : Context) (source : Source)
    (explicit : Explicit) (identity : Option Identity)
    (hs : sourceIdentity context source = some identity)
    (hc : explicitMatches context explicit identity = false) :
    resolve context source explicit = none := by
  simp [resolve, hs, hc]

theorem bootstrap_input_bound (context : Context) (input explicit : Explicit)
    (stamped : Option Identity) (seed owner : Bool) (result : Resolved)
    (h : resolve context (.bootstrap input stamped seed owner) explicit = some result) :
    controllerMatches context input stamped = true ∧ seed = true ∧ owner = true := by
  obtain ⟨_, hs, _, _⟩ :=
    accepted_source_projection context (.bootstrap input stamped seed owner) explicit result h
  simp only [sourceIdentity] at hs
  split at hs
  · rename_i guard
    simp only [Bool.and_eq_true] at guard
    exact ⟨guard.2, guard.1.1, guard.1.2⟩
  · contradiction

theorem bootstrap_cannot_introduce_workspace (context : Context) (input explicit : Explicit)
    (stamped : Option Identity) (seed owner : Bool) (result : Resolved)
    (h : resolve context (.bootstrap input stamped seed owner) explicit = some result) :
    input.workspaceId = stamped.map Identity.workspaceId ∧
    input.owner = stamped.map Identity.owner := by
  have hm := (bootstrap_input_bound context input explicit stamped seed owner result h).1
  simp only [controllerMatches, Bool.and_eq_true, beq_iff_eq] at hm
  exact hm.1

theorem unauthenticated_singleton_denied (context : Context) (root : Root)
    (explicit : Explicit) (h : root.authenticatedTarget = false) :
    resolve context (.downstream [root]) explicit = none := by
  apply missing_root_denied
  simp [rootMatches, h]

/-- Same existing GraphRun publication state and generation fence. No second
transaction/status: this stages only when lineage and the current fence agree. -/
def publish (state : LogicalInvocation.PublicationState) (expected : Nat)
    (context : Context) (source : Source) (explicit : Explicit) : LogicalInvocation.PublicationState :=
  if (resolve context source explicit).isSome &&
      state.graph.run.runId == context.correlation &&
      state.graph.run.revisionDigest == context.revision
  then LogicalInvocation.publish state expected else state

theorem rejected_lineage_no_publication (state : LogicalInvocation.PublicationState)
    (expected : Nat) (context : Context) (source : Source) (explicit : Explicit)
    (h : resolve context source explicit = none) :
    publish state expected context source explicit = state := by simp [publish, h]

theorem stale_generation_no_publication (state : LogicalInvocation.PublicationState)
    (expected : Nat) (context : Context) (source : Source) (explicit : Explicit)
    (h : state.graph.generation ≠ expected) :
    publish state expected context source explicit = state := by
  simp [publish, LogicalInvocation.stale_publication_noop state expected h]

theorem cancellation_no_publication (state : LogicalInvocation.PublicationState)
    (expected : Nat) (context : Context) (source : Source) (explicit : Explicit)
    (h : state.graph.run.cancellationRequested = true) :
    publish state expected context source explicit = state := by
  simp [publish, LogicalInvocation.cancellation_blocks_publication state expected h]

theorem captured_cause_no_publication (state : LogicalInvocation.PublicationState)
    (expected : Nat) (context : Context) (source : Source) (explicit : Explicit)
    (h : state.graph.primary ≠ none) :
    publish state expected context source explicit = state := by
  simp [publish, LogicalInvocation.captured_failure_blocks_publication state expected h]

end GraphPipeline.WorkspaceLineage
