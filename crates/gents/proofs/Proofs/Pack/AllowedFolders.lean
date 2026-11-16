import Mathlib.Data.List.Basic

/-!
# Folders a plugin may be bound to

A plugin reaches the filesystem only through a folder or file bound for one
call. What may be bound is the node's list of allowed paths plus, read-only,
the folder the session works in. Three places can never be allowed, whoever
asks: the filesystem root, the user's home or anything holding it, and the
gents home, anything inside it or anything holding it. A grant that broad
would hand a plugin the node's own keys and records.

Paths here are canonical: links are resolved and `..` is gone before any rule
is asked. External premise, with tests in `gents-pack` as evidence: the
filesystem canonicalizes a path before the scope check, a remembered path is
compared with its canonical target again when it is used, and a file is bound
alone through a hard link, so an allowed file never exposes its folder.
-/

namespace Pack.AllowedFolders

/-- A canonical absolute path: its components from the filesystem root. -/
abbrev Path := List String

inductive Access | read | readWrite
  deriving DecidableEq, Repr

def Access.rank : Access → Nat
  | .read => 0
  | .readWrite => 1

def Access.max (a b : Access) : Access := if a.rank < b.rank then b else a

structure World where
  userHome : Option Path
  gentsHome : Path
  /-- The files and folders that exist. -/
  present : List Path
  deriving Repr

/-- `target` is `root` or lies inside it. -/
def under (target root : Path) : Bool := root.isPrefixOf target

/-- A folder that is the filesystem root, or is or holds the user's home or
the gents home. -/
def tooBroad (world : World) (dir : Path) : Bool :=
  dir.isEmpty
    || (match world.userHome with
        | some home => under home dir
        | none => false)
    || under world.gentsHome dir

/-- A path that may be on the allowed list. -/
def allowable (world : World) (path : Path) : Bool :=
  !tooBroad world path && !under path world.gentsHome

structure Entry where
  path : Path
  access : Access
  deriving DecidableEq, Repr

/-- One entry per path. An approval given while another grant was made keeps
the wider of the two. -/
def put (entries : List Entry) (entry : Entry) (keepWider : Bool) : List Entry :=
  let access :=
    if keepWider then
      (entries.filter (·.path = entry.path)).foldl (fun held held' => held.max held'.access)
        entry.access
    else entry.access
  entries.filter (·.path ≠ entry.path) ++ [{ entry with access }]

inductive Action | add | grantAtLeast | remove
  deriving DecidableEq, Repr

inductive Outcome | granted | refused | removed | absent
  deriving DecidableEq, Repr

structure Step where
  action : Action
  path : Path
  access : Access
  deriving DecidableEq, Repr

def step (world : World) (entries : List Entry) (s : Step) : List Entry × Outcome :=
  match s.action with
  | .remove =>
    if entries.any (·.path = s.path) then (entries.filter (·.path ≠ s.path), .removed)
    else (entries, .absent)
  | .add =>
    if s.path ∈ world.present ∧ allowable world s.path = true
    then (put entries ⟨s.path, s.access⟩ false, .granted) else (entries, .refused)
  | .grantAtLeast =>
    if s.path ∈ world.present ∧ allowable world s.path = true
    then (put entries ⟨s.path, s.access⟩ true, .granted) else (entries, .refused)

def run (world : World) (entries : List Entry) : List Step → List Entry × List Outcome
  | [] => (entries, [])
  | s :: rest =>
    let stepped := step world entries s
    let ran := run world stepped.1 rest
    (ran.1, stepped.2 :: ran.2)

/-- What a call may bind: the listed paths and, read-only, the working folder
unless it is too broad. A list holding a path that is gone or not allowable
is refused whole: the list is not trusted because it was once written. -/
def scope (world : World) (entries : List Entry) (workdir : Option Path) : Option (List Entry) :=
  if entries.all fun entry => decide (entry.path ∈ world.present) && allowable world entry.path then
    some (entries ++ match workdir with
      | some dir =>
        if dir ∈ world.present ∧ tooBroad world dir = false then [⟨dir, .read⟩] else []
      | none => [])
  else none

/-- The widest access any entry covering `target` gives. -/
def granted (entries : List Entry) (target : Path) : Option Access :=
  (entries.filter fun entry => under target entry.path).foldl
    (fun held entry => some (match held with
      | none => entry.access
      | some held => held.max entry.access))
    none

/-- A path a call may name at all: it exists and neither is, is inside, nor
holds the gents home. -/
def reachable (world : World) (target : Path) : Bool :=
  decide (target ∈ world.present) && !(under target world.gentsHome || under world.gentsHome target)

/-! ## Rules -/

theorem root_is_never_allowable (world : World) : allowable world [] = false := by
  simp [allowable, tooBroad]

/-- No folder holding the user's home can be listed. -/
theorem home_holder_is_never_allowable (world : World) (home dir : Path)
    (hhome : world.userHome = some home) (h : under home dir = true) :
    allowable world dir = false := by
  simp [allowable, tooBroad, hhome, h]

/-- Neither the gents home, nor anything inside it, nor anything holding it
can be listed. -/
theorem gents_home_is_never_allowable (world : World) (path : Path)
    (h : under path world.gentsHome = true ∨ under world.gentsHome path = true) :
    allowable world path = false := by
  rcases h with h | h <;> simp [allowable, tooBroad, h]

/-- A listed path covers nothing inside the gents home. -/
theorem allowable_covers_nothing_in_gents_home (world : World) (path target : Path)
    (hallowable : allowable world path = true) (hcovers : under target path = true) :
    under target world.gentsHome = false := by
  by_contra hinside
  simp only [Bool.not_eq_false] at hinside
  simp only [under, List.isPrefixOf_iff_prefix] at hcovers hinside
  rcases List.prefix_or_prefix_of_prefix hcovers hinside with h | h
  · have : under world.gentsHome path = true := by simpa [under, List.isPrefixOf_iff_prefix] using h
    simp [allowable, tooBroad, this] at hallowable
  · have : under path world.gentsHome = true := by simpa [under, List.isPrefixOf_iff_prefix] using h
    simp [allowable, this] at hallowable

theorem mem_put (entries : List Entry) (entry : Entry) (keepWider : Bool) (held : Entry)
    (h : held ∈ put entries entry keepWider) : held ∈ entries ∨ held.path = entry.path := by
  simp only [put, List.mem_append, List.mem_filter, List.mem_singleton] at h
  rcases h with h | h
  · exact Or.inl h.1
  · exact Or.inr (by rw [h])

/-- Whatever is asked of the list, it never comes to hold a path that is not
allowable. -/
theorem step_keeps_the_list_allowable (world : World) (entries : List Entry) (s : Step)
    (h : ∀ entry ∈ entries, allowable world entry.path = true) :
    ∀ entry ∈ (step world entries s).1, allowable world entry.path = true := by
  intro entry hmem
  obtain ⟨action, path, access⟩ := s
  cases action <;> simp only [step] at hmem
  · split at hmem
    · rename_i hok
      rcases mem_put _ _ _ _ hmem with hold | hsame
      · exact h entry hold
      · rw [hsame]; exact hok.2
    · exact h entry hmem
  · split at hmem
    · rename_i hok
      rcases mem_put _ _ _ _ hmem with hold | hsame
      · exact h entry hold
      · rw [hsame]; exact hok.2
    · exact h entry hmem
  · split at hmem
    · exact h entry (List.mem_filter.mp hmem).1
    · exact h entry hmem

theorem run_keeps_the_list_allowable (world : World) (steps : List Step) (entries : List Entry)
    (h : ∀ entry ∈ entries, allowable world entry.path = true) :
    ∀ entry ∈ (run world entries steps).1, allowable world entry.path = true := by
  induction steps generalizing entries with
  | nil => exact h
  | cons s rest ih => exact ih _ (step_keeps_the_list_allowable world entries s h)

/-- A removed path is no longer listed. -/
theorem remove_forgets (world : World) (entries : List Entry) (path : Path) (access : Access) :
    ∀ entry ∈ (step world entries ⟨.remove, path, access⟩).1, entry.path ≠ path := by
  intro entry hmem
  simp only [step] at hmem
  split at hmem
  · simpa using (List.mem_filter.mp hmem).2
  · rename_i hnone
    intro hsame
    exact hnone (List.any_eq_true.mpr ⟨entry, hmem, by simp [hsame]⟩)

theorem granted_none_of_no_cover (entries : List Entry) (target : Path)
    (h : ∀ entry ∈ entries, under target entry.path = false) : granted entries target = none := by
  have : entries.filter (fun entry => under target entry.path) = [] :=
    List.filter_eq_nil_iff.mpr fun entry hmem => by simp [h entry hmem]
  simp [granted, this]

/-- Nothing inside the gents home is ever granted from the allowed list. -/
theorem list_grants_nothing_in_gents_home (world : World) (entries : List Entry) (target : Path)
    (hlist : ∀ entry ∈ entries, allowable world entry.path = true)
    (hinside : under target world.gentsHome = true) : granted entries target = none := by
  apply granted_none_of_no_cover
  intro entry hmem
  by_contra hcovers
  simp only [Bool.not_eq_false] at hcovers
  have := allowable_covers_nothing_in_gents_home world entry.path target (hlist entry hmem) hcovers
  simp [hinside] at this

end Pack.AllowedFolders
