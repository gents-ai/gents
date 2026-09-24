import Proofs.Workspace.Types

/-! Authority and identity projection of the native subagent workspace resolver.
Database existence, placement, path and principal checks are observations of
the existing workspace owner, not permissions granted by this model. -/

namespace Workspace

structure ChildStamp where
  workspaceId : Nat
  ownerAgent : Nat
  sealHash : Option Nat
  authority : BindingAuthority
  deriving DecidableEq, Repr

structure ObservedWorkspace where
  workspaceId : Nat
  ownerAgent : Nat
  sealHash : Option Nat
  state : WorkspaceState
  /-- Native load, placement and selected-principal checks have succeeded. -/
  available : Bool
  deriving DecidableEq, Repr

inductive ChildChoice where
  | noWorkspace
  | inherit (workspace : ObservedWorkspace)
  | bind (workspace : ObservedWorkspace) (requestedAuthority : Option BindingAuthority)
  | provision (workspace : ObservedWorkspace) (parentPathExact : Bool)
  deriving DecidableEq, Repr

def authorityRank : BindingAuthority → Nat
  | .readOnly => 0
  | .integrate => 1
  | .readWrite => 2

def authorityInfimum (left right : BindingAuthority) : BindingAuthority :=
  if authorityRank left ≤ authorityRank right then left else right

def stamp (workspace : ObservedWorkspace) (authority : BindingAuthority) : ChildStamp :=
  ⟨workspace.workspaceId, workspace.ownerAgent, workspace.sealHash, authority⟩

def sourceAgrees (source : ChildStamp) (workspace : ObservedWorkspace) : Bool :=
  source.workspaceId == workspace.workspaceId &&
    source.ownerAgent == workspace.ownerAgent &&
    source.sealHash == workspace.sealHash

def defaultAuthorityForState : WorkspaceState → Option BindingAuthority
  | .ready => some .readWrite
  | .sealed => some .readOnly
  | _ => none

def bindableAuthority (state : WorkspaceState) (authority : BindingAuthority) : Bool :=
  match state, authority with
  | .ready, .readOnly | .ready, .readWrite => true
  | .sealed, .readOnly | .sealed, .integrate => true
  | _, _ => false

def stamped (workspace : ObservedWorkspace) (authority : BindingAuthority) :
    Option ChildStamp :=
  if bindableAuthority workspace.state authority &&
      (workspace.state != .sealed || workspace.sealHash.isSome) then
    some (stamp workspace authority)
  else none

theorem stamped_authority (workspace : ObservedWorkspace)
    (authority : BindingAuthority) (child : ChildStamp)
    (h : stamped workspace authority = some child) :
    child.authority = authority := by
  unfold stamped at h
  split at h
  · cases h; rfl
  · contradiction

/-- The result is the lineage passed to child request creation. The caller must
derive `ObservedWorkspace` from the existing native workspace/placement/principal
owners; this function does not implement those owners or perform host I/O. -/
def resolveChild (parent : Option ChildStamp) (parentAgent childAgent : Nat)
    (choice : ChildChoice) : Option (Option ChildStamp) :=
  match choice with
  | .noWorkspace => if parent.isNone then some none else none
  | .inherit workspace =>
      match parent with
      | some source =>
          if workspace.available && sourceAgrees source workspace then
            (defaultAuthorityForState workspace.state).bind fun defaultAuthority =>
              (stamped workspace (authorityInfimum source.authority defaultAuthority)).map some
          else none
      | none => none
  | .bind workspace requestedAuthority =>
      if !workspace.available ||
          workspace.ownerAgent != (parent.map (·.ownerAgent)).getD parentAgent then none
      else
        (defaultAuthorityForState workspace.state).bind fun defaultAuthority =>
          let requested := requestedAuthority.getD defaultAuthority
          let authority := (parent.map (fun source =>
            authorityInfimum source.authority requested)).getD requested
          (stamped workspace authority).map some
  | .provision workspace parentPathExact =>
      match parent with
      | some source =>
          if workspace.available && parentPathExact &&
              workspace.ownerAgent == childAgent then
            if workspace.state == .ready then
              (stamped workspace (authorityInfimum source.authority .readWrite)).map some
            else none
          else none
      | none => none

theorem authority_infimum_never_exceeds_left (left right : BindingAuthority) :
    authorityRank (authorityInfimum left right) ≤ authorityRank left := by
  unfold authorityInfimum
  split
  · simp
  · omega

theorem resolved_child_cannot_exceed_parent (parent child : ChildStamp)
    (parentAgent childAgent : Nat) (choice : ChildChoice)
    (h : resolveChild (some parent) parentAgent childAgent choice = some (some child)) :
    authorityRank child.authority ≤ authorityRank parent.authority := by
  cases choice with
  | noWorkspace => simp [resolveChild] at h
  | inherit workspace =>
      simp only [resolveChild] at h
      split at h <;> try contradiction
      cases hd : defaultAuthorityForState workspace.state with
      | none => simp [hd] at h
      | some default =>
          simp only [hd, Option.bind_some] at h
          cases hs : stamped workspace (authorityInfimum parent.authority default) with
          | none => simp [hs] at h
          | some actual =>
              simp_all
              rw [stamped_authority workspace _ child hs]
              exact authority_infimum_never_exceeds_left ..
  | bind workspace requested =>
      simp only [resolveChild] at h
      split at h <;> try contradiction
      cases hd : defaultAuthorityForState workspace.state with
      | none => simp [hd] at h
      | some default =>
          simp only [hd, Option.bind_some, Option.map_some, Option.getD_some] at h
          cases hs : stamped workspace (authorityInfimum parent.authority
              (requested.getD default)) with
          | none => simp [hs] at h
          | some actual =>
              simp_all
              rw [stamped_authority workspace _ child hs]
              exact authority_infimum_never_exceeds_left ..
  | provision workspace parentPathExact =>
      simp only [resolveChild] at h
      split at h <;> try contradiction
      split at h <;> try contradiction
      cases hs : stamped workspace (authorityInfimum parent.authority .readWrite) with
      | none => simp [hs] at h
      | some actual =>
          simp_all
          rw [stamped_authority workspace _ child hs]
          exact authority_infimum_never_exceeds_left ..

end Workspace
