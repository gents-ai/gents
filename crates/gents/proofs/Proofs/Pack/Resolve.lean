import Proofs.Pack.Store

/-!
# Resolving a pack by name

A pack named by coordinate, with or without a version, is looked for in three
places, in this order: the packs installed on the node, the node's pack
store, and the registry. The order is what lets a node work offline and keeps
an installed pack from changing under its name: what is installed wins over a
newer version that merely sits in the store, and the registry is asked only
when nothing local answers.

An installation record whose archive is gone answers nothing; the store is
asked next. A pinned version resolves to exactly that version or not locally.

The name and pin are a well-formed pack spec; the spec grammar refuses
anything else before resolution starts. What the registry answers is not the
node's to predict, so the model ends at the decision to ask it.
-/

namespace Pack.Resolve

open Pack.Store

inductive Origin | installed | store | registry
  deriving DecidableEq, Repr

structure Resolution where
  origin : Origin
  /-- The version resolved locally. -/
  version : Option String
  deriving DecidableEq, Repr

def wanted (name : String) (version : Option String) (pack : Pack) : Bool :=
  pack.name = name && match version with
    | some pinned => pack.version.text = pinned
    | none => true

/-- The store's answer: the pinned version if it is indexed and present, or
the highest version when none is pinned. -/
def stored (state : State) (name : String) (version : Option String) : Option String :=
  match version with
  | some pinned =>
    (state.index.find? fun pack => wanted name (some pinned) pack && state.found pack).map
      (·.version.text)
  | none => state.latest name

def resolve (state : State) (installed : List Pack) (name : String) (version : Option String) :
    Resolution :=
  match installed.find? fun record => wanted name version record && state.contains record with
  | some record => { origin := .installed, version := some record.version.text }
  | none =>
    match stored state name version with
    | some text => { origin := .store, version := some text }
    | none => { origin := .registry, version := none }

/-- What is installed wins, whatever the store holds. -/
theorem installed_wins (state : State) (installed : List Pack) (name : String)
    (version : Option String) (record : Pack) (hmem : record ∈ installed)
    (hwanted : wanted name version record = true) (hpresent : state.contains record = true) :
    (resolve state installed name version).origin = .installed := by
  unfold resolve
  split
  · rfl
  · rename_i hnone
    have := List.find?_eq_none.mp hnone record hmem
    simp [hwanted, hpresent] at this

/-- A pin resolves locally to exactly the pinned version. -/
theorem pin_is_exact (state : State) (installed : List Pack) (name pinned text : String)
    (h : (resolve state installed name (some pinned)).version = some text) : text = pinned := by
  unfold resolve at h
  split at h
  · rename_i record hfound
    have hwanted := List.find?_some hfound
    simp only [wanted, Bool.and_eq_true, decide_eq_true_eq] at hwanted
    simp only [Option.some.injEq] at h
    rw [← h]; exact hwanted.1.2
  · split at h
    · rename_i found hstored
      simp only [stored, Option.map_eq_some'] at hstored
      obtain ⟨pack, hfound, rfl⟩ := hstored
      have hwanted := List.find?_some hfound
      simp only [wanted, Bool.and_eq_true, decide_eq_true_eq] at hwanted
      simp only [Option.some.injEq] at h
      rw [← h]; exact hwanted.1.2
    · simp at h

/-- The registry is asked only when no installed pack of that name is present
and the store has no answer. -/
theorem registry_is_last (state : State) (installed : List Pack) (name : String)
    (version : Option String)
    (h : (resolve state installed name version).origin = .registry) :
    (∀ record ∈ installed, wanted name version record = true → state.contains record = false)
      ∧ stored state name version = none := by
  unfold resolve at h
  split at h
  · simp at h
  · rename_i hnone
    split at h
    · simp at h
    · rename_i hstored
      refine ⟨fun record hmem hwanted => ?_, hstored⟩
      have := List.find?_eq_none.mp hnone record hmem
      simpa [hwanted] using this

end Pack.Resolve
