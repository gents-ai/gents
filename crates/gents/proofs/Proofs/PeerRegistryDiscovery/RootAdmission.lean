import Mathlib.Data.Finset.Basic
import Mathlib.Data.List.Defs

/-!
# Canonical filesystem-root admission

The model starts after the host path resolver has produced a canonical
component sequence; resolving prefixes, following symlinks, and
re-resolving at execution are Rust refinement obligations, and Lean
claims no TOCTOU safety. Publication resolution is fail-closed, and an
explicit policy whose enabled set is empty is a revocation, never a
fallback to the process ceiling.
-/

namespace PeerRegistryDiscovery
namespace RootAdmission

/-- `anchor` identifies the filesystem root/volume (POSIX `/`, a Windows
drive, a UNC share); containment is defined by components, not rendered
strings, within that anchor. -/
structure CanonicalPath where
  anchor : String
  components : List String
  deriving DecidableEq, Repr

def contains (ceiling candidate : CanonicalPath) : Prop :=
  ceiling.anchor = candidate.anchor ∧
    ceiling.components.IsPrefix candidate.components

instance (ceiling candidate : CanonicalPath) : Decidable (contains ceiling candidate) := by
  unfold contains
  infer_instance

def admitted (published : Finset CanonicalPath) (candidate : Option CanonicalPath) : Prop :=
  match candidate with
  | none => False
  | some path => ∃ ceiling ∈ published, contains ceiling path

instance (published : Finset CanonicalPath) (candidate : Option CanonicalPath) :
    Decidable (admitted published candidate) := by
  unfold admitted
  cases candidate <;> infer_instance

/-- `configured` records fail-closed policy state: either a `WorkspaceRoot`
document was supplied (including disabled documents), or an explicit process
ceiling was supplied but could not be resolved. Keeping it separate from
`enabled` prevents an all-disabled, all-invalid, or invalid-ceiling policy
from looking like an unconfigured deployment. -/
structure PublicationPolicy where
  configured : Bool
  enabled : Finset CanonicalPath
  ceiling : Option CanonicalPath
  deriving DecidableEq

def withinCeiling (ceiling : Option CanonicalPath) (root : CanonicalPath) : Prop :=
  match ceiling with
  | none => True
  | some ceiling => contains ceiling root

instance (ceiling : Option CanonicalPath) (root : CanonicalPath) :
    Decidable (withinCeiling ceiling root) := by
  unfold withinCeiling
  cases ceiling <;> infer_instance

def defaultRoots : Option CanonicalPath → Finset CanonicalPath
  | none => ∅
  | some ceiling => {ceiling}

def publishedRoots (policy : PublicationPolicy) : Finset CanonicalPath :=
  if policy.configured then
    policy.enabled.filter (withinCeiling policy.ceiling)
  else
    defaultRoots policy.ceiling

theorem unconfigured_uses_ceiling (ceiling : Option CanonicalPath) :
    publishedRoots ⟨false, ∅, ceiling⟩ = defaultRoots ceiling := by
  simp [publishedRoots]

theorem all_disabled_revokes_without_ceiling_fallback (ceiling : Option CanonicalPath) :
    publishedRoots ⟨true, ∅, ceiling⟩ = ∅ := by
  simp [publishedRoots]

theorem invalid_ceiling_fails_closed (candidate : CanonicalPath) :
    publishedRoots ⟨true, ∅, none⟩ = ∅ ∧
      ¬ admitted (publishedRoots ⟨true, ∅, none⟩) (some candidate) := by
  simp [publishedRoots, admitted]

theorem explicit_publication_subset (policy : PublicationPolicy)
    (hconfigured : policy.configured = true) :
    publishedRoots policy ⊆ policy.enabled := by
  intro root hroot
  simp [publishedRoots, hconfigured] at hroot
  exact hroot.1

theorem explicit_publication_under_ceiling (enabled : Finset CanonicalPath)
    (ceiling root : CanonicalPath)
    (hroot : root ∈ publishedRoots ⟨true, enabled, some ceiling⟩) :
    contains ceiling root := by
  simp [publishedRoots, withinCeiling] at hroot
  exact hroot.2

theorem narrower_policy_does_not_publish_ceiling (ceiling nested : CanonicalPath)
    (hneq : nested ≠ ceiling) :
    ceiling ∉ publishedRoots ⟨true, {nested}, some ceiling⟩ := by
  simp only [publishedRoots, Bool.true_eq, if_pos, Finset.mem_filter,
    Finset.mem_singleton]
  intro hmem
  exact hneq hmem.1.symm

theorem unresolved_rejected (published : Finset CanonicalPath) :
    ¬ admitted published none := by
  simp [admitted]

theorem exact_root_admitted (published : Finset CanonicalPath) (path : CanonicalPath)
    (h : path ∈ published) : admitted published (some path) := by
  exact ⟨path, h, rfl, ⟨[], by simp⟩⟩

theorem descendant_admitted (published : Finset CanonicalPath) (ceiling : CanonicalPath)
    (suffix : List String) (h : ceiling ∈ published) :
    admitted published (some ⟨ceiling.anchor, ceiling.components ++ suffix⟩) := by
  exact ⟨ceiling, h, rfl, List.prefix_append _ _⟩

theorem admitted_has_published_ceiling (published : Finset CanonicalPath)
    (candidate : CanonicalPath) (h : admitted published (some candidate)) :
    ∃ ceiling ∈ published, contains ceiling candidate := by
  exact h

theorem descendant_transitive (outer inner candidate : CanonicalPath)
    (hOuter : contains outer inner) (hInner : contains inner candidate) :
    contains outer candidate := by
  exact ⟨hOuter.1.trans hInner.1, hOuter.2.trans hInner.2⟩

/-- Prevents a drive/volume collision from becoming containment. -/
theorem distinct_anchor_rejected (left right : String) (components : List String)
    (h : left ≠ right) :
    ¬ contains ⟨left, components⟩ ⟨right, components⟩ := by
  simp [contains, h]

theorem prefix_sibling_rejected (base sibling : String) (tail : List String)
    (h : base ≠ sibling) :
    ¬ contains ⟨"/", [base]⟩ ⟨"/", sibling :: tail⟩ := by
  simp [contains, h]

/-- Overlap tolerance is owned by `storedRootSelectionOk` only: a freshly
authored root must be strictly admitted, so a behavior cannot pre-position a
wide grant that silently activates when the operator later widens authority. -/
def overlapping (published : Finset CanonicalPath) (candidate : Option CanonicalPath) : Prop :=
  match candidate with
  | none => False
  | some path => ∃ root ∈ published, contains path root

instance (published : Finset CanonicalPath) (candidate : Option CanonicalPath) :
    Decidable (overlapping published candidate) := by
  unfold overlapping
  cases candidate <;> infer_instance

/-- A blank root may inherit the runtime default only when the operator has
not authored explicit workspace-root policy; under explicit policy a request
must select an admitted root so omission cannot widen to the process ceiling.
The host execution owner resolves again before use; no TOCTOU claim. -/
abbrev rootSelectionOk
    (policyConfigured : Bool)
    (published : Finset CanonicalPath)
    (blank : Bool)
    (resolved : Option CanonicalPath) : Prop :=
  (blank = true ∧ policyConfigured = false) ∨
    (blank = false ∧ policyConfigured = false ∧ published = ∅ ∧ resolved.isSome) ∨
    admitted published resolved

/-- Tolerance for re-admitting a stored grant against current policy: the
behavior root is authored once while the operator root moves independently,
so a stored grant the operator narrowed below stays selectable because the
meet (`effectiveSelectionOk`) clamps it before use; refusing every overlap
would strand a narrowed deployment's only behavior. A write that changes the
root follows `rootSelectionOk` strictly. -/
abbrev storedRootSelectionOk
    (policyConfigured : Bool)
    (published : Finset CanonicalPath)
    (blank : Bool)
    (resolved : Option CanonicalPath) : Prop :=
  rootSelectionOk policyConfigured published blank resolved ∨
    overlapping published resolved

/-- Selection/ceiling meet for the effective tool root: containment decides
whether the grant or the cap wins. Premise this meet alone cannot close: under
a configured multi-root policy publishing roots strictly inside the process
ceiling, the clamp arm lands at the raw ceiling, admitted by neither `admitted`
nor any published root; the execution-boundary re-admission (the native root
execution guard) owns refusing such an effective root before use. -/
def effectiveSelectionOk
    (policyConfigured : Bool)
    (ceiling : Option CanonicalPath)
    (selection : Option CanonicalPath)
    (effective : Option CanonicalPath) : Prop :=
  match ceiling, selection with
  | _, none => policyConfigured = false ∧ effective = ceiling
  | none, some sel => effective = some sel
  | some c, some sel =>
      (contains c sel ∧ effective = some sel) ∨ (contains sel c ∧ effective = some c)

instance (policyConfigured : Bool) (ceiling selection effective : Option CanonicalPath) :
    Decidable (effectiveSelectionOk policyConfigured ceiling selection effective) := by
  unfold effectiveSelectionOk
  cases ceiling <;> cases selection <;> infer_instance

theorem effective_selection_never_widens (policyConfigured : Bool)
    (ceiling : CanonicalPath) (selection : Option CanonicalPath)
    (effective : Option CanonicalPath)
    (hok : effectiveSelectionOk policyConfigured (some ceiling) selection effective) :
    ∃ admittedRoot, effective = some admittedRoot ∧ contains ceiling admittedRoot := by
  simp only [effectiveSelectionOk] at hok
  cases selection with
  | none => exact ⟨ceiling, hok.2, rfl, [], by simp⟩
  | some sel =>
    rcases hok with ⟨hcontains, heffective⟩ | ⟨_, heffective⟩
    · exact ⟨sel, heffective, hcontains⟩
    · exact ⟨ceiling, heffective, rfl, [], by simp⟩

theorem clamped_selection_only_narrows (policyConfigured : Bool)
    (ceiling sel : CanonicalPath)
    (hcontains : contains sel ceiling)
    (effective : Option CanonicalPath)
    (hok : effectiveSelectionOk policyConfigured (some ceiling) (some sel) effective) :
    ∃ admittedRoot, effective = some admittedRoot ∧ contains sel admittedRoot := by
  simp only [effectiveSelectionOk] at hok
  rcases hok with ⟨_, heffective⟩ | ⟨_, heffective⟩
  · exact ⟨sel, heffective, rfl, [], by simp⟩
  · exact ⟨ceiling, heffective, hcontains⟩

theorem unconfigured_meet_stays_published (ceiling : CanonicalPath)
    (selection effective : Option CanonicalPath)
    (hok : effectiveSelectionOk false (some ceiling) selection effective) :
    admitted (publishedRoots ⟨false, ∅, some ceiling⟩) effective := by
  have hpublished : publishedRoots ⟨false, ∅, some ceiling⟩ = {ceiling} := by
    simp [publishedRoots, defaultRoots]
  rw [hpublished]
  simp only [effectiveSelectionOk] at hok
  cases selection with
  | none =>
      obtain ⟨_, heffective⟩ := hok
      rw [heffective]
      exact exact_root_admitted _ ceiling (by simp)
  | some sel =>
      rcases hok with ⟨hcontains, heffective⟩ | ⟨_, heffective⟩
      · rw [heffective]
        exact ⟨ceiling, by simp, hcontains⟩
      · rw [heffective]
        exact exact_root_admitted _ ceiling (by simp)

abbrev rootEditSelectionOk
    (policyConfigured : Bool)
    (published : Finset CanonicalPath)
    (storedRoot : String)
    (resolvedStored : Option CanonicalPath)
    (storedRootRequired : Bool)
    (update : Option (Option String))
    (resolved : Option CanonicalPath) : Prop :=
  match update with
  | none =>
      storedRootRequired = false ∨
        storedRootSelectionOk policyConfigured published (storedRoot.trim == "") resolvedStored
  | some none => policyConfigured = false
  | some (some root) =>
      rootSelectionOk policyConfigured published (root.trim == "") resolved

end RootAdmission
end PeerRegistryDiscovery
