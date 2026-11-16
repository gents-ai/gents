import Lean

/-! Repair backups under `<home>/service-updates/repair-*`. Each service repair or repoint that
changes the service program takes one: a copy of the home's owned inventory and `prior-gents`, the
program the service ran, which the repair restores when it fails. Without a bound they accumulated
at about 743 MB each, mostly `prior-gents`. -/

namespace RepairBackups

/-- One `repair-*` directory. `id` names it; `program` is the recorded SHA-256 of its `prior-gents`
(an opaque number here), `none` when the service had no program file or the directory predates the
record. -/
structure Backup where
  id : Nat
  program : Option Nat
  deriving Repr, DecidableEq, Lean.ToJson

/-- How many repair backups a home keeps. One is the backup the running repair rolls back to; the
second covers a repair whose result turns out bad only after it finished (a repoint, then a repair
back); the third leaves one more manual recovery point after two quick repairs. Since equal
programs share their bytes, the bound costs about three copies of the home's inventory plus one
program per distinct build. -/
def limit : Nat := 3

/-- What pruning sees: `backups` newest first, its head the backup the running repair just took;
`inUse` the backup whose `prior-gents` the installed service definition runs, which a restore
leaves installed and which a later move carries to another home unchanged. -/
structure Retention where
  backups : List Backup
  inUse : Option Nat
  deriving Repr, DecidableEq, Lean.ToJson

/-- The newest `n` backups, and the backup in use when it is older than those. -/
def retainedWithin (n : Nat) (r : Retention) : List Backup :=
  r.backups.take n ++ ((r.backups.drop n).find? fun b => r.inUse == some b.id).toList

def retained (r : Retention) : List Backup := retainedWithin limit r

/-- The kept backup whose `prior-gents` a new backup of `program` hard-links instead of copying:
the newest one that recorded the same program. A backup without a recorded program shares
nothing. -/
def sharedWith (kept : List Backup) : Option Nat → Option Nat
  | none => none
  | some digest => (kept.find? fun b => b.program == some digest).map (·.id)

theorem limit_positive : 0 < limit := by decide

theorem retained_at_most (n : Nat) (r : Retention) : (retainedWithin n r).length ≤ n + 1 := by
  have newest := List.length_take_le n r.backups
  have older : (((r.backups.drop n).find? fun b => r.inUse == some b.id).toList).length ≤ 1 := by
    cases (r.backups.drop n).find? (fun b => r.inUse == some b.id) <;> simp
  simp only [retainedWithin, List.length_append]
  omega

/-- Only the backup in use, when it is older than the newest `n`, takes the bound past `n`. -/
theorem retained_at_most_limit_without_in_use (n : Nat) (r : Retention) (h : r.inUse = none) :
    (retainedWithin n r).length ≤ n := by
  have absent : ((r.backups.drop n).find? fun b => r.inUse == some b.id) = none :=
    List.find?_eq_none.mpr fun b _ => by simp [h]
  simp only [retainedWithin, absent, Option.toList, List.append_nil]
  exact List.length_take_le n r.backups

theorem retained_keeps_newest (n : Nat) (r : Retention) :
    r.backups.take n <+: retainedWithin n r :=
  List.prefix_append _ _

theorem retained_keeps_current (n : Nat) (r : Retention) (b : Backup) (positive : 0 < n)
    (current : r.backups.head? = some b) : b ∈ retainedWithin n r := by
  obtain ⟨m, rfl⟩ : ∃ m, n = m + 1 := ⟨n - 1, by omega⟩
  unfold retainedWithin
  apply List.mem_append_left
  cases backups : r.backups with
  | nil => simp [backups] at current
  | cons c rest =>
    simp [backups] at current
    subst current
    simp

theorem retained_keeps_in_use (n : Nat) (r : Retention) (b : Backup) (mem : b ∈ r.backups)
    (inUse : r.inUse = some b.id) : ∃ c ∈ retainedWithin n r, c.id = b.id := by
  have split : b ∈ r.backups.take n ++ r.backups.drop n := by
    rw [List.take_append_drop]; exact mem
  unfold retainedWithin
  rcases List.mem_append.mp split with newer | older
  · exact ⟨b, List.mem_append_left _ newer, rfl⟩
  · cases found : (r.backups.drop n).find? (fun c => r.inUse == some c.id) with
    | none =>
      have := List.find?_eq_none.mp found b older
      simp [inUse] at this
    | some c =>
      refine ⟨c, List.mem_append_right _ (by simp), ?_⟩
      have := List.find?_some found
      simp [inUse] at this
      omega

theorem retained_sub (n : Nat) (r : Retention) (b : Backup) (h : b ∈ retainedWithin n r) :
    b ∈ r.backups := by
  unfold retainedWithin at h
  rcases List.mem_append.mp h with newer | older
  · exact List.mem_of_mem_take newer
  · cases found : (r.backups.drop n).find? (fun c => r.inUse == some c.id) with
    | none => simp [found] at older
    | some c =>
      simp [found] at older
      rw [older]
      exact List.mem_of_mem_drop (List.mem_of_find?_eq_some found)

theorem sharedWith_names_same_program (kept : List Backup) (digest k : Nat)
    (h : sharedWith kept (some digest) = some k) :
    ∃ b ∈ kept, b.id = k ∧ b.program = some digest := by
  simp only [sharedWith] at h
  cases found : kept.find? (fun b => b.program == some digest) with
  | none => simp [found] at h
  | some b =>
    simp [found] at h
    refine ⟨b, List.mem_of_find?_eq_some found, h, ?_⟩
    have := List.find?_some found
    simpa using this

/-- The files under `service-updates`: each backup's `prior-gents` is a link naming an inode, and
an inode's bytes have a digest. An inode lives while any link names it, the hard-link reference
count; removing a directory removes its links only. -/
structure Disk where
  links : List (Nat × Nat)
  inodes : List (Nat × Nat)

def find (key : Nat) : List (Nat × Nat) → Option Nat
  | [] => none
  | (k, v) :: rest => if k = key then some v else find key rest

/-- The digest of the program a backup's `prior-gents` resolves to. -/
def Disk.resolve (d : Disk) (backup : Nat) : Option Nat :=
  match find backup d.links with
  | none => none
  | some inode => find inode d.inodes

def Disk.link (d : Disk) (backup inode : Nat) : Disk :=
  { d with links := (backup, inode) :: d.links }

def Disk.copy (d : Disk) (backup digest : Nat) : Disk :=
  { links := (backup, backup) :: d.links, inodes := (backup, digest) :: d.inodes }

/-- Taking `new`: link the inode of the kept backup `sharedWith` names, else copy the program into
a new inode. -/
def Disk.take (d : Disk) (kept : List Backup) (new : Backup) : Disk :=
  match new.program with
  | none => d
  | some digest =>
    match (sharedWith kept (some digest)).bind fun k => find k d.links with
    | some inode => d.link new.id inode
    | none => d.copy new.id digest

/-- Removing every backup directory `keep` rejects, and with it every inode no remaining link
names. -/
def Disk.prune (d : Disk) (keep : Nat → Bool) : Disk where
  links := d.links.filter fun l => keep l.1
  inodes := d.inodes.filter fun i => (d.links.filter fun l => keep l.1).any fun l => l.2 == i.1

/-- Every kept backup with a recorded program resolves to a program of that digest. -/
def Valid (d : Disk) (kept : List Backup) : Prop :=
  ∀ b ∈ kept, ∀ x, b.program = some x → d.resolve b.id = some x

theorem find_filter (key : Nat) (f : Nat × Nat → Bool) (keeps : ∀ v, f (key, v) = true)
    (l : List (Nat × Nat)) : find key (l.filter f) = find key l := by
  induction l with
  | nil => simp [find]
  | cons head rest ih =>
    obtain ⟨k, v⟩ := head
    by_cases same : k = key
    · subst same
      simp [List.filter_cons, keeps, find]
    · by_cases kept : f (k, v) = true
      · simp [List.filter_cons, kept, find, same, ih]
      · simp [List.filter_cons, kept, find, same, ih]

theorem find_mem (key v : Nat) (l : List (Nat × Nat)) (h : find key l = some v) :
    (key, v) ∈ l := by
  induction l with
  | nil => simp [find] at h
  | cons head rest ih =>
    obtain ⟨k, w⟩ := head
    by_cases same : k = key
    · subst same
      simp [find] at h
      subst h
      simp
    · simp [find, same] at h
      exact List.mem_cons_of_mem _ (ih h)

theorem prune_resolves (d : Disk) (keep : Nat → Bool) (b x : Nat) (kept : keep b = true)
    (h : d.resolve b = some x) : (d.prune keep).resolve b = some x := by
  unfold Disk.resolve at h
  cases found : find b d.links with
  | none => simp [found] at h
  | some inode =>
    simp only [found] at h
    have links : find b (d.links.filter fun l => keep l.1) = some inode := by
      rw [find_filter b (fun l => keep l.1) (fun _ => kept)]; exact found
    have live : (d.links.filter fun l => keep l.1).any (fun l => l.2 == inode) = true :=
      List.any_eq_true.mpr ⟨(b, inode), find_mem b inode _ links, by simp⟩
    have inodes : find inode (d.inodes.filter fun i =>
        (d.links.filter fun l => keep l.1).any fun l => l.2 == i.1) = find inode d.inodes :=
      find_filter inode (fun i => (d.links.filter fun l => keep l.1).any fun l => l.2 == i.1)
        (fun _ => live) d.inodes
    simpa only [Disk.resolve, Disk.prune, links, inodes] using h

theorem link_resolves_new (d : Disk) (b inode : Nat) :
    (d.link b inode).resolve b = find inode d.inodes := by
  simp [Disk.link, Disk.resolve, find]

theorem link_keeps (d : Disk) (b inode other : Nat) (ne : other ≠ b) :
    (d.link b inode).resolve other = d.resolve other := by
  simp [Disk.link, Disk.resolve, find, Ne.symm ne]

theorem copy_resolves_new (d : Disk) (b digest : Nat) :
    (d.copy b digest).resolve b = some digest := by
  simp [Disk.copy, Disk.resolve, find]

theorem copy_keeps (d : Disk) (b digest other x : Nat) (ne : other ≠ b)
    (fresh : ∀ v, (b, v) ∉ d.inodes) (h : d.resolve other = some x) :
    (d.copy b digest).resolve other = some x := by
  unfold Disk.resolve at h
  cases found : find other d.links with
  | none => simp [found] at h
  | some inode =>
    simp only [found] at h
    have distinct : b ≠ inode := by
      intro same
      subst same
      exact fresh x (find_mem _ _ _ h)
    simp [Disk.copy, Disk.resolve, find, Ne.symm ne, found, distinct, h]

/-- A new backup resolves to the program it recorded, whether linked or copied: `sharedWith` only
names a kept backup of the same program. -/
theorem take_resolves_new (d : Disk) (kept : List Backup) (new : Backup) (valid : Valid d kept)
    (unlinked : find new.id d.links = none) :
    (d.take kept new).resolve new.id = new.program := by
  unfold Disk.take
  split
  next none_program =>
    simp [Disk.resolve, unlinked, none_program]
  next digest some_program =>
    split
    next inode shared =>
      cases sw : sharedWith kept (some digest) with
      | none => simp [sw] at shared
      | some k =>
        simp [sw] at shared
        obtain ⟨b, mem, named, same⟩ := sharedWith_names_same_program kept digest k sw
        have resolved := valid b mem digest same
        rw [named] at resolved
        unfold Disk.resolve at resolved
        simp only [shared] at resolved
        simpa [link_resolves_new, some_program] using resolved
    next => simpa [some_program] using copy_resolves_new d new.id digest

theorem take_keeps (d : Disk) (kept : List Backup) (new : Backup) (other x : Nat)
    (ne : other ≠ new.id) (fresh : ∀ v, (new.id, v) ∉ d.inodes) (h : d.resolve other = some x) :
    (d.take kept new).resolve other = some x := by
  unfold Disk.take
  split
  next => exact h
  next digest _ =>
    split
    next inode _ =>
      rw [link_keeps d new.id inode other ne]
      exact h
    next => exact copy_keeps d new.id digest other x ne fresh h

/-- One repair: take `new`, then prune to `retained`. Every backup pruning keeps still resolves to
the program it recorded, so the restore path finds a valid program for each; a program another
kept backup links is never removed with the directory that first copied it. -/
theorem repair_keeps_programs (d : Disk) (new : Backup) (kept : List Backup) (r : Retention)
    (backups : r.backups = new :: kept)
    (valid : Valid d kept)
    (unlinked : find new.id d.links = none)
    (fresh : ∀ v, (new.id, v) ∉ d.inodes)
    (distinct : ∀ b ∈ kept, b.id ≠ new.id) :
    Valid ((d.take kept new).prune fun id => (retained r).any fun b => b.id == id) (retained r) := by
  unfold Valid
  intro b mem x program
  have keeps : (fun id => (retained r).any fun c => c.id == id) b.id = true :=
    List.any_eq_true.mpr ⟨b, mem, by simp⟩
  refine prune_resolves _ (fun id => (retained r).any fun c => c.id == id) b.id x keeps ?_
  have source := retained_sub limit r b mem
  rw [backups] at source
  rcases List.mem_cons.mp source with same | older
  · rw [same, take_resolves_new d kept new valid unlinked, ← same]
    exact program
  · exact take_keeps d kept new b.id x (distinct b older) fresh (valid b older x program)

/-- The backup the running repair just took survives its own pruning, so a failed repair can
still roll back to it. -/
theorem repair_keeps_current (new : Backup) (kept : List Backup) (inUse : Option Nat) :
    new ∈ retained ⟨new :: kept, inUse⟩ :=
  retained_keeps_current limit _ new limit_positive rfl

end RepairBackups
