import Mathlib.Data.List.Basic
import Mathlib.Data.List.Dedup

/-!
# The pack store

A node keeps every pack it has received as one archive named by the digest of
its contents, and beside the archives a name index: one entry per coordinate
and version, recording the digest. The archives are the truth. An index entry
is only a way to find a digest by name, and it answers only while its archive
is present.

External premises, each with a test in `gents-pack` as evidence:
- an archive is stored under the digest of its verified contents, so a
  damaged or foreign file is never stored and a stored file that no longer
  matches its name is refused when opened;
- a version is compared as the `semver` crate compares it. The model orders
  the numeric triple and puts a pre-release below its release; two
  pre-releases of one triple are ordered as text, which is `semver`'s order
  only for single non-numeric tags, so the model's inputs use those;
- the name index lives on a filesystem that may fold case, and its file names
  are encoded so that versions differing only in case stay distinct;
- a version that is not indexable is never used as a path.
-/

namespace Pack.Store

structure Semver where
  major : Nat
  minor : Nat
  patch : Nat
  pre : Option String
  deriving DecidableEq, Repr

/-- A version: its text, and its semantic reading when it has one. -/
structure Version where
  text : String
  semver : Option Semver
  deriving DecidableEq, Repr

def indexChar (c : Char) : Bool :=
  c.isAlphanum || c == '.' || c == '+' || c == '-'

/-- Only such a version is indexed by name: it is used as a file name. -/
def Version.indexable (version : Version) : Bool :=
  version.text ≠ "" && version.text ≠ "." && version.text ≠ ".." && version.text.all indexChar

def Semver.below (a b : Semver) : Bool :=
  if a.major ≠ b.major then a.major < b.major
  else if a.minor ≠ b.minor then a.minor < b.minor
  else if a.patch ≠ b.patch then a.patch < b.patch
  else match a.pre, b.pre with
    | some _, none => true
    | some x, some y => x < y
    | none, _ => false

/-- Every semantic version is above every other version; the others are
ordered as text. -/
def Version.below (a b : Version) : Bool :=
  match a.semver, b.semver with
  | none, some _ => true
  | some _, none => false
  | some x, some y => x.below y
  | none, none => a.text < b.text

structure Pack where
  name : String
  version : Version
  deriving DecidableEq, Repr

structure State where
  /-- Archives present, readable or not. -/
  archives : List Pack
  /-- Archives present whose bytes no longer read as a pack. -/
  damaged : List Pack
  /-- Unpacked copies. -/
  unpacked : List Pack
  /-- Name-index entries. -/
  index : List Pack
  deriving DecidableEq, Repr

def State.empty : State := { archives := [], damaged := [], unpacked := [], index := [] }

inductive Action
  /-- Receive the pack, with or without naming its digest first. -/
  | receive
  /-- Receive the pack after asking for a different digest. -/
  | receiveExpectingAnother
  | openPack
  | release
  /-- The archive file disappears without the store being asked. -/
  | loseArchive
  /-- The archive file is overwritten with bytes that are not a pack. -/
  | damage
  deriving DecidableEq, Repr

inductive Result | stored | refused | opened | released | nothingToRelease | done
  deriving DecidableEq, Repr

structure Step where
  action : Action
  pack : Pack
  deriving DecidableEq, Repr

def step (state : State) (s : Step) : State × Result :=
  let pack := s.pack
  match s.action with
  | .receive =>
    ({ state with
        archives := state.archives.insert pack
        index := if pack.version.indexable then state.index.insert pack else state.index },
      .stored)
  | .receiveExpectingAnother => (state, .refused)
  | .openPack =>
    if pack ∈ state.unpacked then (state, .opened)
    else if pack ∈ state.archives ∧ pack ∉ state.damaged then
      ({ state with unpacked := state.unpacked.insert pack }, .opened)
    else (state, .refused)
  | .release =>
    ({ archives := state.archives.erase pack
       damaged := state.damaged.erase pack
       unpacked := state.unpacked.erase pack
       index := state.index.erase pack },
      if pack ∈ state.archives ∨ pack ∈ state.unpacked then .released else .nothingToRelease)
  | .loseArchive =>
    ({ state with archives := state.archives.erase pack, damaged := state.damaged.erase pack },
      .done)
  | .damage =>
    ({ state with
        damaged := if pack ∈ state.archives then state.damaged.insert pack else state.damaged },
      .done)

def run (state : State) : List Step → State × List Result
  | [] => (state, [])
  | s :: rest =>
    let stepped := step state s
    let ran := run stepped.1 rest
    (ran.1, stepped.2 :: ran.2)

def State.contains (state : State) (pack : Pack) : Bool := pack ∈ state.archives

/-- An exact lookup answers only for an indexed version whose archive is present. -/
def State.found (state : State) (pack : Pack) : Bool :=
  pack.version.indexable && pack ∈ state.index && pack ∈ state.archives

def State.versions (state : State) (name : String) : List Version :=
  ((state.index.filter fun pack => pack.name = name && pack ∈ state.archives).map (·.version)).mergeSort
    fun a b => !a.below b

/-- A lookup without a version answers the highest. -/
def State.latest (state : State) (name : String) : Option String :=
  (state.versions name).head?.map (·.text)

structure Listing where
  name : String
  versions : List String
  deriving DecidableEq, Repr

def State.names (state : State) : List Listing :=
  let names := (state.index.map (·.name)).eraseDups.mergeSort fun a b => a ≤ b
  (names.map fun name => { name, versions := (state.versions name).map (·.text) : Listing }).filter
    fun listing => !listing.versions.isEmpty

/-! ## Rules -/

/-- The archives are the truth: the name index never answers for a pack whose
archive is gone. -/
theorem found_needs_the_archive (state : State) (pack : Pack) (h : state.found pack = true) :
    state.contains pack = true := by
  simp only [State.found, Bool.and_eq_true, decide_eq_true_eq] at h
  simp [State.contains, h.2]

theorem refused_receive_stores_nothing (state : State) (pack : Pack) :
    (step state ⟨.receiveExpectingAnother, pack⟩).1 = state := rfl

theorem receive_stores (state : State) (pack : Pack) :
    (step state ⟨.receive, pack⟩).1.contains pack = true := by
  simp [step, State.contains]

theorem receive_indexes (state : State) (pack : Pack) (h : pack.version.indexable = true) :
    (step state ⟨.receive, pack⟩).1.found pack = true := by
  simp [step, State.found, h]

/-- Receiving a pack again changes nothing. -/
theorem receive_twice (state : State) (pack : Pack) :
    (step (step state ⟨.receive, pack⟩).1 ⟨.receive, pack⟩).1 = (step state ⟨.receive, pack⟩).1 := by
  simp only [step]
  split <;> simp [List.insert_of_mem]

/-- Nothing a store does indexes a version that is not indexable. -/
theorem step_indexes_only_indexable (state : State) (s : Step)
    (h : ∀ pack ∈ state.index, pack.version.indexable = true) :
    ∀ pack ∈ (step state s).1.index, pack.version.indexable = true := by
  intro pack hmem
  obtain ⟨action, target⟩ := s
  cases action <;> simp only [step] at hmem
  · split at hmem
    · rename_i hindexable
      rcases List.mem_insert_iff.mp hmem with rfl | hold
      · exact hindexable
      · exact h pack hold
    · exact h pack hmem
  · exact h pack hmem
  · split at hmem
    · exact h pack hmem
    · split at hmem <;> exact h pack hmem
  · exact h pack (List.mem_of_mem_erase hmem)
  · exact h pack hmem
  · exact h pack hmem

theorem run_indexes_only_indexable (steps : List Step) (state : State)
    (h : ∀ pack ∈ state.index, pack.version.indexable = true) :
    ∀ pack ∈ (run state steps).1.index, pack.version.indexable = true := by
  induction steps generalizing state with
  | nil => exact h
  | cons s rest ih => exact ih _ (step_indexes_only_indexable state s h)

/-- Releasing a pack whose archives and index hold it once leaves no trace of
it, whether or not its archive could still be read. -/
theorem release_forgets (state : State) (pack : Pack)
    (harchives : state.archives.Nodup) (hindex : state.index.Nodup) :
    (step state ⟨.release, pack⟩).1.contains pack = false
      ∧ pack ∉ (step state ⟨.release, pack⟩).1.index := by
  simp only [step, State.contains, decide_eq_false_iff_not]
  exact ⟨fun hmem => (List.Nodup.mem_erase_iff harchives).mp hmem |>.1 rfl,
    fun hmem => (List.Nodup.mem_erase_iff hindex).mp hmem |>.1 rfl⟩

/-- Releasing one pack leaves every other pack as it was. -/
theorem release_leaves_others (state : State) (pack other : Pack) (h : other ≠ pack) :
    (step state ⟨.release, pack⟩).1.contains other = state.contains other
      ∧ (step state ⟨.release, pack⟩).1.found other = state.found other := by
  simp [step, State.contains, State.found, List.mem_erase_of_ne h]

/-- A second release has nothing to release. -/
theorem release_twice (state : State) (pack : Pack)
    (harchives : state.archives.Nodup) (hunpacked : state.unpacked.Nodup) :
    (step (step state ⟨.release, pack⟩).1 ⟨.release, pack⟩).2 = .nothingToRelease := by
  simp only [step]
  rw [if_neg]
  intro hmem
  rcases hmem with hmem | hmem
  · exact (List.Nodup.mem_erase_iff harchives).mp hmem |>.1 rfl
  · exact (List.Nodup.mem_erase_iff hunpacked).mp hmem |>.1 rfl

/-- A pack whose archive was lost is still released from the index. -/
theorem lost_archive_release_cleans_index (state : State) (pack : Pack)
    (hindex : state.index.Nodup) :
    pack ∉ (step (step state ⟨.loseArchive, pack⟩).1 ⟨.release, pack⟩).1.index := by
  simp only [step]
  exact fun hmem => (List.Nodup.mem_erase_iff hindex).mp hmem |>.1 rfl

/-! ## Removing a pack

Removing a pack forgets its record, then hands the store the archives it used
(its own and its dependencies') as candidates. The store deletes a candidate
only when no remaining home record and no installation on the node still
references it: an archive another pack or a retained history entry names stays,
so a later reinstall or rollback finds it without the registry. -/

/-- Whether removal deletes this archive: it was a candidate and nothing that
remains references it. -/
def unreferenced (candidates referenced : List Pack) (pack : Pack) : Bool :=
  decide (pack ∈ candidates) && !decide (pack ∈ referenced)

/-- The store after removing a pack whose archives were `candidates`, while
`referenced` lists the archives the remaining records and installations name;
and the archives it released. Each released archive goes as `release` takes it:
with its unpacked copy and its index entry. -/
def State.removePack (state : State) (candidates referenced : List Pack) :
    State × List Pack :=
  let gone := unreferenced candidates referenced
  let keep := fun pack => !gone pack
  ({ archives := state.archives.filter keep
     damaged := state.damaged.filter keep
     unpacked := state.unpacked.filter keep
     index := state.index.filter keep },
    (state.archives ++ state.unpacked).dedup.filter gone)

/-- Removal deletes exactly the candidate archives nothing remaining
references, and leaves every other archive as it was. -/
theorem removal_deletes_exactly_the_unreferenced (state : State)
    (candidates referenced : List Pack) (pack : Pack) :
    (state.removePack candidates referenced).1.contains pack
      = (state.contains pack && !unreferenced candidates referenced pack) := by
  simp [State.removePack, State.contains, List.mem_filter]

/-- An archive a remaining record references survives the removal. -/
theorem removal_keeps_a_referenced_archive (state : State)
    (candidates referenced : List Pack) (pack : Pack) (h : pack ∈ referenced) :
    (state.removePack candidates referenced).1.contains pack = state.contains pack := by
  simp [removal_deletes_exactly_the_unreferenced, unreferenced, h]

/-- A released archive leaves no index entry behind. -/
theorem removal_unindexes_what_it_releases (state : State)
    (candidates referenced : List Pack) (pack : Pack)
    (h : unreferenced candidates referenced pack = true) :
    pack ∉ (state.removePack candidates referenced).1.index := by
  simp [State.removePack, List.mem_filter, h]

/-- The released list names exactly the present candidates nothing references. -/
theorem removal_reports_what_it_released (state : State)
    (candidates referenced : List Pack) (pack : Pack) :
    pack ∈ (state.removePack candidates referenced).2
      ↔ (pack ∈ state.archives ∨ pack ∈ state.unpacked)
        ∧ unreferenced candidates referenced pack = true := by
  simp [State.removePack, List.mem_filter, List.mem_dedup]

end Pack.Store
