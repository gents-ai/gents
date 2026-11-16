import Mathlib.Data.List.Basic

/-!
# Dependency claims between installed packs

A pack may depend on other packs. Each installed pack records who requires it,
and whether someone installed it on purpose. A pack installed only because
another needed it is released with the last pack that requires it; a pack
installed on purpose never is. While anything requires a pack it cannot be
removed.

Release goes one level deep: releasing a dependency does not in turn release
what that dependency required. That is sound only because a pack that is
depended on may not itself have dependencies, which install preparation
refuses (`a_documents_dependency_with_its_own_dependencies_is_refused` in
`gents-pack` is the evidence). `claims_name_installed_packs` states what that
premise buys.
-/

namespace Pack.DependencyClaims

structure Installation where
  coordinate : String
  /-- Someone installed it on purpose. Once true it stays true. -/
  explicit : Bool
  requiredBy : List String
  deriving DecidableEq, Repr

abbrev State := List Installation

def installed (state : State) (coordinate : String) : Bool :=
  state.any (·.coordinate = coordinate)

/-- `dependent` gives up its claim on every pack not in `keep`. A pack left
with no claim that nobody installed on purpose is released. -/
def unclaim (dependent : String) (keep : List String) (state : State) : State :=
  state.filterMap fun installation =>
    if installation.coordinate ∈ keep ∨ dependent ∉ installation.requiredBy then some installation
    else
      let rest := installation.requiredBy.filter (· ≠ dependent)
      if rest.isEmpty ∧ installation.explicit = false then none
      else some { installation with requiredBy := rest }

def claim (dependent : String) (requiredBy : List String) : List String :=
  (if dependent ∈ requiredBy then requiredBy else dependent :: requiredBy).mergeSort
    fun a b => a ≤ b

def record (coordinate : String) (explicit : Bool) : State → State
  | [] => [{ coordinate, explicit, requiredBy := [] }]
  | installation :: rest =>
    if installation.coordinate = coordinate
    then { installation with explicit := installation.explicit || explicit } :: rest
    else installation :: record coordinate explicit rest

/-- Installing or upgrading a pack: every dependency must already be
installed; the pack claims them and gives up the claims it no longer needs.
`none` is a refusal, which changes nothing. -/
def install (state : State) (coordinate : String) (explicit : Bool) (dependencies : List String) :
    Option State :=
  if dependencies.all (installed state) then
    some (unclaim coordinate dependencies
      ((record coordinate explicit state).map fun installation =>
        if installation.coordinate ∈ dependencies
        then { installation with requiredBy := claim coordinate installation.requiredBy }
        else installation))
  else none

/-- Removing a pack: refused while anything requires it. Its claims go with it. -/
def remove (state : State) (coordinate : String) : Option State :=
  match state.find? (·.coordinate = coordinate) with
  | none => none
  | some installation =>
    if installation.requiredBy.isEmpty
    then some (unclaim coordinate [] (state.filter (·.coordinate ≠ coordinate)))
    else none

/-! ## Rules -/

/-- A pack installed on purpose is never released by another pack's leaving. -/
theorem explicit_is_never_released (dependent : String) (keep : List String) (state : State)
    (installation : Installation) (hmem : installation ∈ state)
    (hexplicit : installation.explicit = true) :
    ∃ kept ∈ unclaim dependent keep state,
      kept.coordinate = installation.coordinate ∧ kept.explicit = true := by
  simp only [unclaim, List.mem_filterMap]
  by_cases hkeep : installation.coordinate ∈ keep ∨ dependent ∉ installation.requiredBy
  · exact ⟨installation, ⟨installation, hmem, by simp [hkeep]⟩, rfl, hexplicit⟩
  · refine ⟨{ installation with requiredBy := installation.requiredBy.filter (· ≠ dependent) },
      ⟨installation, hmem, ?_⟩, rfl, hexplicit⟩
    simp [hkeep, hexplicit]

/-- A pack another still requires is never released. -/
theorem required_is_never_released (dependent other : String) (keep : List String) (state : State)
    (installation : Installation) (hmem : installation ∈ state)
    (hother : other ∈ installation.requiredBy) (hne : other ≠ dependent) :
    ∃ kept ∈ unclaim dependent keep state,
      kept.coordinate = installation.coordinate ∧ other ∈ kept.requiredBy := by
  simp only [unclaim, List.mem_filterMap]
  by_cases hkeep : installation.coordinate ∈ keep ∨ dependent ∉ installation.requiredBy
  · exact ⟨installation, ⟨installation, hmem, by simp [hkeep]⟩, rfl, hother⟩
  · have hrest : other ∈ installation.requiredBy.filter (· ≠ dependent) := by
      simp [List.mem_filter, hother, hne]
    refine ⟨{ installation with requiredBy := installation.requiredBy.filter (· ≠ dependent) },
      ⟨installation, hmem, ?_⟩, rfl, hrest⟩
    simp only [hkeep, if_false, Option.some.injEq]
    rw [if_neg]
    intro hreleased
    have hempty := List.isEmpty_iff.mp hreleased.1
    rw [hempty] at hrest
    exact List.not_mem_nil _ hrest

/-- After a pack gives up all its claims, nothing is required by it. -/
theorem unclaim_drops_every_claim (dependent : String) (state : State) :
    ∀ kept ∈ unclaim dependent [] state, dependent ∉ kept.requiredBy := by
  intro kept hkept
  simp only [unclaim, List.mem_filterMap, List.not_mem_nil, false_or] at hkept
  obtain ⟨installation, _, hresult⟩ := hkept
  split at hresult
  · rename_i hnone
    simp only [Option.some.injEq] at hresult
    rw [← hresult]; exact hnone
  · split at hresult
    · exact absurd hresult (by simp)
    · simp only [Option.some.injEq] at hresult
      rw [← hresult]
      simp [List.mem_filter]

/-- A pack anything requires cannot be removed. -/
theorem required_cannot_be_removed (state : State) (coordinate : String)
    (installation : Installation)
    (hfound : state.find? (·.coordinate = coordinate) = some installation)
    (hrequired : installation.requiredBy ≠ []) : remove state coordinate = none := by
  simp only [remove, hfound]
  rw [if_neg]
  simpa [List.isEmpty_iff] using hrequired

/-- A removal that goes through leaves no claim by the removed pack. -/
theorem removed_pack_claims_nothing (state after : State) (coordinate : String)
    (h : remove state coordinate = some after) :
    ∀ kept ∈ after, coordinate ∉ kept.requiredBy := by
  unfold remove at h
  split at h
  · exact absurd h (by simp)
  · split at h
    · simp only [Option.some.injEq] at h
      rw [← h]; exact unclaim_drops_every_claim coordinate _
    · exact absurd h (by simp)

theorem mem_unclaim_requiredBy (dependent : String) (keep : List String) (state : State)
    (kept : Installation) (claimant : String) (hkept : kept ∈ unclaim dependent keep state)
    (hclaim : claimant ∈ kept.requiredBy) :
    ∃ installation ∈ state, installation.coordinate = kept.coordinate
      ∧ claimant ∈ installation.requiredBy := by
  simp only [unclaim, List.mem_filterMap] at hkept
  obtain ⟨installation, hmem, hresult⟩ := hkept
  split at hresult
  · simp only [Option.some.injEq] at hresult
    exact ⟨installation, hmem, by rw [hresult], by rw [hresult]; exact hclaim⟩
  · split at hresult
    · exact absurd hresult (by simp)
    · simp only [Option.some.injEq] at hresult
      refine ⟨installation, hmem, by rw [← hresult], ?_⟩
      rw [← hresult] at hclaim
      exact (List.mem_filter.mp hclaim).1

/-- What the one-level release relies on. If every claim names an installed
pack, and no pack that requires another is itself required, then after a
removal every remaining claim still names an installed pack. Without the
second premise a released dependency could leave its own claim behind, naming
a pack that is gone. -/
theorem claims_name_installed_packs (state after : State) (coordinate : String)
    (hclaims : ∀ installation ∈ state, ∀ claimant ∈ installation.requiredBy,
      installed state claimant = true)
    (hleaves : ∀ installation ∈ state, ∀ claimant ∈ installation.requiredBy,
      ∀ holder ∈ state, holder.coordinate = claimant → holder.requiredBy = [])
    (h : remove state coordinate = some after) :
    ∀ kept ∈ after, ∀ claimant ∈ kept.requiredBy, installed after claimant = true := by
  intro kept hkept claimant hclaimant
  have hnot : claimant ≠ coordinate := fun heq =>
    removed_pack_claims_nothing state after coordinate h kept hkept (heq ▸ hclaimant)
  unfold remove at h
  split at h
  · exact absurd h (by simp)
  · split at h
    · simp only [Option.some.injEq] at h
      subst h
      obtain ⟨installation, hmem, _, hclaim⟩ :=
        mem_unclaim_requiredBy coordinate [] _ kept claimant hkept hclaimant
      have hstate := (List.mem_filter.mp hmem).1
      have hinstalled := hclaims installation hstate claimant hclaim
      simp only [installed, List.any_eq_true, decide_eq_true_eq] at hinstalled ⊢
      obtain ⟨holder, hholder, hname⟩ := hinstalled
      have hfree := hleaves installation hstate claimant hclaim holder hholder hname
      refine ⟨holder, ?_, hname⟩
      simp only [unclaim, List.mem_filterMap, List.not_mem_nil, false_or]
      refine ⟨holder, List.mem_filter.mpr ⟨hholder, by simpa [hname] using hnot⟩, ?_⟩
      simp [hfree]
    · exact absurd h (by simp)

end Pack.DependencyClaims
