import Mathlib.Data.Finset.Basic
import Mathlib.Data.List.Defs

/-!
# Canonical filesystem-root admission

This model starts *after* the host path resolver has produced a canonical
component sequence.  Resolving existing prefixes, following symlinks, retaining
nonexistent suffixes, and re-resolving at execution are Rust refinement
obligations; Lean does not model the host filesystem or claim TOCTOU safety.

Authority is ordered by path containment.  A candidate is admitted only when it
is equal to or below a published root.  Component-prefix containment admits a
least-privilege descendant without admitting string-prefix siblings.

Publication retains whether the operator authored any root policy.  No
documents may use the process ceiling as a backwards-compatible default, but
an explicit policy whose enabled set is empty is a revocation and must not
fall back to that broader ceiling.
-/

namespace PeerRegistryDiscovery
namespace RootAdmission

/-- A host-observed path after the canonical path owner has resolved it.
`anchor` identifies the filesystem root/volume (for example POSIX `/`, a
Windows drive, or a UNC share). Components, rather than rendered strings,
define containment within that anchor. -/
structure CanonicalPath where
  anchor : String
  components : List String
  deriving DecidableEq, Repr

/-- `ceiling` contains `candidate` exactly when the ceiling's canonical
components are a prefix of the candidate's canonical components. -/
def contains (ceiling candidate : CanonicalPath) : Prop :=
  ceiling.anchor = candidate.anchor ∧
    ceiling.components.IsPrefix candidate.components

instance (ceiling candidate : CanonicalPath) : Decidable (contains ceiling candidate) := by
  unfold contains
  infer_instance

/-- Resolution failure is fail-closed.  A successful candidate must be equal
to or narrower than at least one published root. -/
def admitted (published : Finset CanonicalPath) (candidate : Option CanonicalPath) : Prop :=
  match candidate with
  | none => False
  | some path => ∃ ceiling ∈ published, contains ceiling path

instance (published : Finset CanonicalPath) (candidate : Option CanonicalPath) :
    Decidable (admitted published candidate) := by
  unfold admitted
  cases candidate <;> infer_instance

/-- The operator-local workspace-root policy after document and process-ceiling
resolution. `configured` records fail-closed policy state: either a
`WorkspaceRoot` document was supplied (including disabled documents), or an
explicit process ceiling was supplied but could not be resolved. `enabled`
contains only enabled, resolved roots. Keeping those facts separate prevents
an all-disabled, all-invalid, or invalid-ceiling policy from looking like an
unconfigured deployment. -/
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

/-- Explicit roots are published only when contained by the process ceiling.
When there are no root documents, the ceiling remains the legacy default.
An explicit-but-empty policy publishes nothing. -/
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

/-- An explicitly supplied process ceiling that failed host resolution is
represented by configured policy with no resolved ceiling. It publishes no
authority and cannot admit an authored root through the legacy no-policy
fallback. -/
theorem invalid_ceiling_fails_closed (candidate : CanonicalPath) :
    publishedRoots ⟨true, ∅, none⟩ = ∅ ∧
      ¬ admitted (publishedRoots ⟨true, ∅, none⟩) (some candidate) := by
  simp [publishedRoots, admitted]

/-- Explicit publication never invents the process ceiling or another root:
every published path came from the enabled document set. -/
theorem explicit_publication_subset (policy : PublicationPolicy)
    (hconfigured : policy.configured = true) :
    publishedRoots policy ⊆ policy.enabled := by
  intro root hroot
  simp [publishedRoots, hconfigured] at hroot
  exact hroot.1

/-- With a process ceiling, every explicitly published root is contained by
that ceiling.  Publication can narrow authority but cannot widen it. -/
theorem explicit_publication_under_ceiling (enabled : Finset CanonicalPath)
    (ceiling root : CanonicalPath)
    (hroot : root ∈ publishedRoots ⟨true, enabled, some ceiling⟩) :
    contains ceiling root := by
  simp [publishedRoots, withinCeiling] at hroot
  exact hroot.2

/-- A narrower explicit root does not republish the broader ceiling unless the
ceiling was itself explicitly enabled. -/
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

/-- Admission never widens authority: it returns a published ceiling witness
whose canonical components prefix the selected path. -/
theorem admitted_has_published_ceiling (published : Finset CanonicalPath)
    (candidate : CanonicalPath) (h : admitted published (some candidate)) :
    ∃ ceiling ∈ published, contains ceiling candidate := by
  exact h

/-- Adding a path component narrows (or preserves) path authority. -/
theorem descendant_transitive (outer inner candidate : CanonicalPath)
    (hOuter : contains outer inner) (hInner : contains inner candidate) :
    contains outer candidate := by
  exact ⟨hOuter.1.trans hInner.1, hOuter.2.trans hInner.2⟩

/-- Equal component sequences on different filesystem anchors never confer
authority. This prevents a drive/volume collision from becoming containment. -/
theorem distinct_anchor_rejected (left right : String) (components : List String)
    (h : left ≠ right) :
    ¬ contains ⟨left, components⟩ ⟨right, components⟩ := by
  simp [contains, h]

/-- A component-wise sibling is not a descendant merely because its rendered
name shares a string prefix. -/
theorem prefix_sibling_rejected (base sibling : String) (tail : List String)
    (h : base ≠ sibling) :
    ¬ contains ⟨"/", [base]⟩ ⟨"/", sibling :: tail⟩ := by
  simp [contains, h]

end RootAdmission
end PeerRegistryDiscovery
