/-! A help page or a failing call names the skill in the caller's own catalog
that covers its resource. A path is the tool's name followed by the words that
name the resource and its topic or verb (`config trigger explain`, `schema
collections policy`); a skill covers the paths that start with one of its
covers. -/

namespace SkillPointer

abbrev Path := List String

/-- A skill as the caller's catalog lists it: the name `load_skill` takes and
the paths it declares it covers. -/
structure Entry where
  name : String
  covers : List Path
  deriving DecidableEq, Repr

/-- The longest of `entry`'s covers that starts `path`; an empty cover covers
nothing. -/
def matched (entry : Entry) (path : Path) : Option Path :=
  entry.covers.foldl
    (fun best cover =>
      if !cover.isEmpty && cover.isPrefixOf path then
        match best with
        | some b => if b.length < cover.length then some cover else some b
        | none => some cover
      else best)
    none

/-- `entry` replaces the best so far only with a strictly longer cover, so the
most specific skill wins and, among equally specific ones, the first in
catalog order. -/
def better (path : Path) (best : Option (String × Path)) (entry : Entry) :
    Option (String × Path) :=
  match matched entry path, best with
  | none, _ => best
  | some cover, none => some (entry.name, cover)
  | some cover, some (name, b) =>
    if b.length < cover.length then some (entry.name, cover) else some (name, b)

/-- The skill a page or failure at `path` names, with the cover that matched.
The catalog is the caller's own, in the order it renders; a caller whose
catalog has no covering skill is named nothing. -/
def pointer (catalog : List Entry) (path : Path) : Option (String × Path) :=
  catalog.foldl (better path) none

inductive Kind where
  | help
  | failure
  deriving DecidableEq, Repr

/-- A help page read, or a non-help call that failed, in request `request`. -/
structure Event where
  kind : Kind
  request : String
  path : Path
  deriving DecidableEq, Repr

/-- The covers a failure has already named in the request seen last. -/
structure Memory where
  request : Option String := none
  failed : List Path := []
  deriving DecidableEq, Repr

/-- Every help page names its covering skill. A failure names it only the first
time that cover fails in a request; a failure in another request starts the
memory again, so a request interleaved with another can be named twice and is
never named less than once. -/
def step (catalog : List Entry) (memory : Memory) (event : Event) :
    Option String × Memory :=
  match event.kind with
  | .help => ((pointer catalog event.path).map Prod.fst, memory)
  | .failure =>
    let failed := if memory.request = some event.request then memory.failed else []
    let seen : Memory := { request := some event.request, failed }
    match pointer catalog event.path with
    | none => (none, seen)
    | some (name, cover) =>
      if cover ∈ failed then (none, seen)
      else (some name, { seen with failed := cover :: failed })

def run (catalog : List Entry) : Memory → List Event → List (Option String)
  | _, [] => []
  | memory, event :: events =>
    let (named, memory) := step catalog memory event
    named :: run catalog memory events

theorem better_keeps_or_names (path : Path) (best : Option (String × Path)) (entry : Entry)
    (name : String) (cover : Path) (h : better path best entry = some (name, cover)) :
    best = some (name, cover) ∨ entry.name = name := by
  unfold better at h
  split at h
  · exact Or.inl h
  · simp at h
    exact Or.inr h.1
  · split at h
    · simp at h
      exact Or.inr h.1
    · simp_all

theorem fold_names_from (path : Path) (catalog : List Entry) :
    ∀ (entries : List Entry) (best : Option (String × Path)),
      (∀ name cover, best = some (name, cover) → ∃ e ∈ catalog, e.name = name) →
      (∀ e ∈ entries, e ∈ catalog) →
      ∀ name cover, entries.foldl (better path) best = some (name, cover) →
        ∃ e ∈ catalog, e.name = name := by
  intro entries
  induction entries with
  | nil => intro best hbest _ name cover h; exact hbest name cover h
  | cons entry rest ih =>
    intro best hbest hmem name cover h
    apply ih (better path best entry) _ (fun e he => hmem e (List.mem_cons_of_mem _ he))
      name cover h
    intro n c hc
    rcases better_keeps_or_names path best entry n c hc with hb | he
    · exact hbest n c hb
    · exact ⟨entry, hmem entry (List.mem_cons_self _ _), he⟩

/-- Only a skill the caller can load is ever named. -/
theorem pointer_names_a_catalog_entry (catalog : List Entry) (path : Path)
    (name : String) (cover : Path) (h : pointer catalog path = some (name, cover)) :
    ∃ e ∈ catalog, e.name = name :=
  fold_names_from path catalog catalog none (by simp) (fun _ he => he) name cover h

theorem empty_catalog_names_nothing (path : Path) : pointer [] path = none := rfl

theorem step_names_a_catalog_entry (catalog : List Entry) (memory : Memory) (event : Event)
    (name : String) (h : (step catalog memory event).1 = some name) :
    ∃ e ∈ catalog, e.name = name := by
  unfold step at h
  split at h
  · cases hp : pointer catalog event.path with
    | none => simp [hp] at h
    | some p =>
      simp [hp] at h
      exact pointer_names_a_catalog_entry catalog event.path name p.2 (by rw [hp, ← h])
  · cases hp : pointer catalog event.path with
    | none => simp [hp] at h
    | some p =>
      obtain ⟨n, c⟩ := p
      by_cases hc : c ∈ (if memory.request = some event.request then memory.failed else [])
      · simp [hp, hc] at h
      · simp [hp, hc] at h
        subst h
        exact pointer_names_a_catalog_entry catalog event.path n c hp

/-- A failure repeated in the same request after its cover was named names
nothing. -/
theorem repeated_failure_names_nothing (catalog : List Entry) (memory : Memory)
    (event : Event) (h : event.kind = .failure) :
    (step catalog (step catalog memory event).2 event).1 = none := by
  cases hp : pointer catalog event.path with
  | none => simp [step, h, hp]
  | some p =>
    obtain ⟨name, cover⟩ := p
    by_cases hm : memory.request = some event.request <;> by_cases hc : cover ∈ memory.failed <;>
      simp [step, h, hp, hm, hc]

end SkillPointer
