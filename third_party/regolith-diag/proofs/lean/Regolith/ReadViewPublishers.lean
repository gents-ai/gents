/-!
# ReadViewPublishers: every publisher of the read view loses nothing

This file backs the TLA+ model `proofs/tla/ReadViewPublishers.tla`. TLC
checks there rotations, their flushes, a rollback, a compaction and an
ingest racing. Here the same rules are proved for every number of
publications.

## The story, for a reader who has never seen the code

Every read starts from the read view (`src/engine/read_view.rs`): the
memtable writers append to (the active one), the memtables waiting for a
flush (frozen, oldest first), and the tables of the current version. Data
lives in exactly those three places.

Several threads change the view: a rotation, in an ordered unit of the
commit pipeline, seals the active memtable behind a fresh one; a rollback,
after the log failed, replaces the active memtable by a copy without the
commits it discards; a flush (the compaction worker's, a writer's after its
commit, or a rotation's that has to make room first) installs a frozen
memtable's table and retires the memtable; a compaction and an ingest
install tables. Each builds the next view from the view it loaded and swaps
it in with one compare-and-swap (CAS): only if the view it built from is
still the published one. Otherwise it builds again on the newer view.

A rotation and a rollback both hold the pipeline's round, so they never run
at once; the version publishes run one at a time in the version store's
step. A rotation or a rollback can still race a version publish, and that is
what the CAS is for.

Tiny example of what the CAS prevents. The view is (active 2, frozen [1],
tables [1]) and a flush is about to retire memtable 1; it builds (active 2,
frozen [], tables [1]). A rotation then publishes (active 3, frozen [1, 2],
tables [1]). If the flush stores its view without the CAS, memtable 3, the
one writers are appending to, is in no place at all, and every write in it
is lost. With the CAS the flush's swap loses, and it retires memtable 1 from
the newer view instead.

## What is proved, in plain words

1. `cas_applies_to_current`: a swap that goes in has the same effect as
   applying its update to the view published now. So a run of CAS
   publications is a run of updates, each applied to its predecessor,
   which is what the steps below are.
2. `no_lost_memtable`: every memtable ever made is active, frozen, or has
   its table in the version, or a rollback replaced it by its copy, which is
   such a memtable (TLA+ `NoLostMemtable`).
3. `no_lost_ingest`: every ingested table is in the version (TLA+
   `NoLostIngest`).
4. `retired_stay_gone`: a memtable a flush retired is never active or
   frozen again (TLA+ `RetiredStayGone`).
5. `frozen_sorted`: the frozen list is in seal order (TLA+
   `FrozenInSealOrder`).
6. `replaced_stay_gone`: a memtable a rollback replaced is never active,
   frozen or flushed again (TLA+ `ReplacedStayGone`).
7. The RED cases, as counterexamples: a retire stored without the CAS loses
   a racing seal (`plain_retire_loses_a_seal`), and so does a compaction's
   version publish stored without it (`plain_version_loses_a_seal`); a
   rollback stored without it loses a racing ingest
   (`plain_rollback_loses_an_ingest`) and brings back a memtable a racing
   flush retired (`plain_rollback_brings_back_a_flushed_memtable`); a
   rotation that runs beside a rollback freezes the memtable the rollback
   meant to replace (`seal_beside_a_rollback_freezes_the_replaced_memtable`).
8. One publication per flush: the code installs a flushed memtable's table
   and retires the memtable in one view (`flushView`). Every such run is a
   run of the steps above (`real_reach_reach`), so items 2 to 6 hold for
   it, and in it no view ever holds a memtable and its table at once
   (`flush_keeps_sources_disjoint`, TLA+ `NoDoubleSource`). Publishing the
   two apart leaves a view that holds both
   (`split_install_holds_a_memtable_twice`), in which a read applied each
   of the memtable's merge operands twice.

## How to read the Lean

`def` defines a thing, `theorem` states a fact and its proof follows
`:= by`. Lines starting with `--` are comments, in plain words. A proof is a
list of *tactics*; each one changes the goal still to be shown, and the
comment above it says how. `simp` rewrites with known facts; `omega` solves
arithmetic over natural numbers; `cases` splits on the ways a fact could be
true; `induction` proves a fact for every number of steps by proving it for
none and then for one more.
-/

-- Everything below is named Regolith.ReadViewPublishers.<name>.
namespace Regolith.ReadViewPublishers

/-! ## Part 1. Views and the publications -/

/-- One read view. Memtables are named by number, in the order they were
made; a table of a flushed memtable carries the memtable's number. -/
structure View where
  /-- The memtable writers append to. -/
  active : Nat
  /-- The memtables waiting for a flush, oldest first. -/
  frozen : List Nat
  /-- The flushed memtables whose tables the version holds. -/
  tables : List Nat
  /-- The ingested tables the version holds. -/
  ingests : List Nat
  -- Two views can be compared for equality, which the CAS needs.
  deriving DecidableEq

/-- A rotation's update (`seal_active`): the active memtable becomes the
newest frozen one, and `fresh`, made before the publication, is active. -/
def rotate (fresh : Nat) (v : View) : View :=
  -- Active is the fresh memtable; the old active joins the end of frozen.
  { v with active := fresh, frozen := v.frozen ++ [v.active] }

/-- A rollback's update (`roll_back`): `fresh`, the copy made before the
publication, is active where the memtable it replaces, `old`, was. A view
whose active memtable is not `old` is left as it is. -/
def replaceActive (old fresh : Nat) (v : View) : View :=
  -- The copy in place of the replaced memtable; nothing else changes.
  { v with active := if v.active = old then fresh else v.active }

/-- A flush's install of memtable `m`'s table (a version publish). -/
def install (m : Nat) (v : View) : View :=
  -- The version holds `m`'s table now.
  { v with tables := m :: v.tables }

/-- A flush's retire of memtable `m` (`retire_memtable`), by identity. -/
def retire (m : Nat) (v : View) : View :=
  -- Every frozen memtable but `m`, in the same order.
  { v with frozen := v.frozen.filter (fun x => x != m) }

/-- An ingest's install of table `t` (a version publish). -/
def ingest (t : Nat) (v : View) : View :=
  -- The version holds the ingested table now.
  { v with ingests := t :: v.ingests }

/-- A compaction's install: its output holds the same data as its inputs,
so the places data lives in are the same. -/
def compact (v : View) : View :=
  -- Nothing a reader can tell apart changes.
  v

/-- The compare-and-swap of `ReadViewCell::publish`: the view built from
`base` goes in only if `base` is still the published view. -/
def cas (published base next : View) : View :=
  -- In when nothing was published since `base` was loaded, else no change.
  if published = base then next else published

/-- **`cas_applies_to_current`.** A swap that goes in has the effect of the
update applied to the view published now: what was loaded is what is there. -/
theorem cas_applies_to_current (published base : View) (upd : View → View)
    -- The compare-and-swap went in: the published view is the one that was loaded.
    (h : published = base) :
    -- The claim itself; its proof follows.
    cas published base (upd base) = upd published := by
  -- Replace `base` by `published`, then the `if` holds and gives the update.
  subst h; simp [cas]

/-- A swap that loses changes nothing: the publisher builds again on what
won. -/
theorem cas_out (published base next : View) (h : published ≠ base) :
    -- The claim itself; its proof follows.
    cas published base next = published := by
  -- The `if` fails by `h`.
  simp [cas, h]

/-! ## Part 2. The steps and what every reachable state keeps -/

/-- Everything at one moment: the published view, how many memtables exist
(numbered `1..made`), and three ghosts the rules talk about. -/
structure World where
  /-- The published view. -/
  view : View
  /-- Memtables `1..made` have been made. -/
  made : Nat
  /-- Every memtable a flush retired. -/
  retired : List Nat
  /-- Every table an ingest installed. -/
  ingested : List Nat
  /-- Every memtable a rollback replaced by its copy. -/
  replaced : List Nat

/-- The start: memtable 1 is active, nothing else exists. -/
def init : World :=
  -- Active 1, nothing frozen, no table, nothing retired, ingested or replaced.
  ⟨⟨1, [], [], []⟩, 1, [], [], []⟩

/-- The publications, each one CAS that went in, so each one's update
applied to the view published now (`cas_applies_to_current`). -/
inductive Step : World → World → Prop
  /-- A rotation: a fresh memtable, number `made + 1`, becomes active. -/
  | rotate (w : World) :
      -- The view sealed, and one more memtable exists.
      Step w { w with view := rotate (w.made + 1) w.view, made := w.made + 1 }
  /-- A rollback: its copy, number `made + 1`, replaces the active memtable.
  The round it holds keeps a rotation out between its reading which memtable
  is active and its publication, so the one it replaces is the active one. -/
  | rollback (w : World) :
      -- The copy is active, one more memtable exists, and the old one is noted.
      Step w { w with view := replaceActive w.view.active (w.made + 1) w.view,
                      made := w.made + 1, replaced := w.view.active :: w.replaced }
  /-- A flush installs the table of frozen memtable `m`. -/
  | install (w : World) (m : Nat) (h : m ∈ w.view.frozen) :
      -- The version holds `m`'s table.
      Step w { w with view := install m w.view }
  /-- A flush retires memtable `m`, whose table the version holds. -/
  | retire (w : World) (m : Nat) (h : m ∈ w.view.tables) :
      -- `m` leaves the frozen list, and is noted as retired.
      Step w { w with view := retire m w.view, retired := m :: w.retired }
  /-- An ingest installs table `t`. -/
  | ingest (w : World) (t : Nat) :
      -- The version holds `t`, and it is noted as ingested.
      Step w { w with view := ingest t w.view, ingested := t :: w.ingested }
  /-- A compaction installs its output. -/
  | compact (w : World) :
      -- The places data lives in are the same.
      Step w { w with view := compact w.view }

/-- The worlds the steps can reach from the start. -/
inductive Reach : World → Prop
  /-- The start is reachable. -/
  | init : Reach init
  /-- One step from a reachable world reaches another. -/
  | step {w u : World} : Reach w → Step w u → Reach u

/-- The facts every reachable world has. The rules follow from them. -/
structure Inv (w : World) : Prop where
  /-- At least one memtable exists. -/
  made_pos : 1 ≤ w.made
  /-- The active memtable is the newest one. -/
  active_newest : w.view.active = w.made
  /-- Every frozen memtable is older than the active one. -/
  frozen_old : ∀ m ∈ w.view.frozen, 1 ≤ m ∧ m < w.made
  /-- Every flushed memtable is older than the active one. -/
  tables_old : ∀ m ∈ w.view.tables, m < w.made
  /-- Every memtable made was replaced by its copy, or is active, frozen, or
  has its table installed. -/
  no_lost : ∀ m, 1 ≤ m → m ≤ w.made →
    m ∈ w.replaced ∨ m = w.view.active ∨ m ∈ w.view.frozen ∨ m ∈ w.view.tables
  /-- A retired memtable's table is installed. -/
  retired_in_tables : ∀ r ∈ w.retired, r ∈ w.view.tables
  /-- A retired memtable is not frozen. -/
  retired_gone : ∀ r ∈ w.retired, r ∉ w.view.frozen
  /-- Every ingested table is installed. -/
  ingests_kept : ∀ t ∈ w.ingested, t ∈ w.view.ingests
  /-- The frozen list is in the order the memtables were made. -/
  frozen_sorted : w.view.frozen.Pairwise (· < ·)
  /-- A replaced memtable is older than the active one, is not frozen and has
  no table. -/
  replaced_gone : ∀ r ∈ w.replaced, r < w.made ∧ r ∉ w.view.frozen ∧ r ∉ w.view.tables

/-- The start keeps every fact. -/
theorem inv_init : Inv init := by
  -- One field at a time; the start has one memtable and nothing else.
  refine ⟨?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_⟩
  -- One memtable exists.
  · simp [init]
  -- Memtable 1 is active and is the newest.
  · simp [init]
  -- Nothing is frozen.
  · intro m hm; simp [init] at hm
  -- Nothing is installed.
  · intro m hm; simp [init] at hm
  -- The only memtable, 1, is the active one.
  · intro m h1 h2; right; left; simp [init] at h2 ⊢; omega
  -- Nothing is retired.
  · intro r hr; simp [init] at hr
  -- Nothing is retired.
  · intro r hr; simp [init] at hr
  -- Nothing is ingested.
  · intro t ht; simp [init] at ht
  -- An empty list is in order.
  · simp [init]
  -- Nothing is replaced.
  · intro r hr; simp [init] at hr

/-- Every step keeps every fact. -/
theorem inv_step {w u : World} (hw : Inv w) (hst : Step w u) : Inv u := by
  -- One case per kind of publication.
  cases hst with
  -- Case: a rotation.
  | rotate =>
    -- The old active memtable is the newest one, number `made`.
    have hact := hw.active_newest
    -- Prove the facts one at a time.
    refine ⟨?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_⟩
    -- One more memtable exists.
    · simp only; omega
    -- The fresh one, `made + 1`, is active.
    · simp [rotate]
    -- Frozen: the old ones, older still, and the old active, `made`.
    · intro m hm
      -- A member of `l ++ [x]` is in `l` or is `x`.
      simp only [rotate, List.mem_append, List.mem_singleton] at hm
      -- Split on which.
      rcases hm with hm | rfl
      -- An old frozen memtable: older than `made`, so than `made + 1`.
      · have := hw.frozen_old m hm; simp only; omega
      -- The old active one: it is `made`, at least 1, below `made + 1`.
      · have := hw.made_pos; simp only; omega
    -- Tables: unchanged, and older than `made`, so than `made + 1`.
    · intro m hm; have := hw.tables_old m hm; simp only; omega
    -- Nothing lost: split on the memtable's number.
    · intro m h1 h2
      -- Split: is it the fresh one?
      by_cases hm : m = w.made + 1
      -- The fresh one is active.
      · right; left; simp [rotate, hm]
      -- Otherwise it is one of `1..made`, placed as before.
      · have hle : m ≤ w.made := by simp only at h2; omega
        -- Where it was before the seal.
        rcases hw.no_lost m h1 hle with h | h | h | h
        -- A replaced one is still noted as replaced.
        · left; exact h
        -- The old active one is now the newest frozen one.
        · right; right; left; simp [rotate, h]
        -- An old frozen one is still frozen.
        · right; right; left; simp [rotate, h]
        -- An installed one is still installed.
        · right; right; right; simpa [rotate] using h
    -- Retired ones keep their tables: tables are unchanged.
    · intro r hr; simpa [rotate] using hw.retired_in_tables r hr
    -- Retired ones are not frozen: not before, and not the old active.
    · intro r hr
      -- Its table is installed, so it is older than `made`, the old active.
      have hold := hw.tables_old r (hw.retired_in_tables r hr)
      -- Not in `frozen ++ [active]`.
      simp only [rotate, List.mem_append, List.mem_singleton, not_or]
      -- Not an old frozen one, and not the old active one (`made`).
      exact ⟨hw.retired_gone r hr, by omega⟩
    -- Ingests are unchanged.
    · intro t ht; simpa [rotate] using hw.ingests_kept t ht
    -- In order: the old list was, and every old frozen one is below `made`.
    · simp only [rotate, List.pairwise_append, List.pairwise_singleton, List.mem_singleton]
      -- The three parts: the old list, the one-element list, and between.
      refine ⟨hw.frozen_sorted, trivial, ?_⟩
      -- Each old frozen one is below the old active one, `made`.
      intro a ha b hb; subst hb; have := hw.frozen_old a ha; omega
    -- Replaced ones stay out: older than `made`, so not the memtable sealed
    -- now, and tables are unchanged.
    · intro r hr
      -- What was known of it before the seal.
      obtain ⟨hlt, hfr, htb⟩ := hw.replaced_gone r hr
      -- The three parts, one at a time.
      refine ⟨?_, ?_, ?_⟩
      -- Older than `made`, so than `made + 1`.
      · simp only; omega
      -- Not in `frozen ++ [active]`: not frozen before, and not `made`.
      · simp only [rotate, List.mem_append, List.mem_singleton, not_or]
        -- Not an old frozen one, and not the old active one (`made`).
        exact ⟨hfr, by omega⟩
      -- No table: tables are unchanged.
      · simpa [rotate] using htb
  -- Case: a rollback replaces the active memtable by its copy.
  | rollback =>
    -- The old active memtable is the newest one, number `made`.
    have hact := hw.active_newest
    -- Prove the facts one at a time.
    refine ⟨?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_⟩
    -- One more memtable exists.
    · simp only; omega
    -- The copy, `made + 1`, is active.
    · simp [replaceActive]
    -- Frozen is unchanged, and older than `made`, so than `made + 1`.
    · intro m hm
      -- It was frozen before the rollback.
      have := hw.frozen_old m (by simpa [replaceActive] using hm)
      -- So it is at least 1 and below `made + 1`.
      simp only; omega
    -- Tables are unchanged, and older than `made`, so than `made + 1`.
    · intro m hm
      -- Its table was installed before the rollback.
      have := hw.tables_old m (by simpa [replaceActive] using hm)
      -- So it is below `made + 1`.
      simp only; omega
    -- Nothing lost: split on the memtable's number.
    · intro m h1 h2
      -- Split: is it the copy?
      by_cases hm : m = w.made + 1
      -- The copy is active.
      · right; left; simp [replaceActive, hm]
      -- Otherwise it is one of `1..made`, placed as before.
      · have hle : m ≤ w.made := by simp only at h2; omega
        -- Where it was before the rollback.
        rcases hw.no_lost m h1 hle with h | h | h | h
        -- A replaced one is still noted as replaced.
        · left; simp [h]
        -- The old active one is the one replaced now.
        · left; simp [h]
        -- A frozen one is still frozen.
        · right; right; left; simpa [replaceActive] using h
        -- An installed one is still installed.
        · right; right; right; simpa [replaceActive] using h
    -- Retired ones keep their tables: tables are unchanged.
    · intro r hr; simpa [replaceActive] using hw.retired_in_tables r hr
    -- Retired ones are not frozen: frozen is unchanged.
    · intro r hr; simpa [replaceActive] using hw.retired_gone r hr
    -- Ingests are unchanged.
    · intro t ht; simpa [replaceActive] using hw.ingests_kept t ht
    -- Frozen, unchanged, is in order.
    · simpa [replaceActive] using hw.frozen_sorted
    -- Replaced ones stay out: the one replaced now, and the earlier ones.
    · intro r hr
      -- A member of `active :: l` is the old active one or in `l`.
      simp only [List.mem_cons] at hr
      -- Split on which.
      rcases hr with rfl | hr
      -- The old active one, number `made`: every frozen one and every table
      -- is older than it.
      · refine ⟨(by simp only; omega), ?_, ?_⟩
        -- Not frozen: a frozen one is below `made`.
        · intro hin
          -- If it were frozen it would be below `made`, which it is.
          have := hw.frozen_old w.view.active (by simpa [replaceActive] using hin)
          -- A number is not below itself.
          omega
        -- No table: a flushed one is below `made`.
        · intro hin
          -- If it had a table it would be below `made`, which it is.
          have := hw.tables_old w.view.active (by simpa [replaceActive] using hin)
          -- A number is not below itself.
          omega
      -- An earlier one: as before, with one more memtable made.
      · obtain ⟨hlt, hfr, htb⟩ := hw.replaced_gone r hr
        -- The three parts: older than `made + 1`, not frozen, no table.
        exact ⟨(by simp only; omega), by simpa [replaceActive] using hfr,
          by simpa [replaceActive] using htb⟩
  -- Case: a flush installs the table of frozen memtable m.
  | install m h =>
    -- `m` is frozen, so older than the active one.
    have hm := hw.frozen_old m h
    -- Prove the facts one at a time.
    refine ⟨hw.made_pos, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_⟩
    -- The active one is unchanged.
    · simpa [install] using hw.active_newest
    -- Frozen is unchanged.
    · simpa [install] using hw.frozen_old
    -- Tables: `m`, older than `made`, then the old ones.
    · intro x hx
      -- A member of `m :: l` is `m` or in `l`.
      simp only [install, List.mem_cons] at hx
      -- Split on which.
      rcases hx with rfl | hx
      -- `m`: older than `made`.
      · exact hm.2
      -- An old one: as before.
      · exact hw.tables_old x hx
    -- Nothing lost: every place grew or stayed.
    · intro x h1 h2
      -- Where it was before.
      rcases hw.no_lost x h1 h2 with h' | h' | h' | h'
      -- Still noted as replaced.
      · left; exact h'
      -- Still active.
      · right; left; simpa [install] using h'
      -- Still frozen.
      · right; right; left; simpa [install] using h'
      -- Still installed, behind `m`.
      · right; right; right; simp [install, h']
    -- Retired ones' tables: still installed.
    · intro r hr; simp [install, hw.retired_in_tables r hr]
    -- Retired ones: frozen is unchanged.
    · intro r hr; simpa [install] using hw.retired_gone r hr
    -- Ingests are unchanged.
    · intro t ht; simpa [install] using hw.ingests_kept t ht
    -- Frozen, unchanged, is in order.
    · simpa [install] using hw.frozen_sorted
    -- Replaced ones stay out: `m` is frozen, so it is none of them.
    · intro r hr
      -- What was known of it before the install.
      obtain ⟨hlt, hfr, htb⟩ := hw.replaced_gone r hr
      -- Older than the active one as before; then the two lists.
      refine ⟨hlt, ?_, ?_⟩
      -- Frozen is unchanged.
      · simpa [install] using hfr
      -- Not in `m :: tables`: it is not `m`, and it had no table.
      · simp only [install, List.mem_cons, not_or]
        -- If it were `m` it would be frozen, which it is not.
        exact ⟨fun hrm => hfr (by rw [hrm]; exact h), htb⟩
  -- Case: a flush retires memtable m.
  | retire m h =>
    -- `m`'s table is installed, so it is older than the active one.
    have hm := hw.tables_old m h
    -- Prove the facts one at a time.
    refine ⟨hw.made_pos, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_⟩
    -- The active one is unchanged.
    · simpa [retire] using hw.active_newest
    -- Frozen only lost `m`: every one left was frozen before.
    · intro x hx
      -- A member of the filtered list was in the list.
      simp only [retire, List.mem_filter] at hx
      -- As before.
      exact hw.frozen_old x hx.1
    -- Tables are unchanged.
    · simpa [retire] using hw.tables_old
    -- Nothing lost: a frozen one other than `m` stays frozen; `m` is installed.
    · intro x h1 h2
      -- Where it was before.
      rcases hw.no_lost x h1 h2 with h' | h' | h' | h'
      -- Still noted as replaced.
      · left; exact h'
      -- Still active.
      · right; left; simpa [retire] using h'
      -- Was frozen: split on whether it is `m`.
      · by_cases hx : x = m
        -- It is `m`: its table is installed.
        · right; right; right; subst hx; simpa [retire] using h
        -- It is not: still frozen.
        · right; right; left; simp [retire, List.mem_filter, h', hx]
      -- Still installed.
      · right; right; right; simpa [retire] using h'
    -- Retired ones' tables: `m`'s is installed, and the others as before.
    · intro r hr
      -- A member of `m :: l` is `m` or in `l`.
      simp only [List.mem_cons] at hr
      -- Split on which.
      rcases hr with rfl | hr
      -- `m`: installed.
      · simpa [retire] using h
      -- An old one: as before.
      · simpa [retire] using hw.retired_in_tables r hr
    -- Retired ones are not frozen: `m` was filtered out, the others were not
    -- frozen before.
    · intro r hr
      -- A member of `m :: l` is `m` or in `l`.
      simp only [List.mem_cons] at hr
      -- Not in the filtered list.
      simp only [retire, List.mem_filter, not_and]
      -- Split on which.
      rcases hr with rfl | hr
      -- `m`: the filter removed every `m`.
      · intro _; simp
      -- An old one: it was not frozen at all.
      · intro hin; exact absurd hin (hw.retired_gone r hr)
    -- Ingests are unchanged.
    · intro t ht; simpa [retire] using hw.ingests_kept t ht
    -- A filtered list in order stays in order.
    · simpa [retire] using hw.frozen_sorted.filter _
    -- Replaced ones stay out: frozen only lost `m`, and tables are unchanged.
    · intro r hr
      -- What was known of it before the retire.
      obtain ⟨hlt, hfr, htb⟩ := hw.replaced_gone r hr
      -- Older than the active one as before; then the two lists.
      refine ⟨hlt, ?_, ?_⟩
      -- Not in the filtered list: it was not in the list.
      · simp only [retire, List.mem_filter, not_and]
        -- It was not frozen at all.
        intro hin; exact absurd hin hfr
      -- Tables are unchanged.
      · simpa [retire] using htb
  -- Case: an ingest adds table t.
  | ingest t =>
    -- Prove the facts one at a time; only the ingests change.
    refine ⟨hw.made_pos, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_⟩
    -- The active one is unchanged.
    · simpa [ingest] using hw.active_newest
    -- Frozen is unchanged.
    · simpa [ingest] using hw.frozen_old
    -- Tables are unchanged.
    · simpa [ingest] using hw.tables_old
    -- Nothing lost: no memtable place changed.
    · intro x h1 h2; simpa [ingest] using hw.no_lost x h1 h2
    -- Retired ones' tables: unchanged.
    · intro r hr; simpa [ingest] using hw.retired_in_tables r hr
    -- Retired ones: frozen is unchanged.
    · intro r hr; simpa [ingest] using hw.retired_gone r hr
    -- Ingests: `t`, installed now, and the others as before.
    · intro x hx
      -- A member of `t :: l` is `t` or in `l`.
      simp only [List.mem_cons] at hx
      -- Split on which.
      rcases hx with rfl | hx
      -- `t`: installed by this step.
      · simp [ingest]
      -- An old one: as before.
      · simp [ingest, hw.ingests_kept x hx]
    -- Frozen, unchanged, is in order.
    · simpa [ingest] using hw.frozen_sorted
    -- Replaced ones: no memtable place changed.
    · intro r hr; simpa [ingest] using hw.replaced_gone r hr
  -- Case: a compaction.
  | compact =>
    -- Nothing changes: the view is the same.
    simpa [compact] using hw

/-- Every reachable world keeps every fact. -/
theorem inv_reach {w : World} (h : Reach w) : Inv w := by
  -- By how `w` was reached: the start, or one step from a reachable world.
  induction h with
  -- The start satisfies every fact.
  | init => exact inv_init
  -- One more step keeps them.
  | step _ hst ih => exact inv_step ih hst

/-! ## Part 3. The rules -/

/-- **`no_lost_memtable`.** Every memtable ever made is active, frozen, or
has its table in the version: no publication left one out. One a rollback
replaced is the exception, and its copy, a memtable made after it, is under
this same rule. -/
theorem no_lost_memtable {w : World} (h : Reach w) :
    -- The claim itself; its proof follows.
    ∀ m, 1 ≤ m → m ≤ w.made →
      m ∈ w.replaced ∨ m = w.view.active ∨ m ∈ w.view.frozen ∨ m ∈ w.view.tables :=
  -- One of the facts every reachable world keeps.
  (inv_reach h).no_lost

/-- **`no_lost_ingest`.** Every ingested table is in the version. -/
theorem no_lost_ingest {w : World} (h : Reach w) :
    -- The claim itself; its proof follows.
    ∀ t ∈ w.ingested, t ∈ w.view.ingests :=
  -- One of the facts every reachable world keeps.
  (inv_reach h).ingests_kept

/-- **`retired_stay_gone`.** A memtable a flush retired is never frozen or
active again, so it is never flushed twice. -/
theorem retired_stay_gone {w : World} (h : Reach w) :
    -- The claim itself; its proof follows.
    ∀ r ∈ w.retired, r ∉ w.view.frozen ∧ r ≠ w.view.active := by
  -- The facts every reachable world keeps.
  have hi := inv_reach h
  -- Take a retired memtable.
  intro r hr
  -- Its table is installed, so it is older than the active one.
  have := hi.tables_old r (hi.retired_in_tables r hr)
  -- Not frozen, and not the active one, which is the newest.
  exact ⟨hi.retired_gone r hr, by rw [hi.active_newest]; omega⟩

/-- **`frozen_sorted`.** The frozen list is in the order the memtables were
sealed, the order flushes install them in. -/
theorem frozen_sorted {w : World} (h : Reach w) :
    -- The claim itself; its proof follows.
    w.view.frozen.Pairwise (· < ·) :=
  -- One of the facts every reachable world keeps.
  (inv_reach h).frozen_sorted

/-- **`replaced_stay_gone`.** A memtable a rollback replaced is never
active, frozen or flushed again: the commits the rollback discarded, which
it still holds, reach no reader and no table. -/
theorem replaced_stay_gone {w : World} (h : Reach w) :
    -- The claim itself; its proof follows.
    ∀ r ∈ w.replaced, r ≠ w.view.active ∧ r ∉ w.view.frozen ∧ r ∉ w.view.tables := by
  -- The facts every reachable world keeps.
  have hi := inv_reach h
  -- Take a replaced memtable.
  intro r hr
  -- It is older than the newest memtable, is not frozen and has no table.
  obtain ⟨hlt, hfr, htb⟩ := hi.replaced_gone r hr
  -- Not the active one, which is the newest; the rest as just noted.
  exact ⟨(by rw [hi.active_newest]; omega), hfr, htb⟩

/-! ## Part 4. One publication per flush

The steps of Part 2 publish a flush's install and its retire apart, and
the facts hold for every interleaving of them. The code publishes the two
together (`VersionGuard::retire_with_version`): here that is one step, and
no view it makes holds a memtable and its table at once. -/

/-- A flush as the code publishes it: the table installed and the memtable
retired, in one view. -/
def flushView (m : Nat) (v : View) : View :=
  -- Install, then retire, applied to the same view, published once.
  retire m (install m v)

/-- The publications as the code makes them. -/
inductive RealStep : World → World → Prop
  /-- A rotation, as in Part 2. -/
  | rotate (w : World) :
      -- The view sealed, and one more memtable exists.
      RealStep w { w with view := rotate (w.made + 1) w.view, made := w.made + 1 }
  /-- A rollback, as in Part 2. -/
  | rollback (w : World) :
      -- The copy is active, one more memtable exists, and the old one is noted.
      RealStep w { w with view := replaceActive w.view.active (w.made + 1) w.view,
                          made := w.made + 1, replaced := w.view.active :: w.replaced }
  /-- A flush of frozen memtable `m`: one publication. -/
  | flush (w : World) (m : Nat) (h : m ∈ w.view.frozen) :
      -- The table in, the memtable out, and `m` noted as retired.
      RealStep w { w with view := flushView m w.view, retired := m :: w.retired }
  /-- An ingest, as in Part 2. -/
  | ingest (w : World) (t : Nat) :
      -- The version holds `t`, and it is noted as ingested.
      RealStep w { w with view := ingest t w.view, ingested := t :: w.ingested }
  /-- A compaction, as in Part 2. -/
  | compact (w : World) :
      -- The places data lives in are the same.
      RealStep w { w with view := compact w.view }

/-- The worlds the code's publications reach from the start. -/
inductive RealReach : World → Prop
  /-- The start is reachable. -/
  | init : RealReach init
  /-- One publication from a reachable world reaches another. -/
  | step {w u : World} : RealReach w → RealStep w u → RealReach u

/-- **`real_reach_reach`.** Every world the code reaches is one the steps of
Part 2 reach: a flush is an install followed by a retire. -/
theorem real_reach_reach {w : World} (h : RealReach w) : Reach w := by
  -- By how `w` was reached.
  induction h with
  -- The start is reachable in Part 2 as well.
  | init => exact Reach.init
  -- Case: the steps so far, then one more.
  | step _ hst ih =>
    -- One case per publication.
    cases hst with
    -- A rotation is a step of Part 2.
    | rotate => exact Reach.step ih (Step.rotate _)
    -- A rollback is a step of Part 2.
    | rollback => exact Reach.step ih (Step.rollback _)
    -- A flush: the install, then the retire, whose table is now installed.
    | flush m hm =>
      -- First the install.
      have h1 := Reach.step ih (Step.install _ m hm)
      -- Then the retire: `m`'s table is the first one in the list.
      exact Reach.step h1 (Step.retire _ m (by simp [install]))
    -- An ingest is a step of Part 2.
    | ingest t => exact Reach.step ih (Step.ingest _ t)
    -- A compaction is a step of Part 2.
    | compact => exact Reach.step ih (Step.compact _)

/-- **`flush_keeps_sources_disjoint`.** In every world the code reaches, no
frozen memtable has its table installed: no view holds a memtable's data
twice. Rules out a read applying a merge operand twice. -/
theorem flush_keeps_sources_disjoint {w : World} (h : RealReach w) :
    -- The claim itself; its proof follows.
    ∀ m ∈ w.view.frozen, m ∉ w.view.tables := by
  -- By how `w` was reached.
  induction h with
  -- Case: the start.
  | init =>
    -- Nothing is frozen at the start.
    intro m hm; simp [init] at hm
  -- Case: the steps so far, then one more, with the claim before the step (ih) known.
  | step hr hst ih =>
    -- The facts of Part 2 for the world before the step.
    have hi := inv_reach (real_reach_reach hr)
    -- One case per publication.
    cases hst with
    -- Case: a rotation.
    | rotate =>
      -- Take a frozen memtable of the new view.
      intro m hm
      -- It was frozen before, or it is the old active one.
      simp only [rotate, List.mem_append, List.mem_singleton] at hm ⊢
      -- Split on which.
      rcases hm with hm | hm
      -- Frozen before: its table was not installed then, and tables are
      -- unchanged.
      · exact ih m hm
      -- The old active one: every installed table is older than it.
      · intro ht; have := hi.tables_old m ht; rw [hm, hi.active_newest] at this; omega
    -- Case: a rollback.
    | rollback =>
      -- A rollback changes neither the frozen list nor the tables.
      simpa [replaceActive] using ih
    -- Case: a flush of frozen memtable f.
    | flush f hf =>
      -- Take a frozen memtable of the new view.
      intro m hm
      -- It survived the filter: it was frozen before and is not `f`.
      simp only [flushView, retire, install, List.mem_filter, bne_iff_ne, ne_eq] at hm ⊢
      -- Not `f`, and its table was not installed before.
      intro ht
      -- A member of `f :: l` is `f` or in `l`.
      simp only [List.mem_cons] at ht
      -- Split on which.
      rcases ht with ht | ht
      -- It is `f`: but the filter removed `f`.
      · exact hm.2 ht
      -- Installed before: but it was frozen before and not installed.
      · exact ih m hm.1 ht
    -- Case: an ingest.
    | ingest t =>
      -- Ingests do not touch memtables or their tables.
      simpa [ingest] using ih
    -- Case: a compaction.
    | compact =>
      -- Nothing changes.
      simpa [compact] using ih

/-- **`split_install_holds_a_memtable_twice`.** Publishing a flush's install
without its retire (bug `SplitRetire`): memtable 1, sealed behind 2, has its
table installed while it is still frozen. A view in that state holds its
data twice. -/
theorem split_install_holds_a_memtable_twice :
    -- Memtable 1 sealed, its table not installed yet.
    let sealed : View := ⟨2, [1], [], []⟩
    -- After the install alone, 1 is both frozen and installed.
    1 ∈ (install 1 sealed).frozen ∧ 1 ∈ (install 1 sealed).tables := by
  -- Both are read off the lists.
  simp [install]

/-! ## Part 5. The RED cases -/

/-- The view in the story: memtable 1 flushed and installed, 2 active. -/
def flushed : View := ⟨2, [1], [1], []⟩

/-- **`plain_retire_loses_a_seal`.** A flush built its retire of memtable 1
from `flushed`; a rotation then published its seal of 2 behind a fresh 3.
Stored without the CAS, the retire puts back active 2 and an empty frozen
list: memtable 3, which exists (three were made), is in no place. With the
CAS the retire's swap loses, and it retires 1 from the sealed view instead,
which keeps 3 active and 2 frozen (bug `PlainRetire`). -/
theorem plain_retire_loses_a_seal :
    -- What the rotation published, and what the plain store leaves.
    let sealed := rotate 3 flushed
    -- What the flush's plain store leaves: the view it built before the rotation, with memtable 1
    -- retired.
    let stored := retire 1 flushed
    -- The plain store loses memtable 3 ...
    ¬ (3 = stored.active ∨ 3 ∈ stored.frozen ∨ 3 ∈ stored.tables) ∧
    -- ... while the CAS refuses it, and the retire built again keeps 3.
    cas sealed flushed stored = sealed ∧
    (3 = (retire 1 sealed).active ∨ 3 ∈ (retire 1 sealed).frozen) := by
  -- Every part is a computation on small lists.
  refine ⟨?_, ?_, ?_⟩
  -- 3 is not 2, and the lists are [] and [1].
  · simp [retire, flushed]
  -- The views differ, so the swap loses.
  · simp [cas, rotate, flushed]
  -- The rebuilt retire keeps 3 active.
  · simp [retire, rotate, flushed]

/-- **`plain_version_loses_a_seal`.** A compaction built its version publish
from the start (active 1, nothing frozen); a rotation then published its
seal of 1 behind a fresh 2. Stored without the CAS, the compaction's view
puts back active 1: memtable 2, which exists, is in no place (bug
`PlainVersion`). -/
theorem plain_version_loses_a_seal :
    -- What the rotation published, and what the plain store leaves.
    let sealed := rotate 2 init.view
    -- What the compaction's plain store leaves: the view it built before the rotation.
    let stored := compact init.view
    -- The plain store loses memtable 2 ...
    ¬ (2 = stored.active ∨ 2 ∈ stored.frozen ∨ 2 ∈ stored.tables) ∧
    -- ... while the CAS refuses it.
    cas sealed init.view stored = sealed := by
  -- Both parts are computations on small lists.
  refine ⟨?_, ?_⟩
  -- 2 is not 1, and nothing is frozen or installed.
  · simp [compact, init]
  -- The views differ, so the swap loses.
  · simp [cas, rotate, init]

/-- The view in the story below: memtable 1 sealed behind 2, not flushed. -/
def sealedOnce : View := ⟨2, [1], [], []⟩

/-- **`plain_rollback_loses_an_ingest`.** A rollback built its view from the
start (active 1), with memtable 2 as its copy; an ingest then published
table 101. Stored without the CAS, the rollback's view puts back a version
without the table: the ingest's rows are in no view. With the CAS the swap
loses, and the rollback built again keeps the table and makes its copy
active (bug `PlainRollback`). -/
theorem plain_rollback_loses_an_ingest :
    -- What the ingest published while the rollback was at work.
    let installed := ingest 101 init.view
    -- What the rollback's plain store leaves: the view it built before the ingest.
    let stored := replaceActive 1 2 init.view
    -- The plain store loses the table ...
    101 ∉ stored.ingests ∧
    -- ... while the CAS refuses it ...
    cas installed init.view stored = installed ∧
    -- ... and the rollback built again keeps the table, its copy active.
    101 ∈ (replaceActive 1 2 installed).ingests ∧ (replaceActive 1 2 installed).active = 2 := by
  -- Every part is a computation on small lists.
  refine ⟨?_, ?_, ?_, ?_⟩
  -- The view built before the ingest has no ingested table.
  · simp [replaceActive, init]
  -- The views differ, so the swap loses.
  · simp [cas, ingest, init]
  -- The rebuilt view starts from the ingest's, so it holds the table.
  · simp [replaceActive, ingest, init]
  -- Memtable 1 is still active there, so the copy takes its place.
  · simp [replaceActive, ingest, init]

/-- **`plain_rollback_brings_back_a_flushed_memtable`.** A rollback built its
view from `sealedOnce` (memtable 1 frozen, 2 active), with memtable 3 as its
copy of 2; a flush then published table 1 and retired memtable 1 in one
view. Stored without the CAS, the rollback's view puts memtable 1 back in
the frozen list and takes its table out: the next flush writes it a second
time. With the CAS the swap loses, and the rollback built again leaves 1
retired and its table in (bug `PlainRollback`). -/
theorem plain_rollback_brings_back_a_flushed_memtable :
    -- What the flush published while the rollback was at work.
    let flushedOne := flushView 1 sealedOnce
    -- What the rollback's plain store leaves: the view it built before the flush.
    let stored := replaceActive 2 3 sealedOnce
    -- The plain store freezes memtable 1 again and drops its table ...
    1 ∈ stored.frozen ∧ 1 ∉ stored.tables ∧
    -- ... while the CAS refuses it ...
    cas flushedOne sealedOnce stored = flushedOne ∧
    -- ... and the rollback built again leaves 1 out of the frozen list, its table in.
    1 ∉ (replaceActive 2 3 flushedOne).frozen ∧ 1 ∈ (replaceActive 2 3 flushedOne).tables := by
  -- Every part is a computation on small lists.
  refine ⟨?_, ?_, ?_, ?_, ?_⟩
  -- The view built before the flush has memtable 1 frozen.
  · simp [replaceActive, sealedOnce]
  -- And no table.
  · simp [replaceActive, sealedOnce]
  -- The views differ, so the swap loses.
  · simp [cas, flushView, retire, install, sealedOnce]
  -- The rebuilt view starts from the flush's, where 1 is not frozen.
  · simp [replaceActive, flushView, retire, install, sealedOnce]
  -- And where its table is installed.
  · simp [replaceActive, flushView, retire, install, sealedOnce]

/-- **`seal_beside_a_rollback_freezes_the_replaced_memtable`.** Why a
rotation and a rollback take turns (bug `SealOutsideRound`). A rollback set
out to replace memtable 1 by its copy; a rotation that did not hold the
round published its seal of 1 behind a fresh 2. The rollback's swap loses;
it builds again on the sealed view, finds a memtable active that is not the
one it replaces, and leaves the view as it is. Memtable 1, with the
discarded commits in it, is frozen and waits for a flush. -/
theorem seal_beside_a_rollback_freezes_the_replaced_memtable :
    -- What the rotation published while the rollback was at work.
    let sealed := rotate 2 init.view
    -- The rollback's first build loses its swap ...
    cas sealed init.view (replaceActive 1 3 init.view) = sealed ∧
    -- ... its second build changes nothing ...
    replaceActive 1 3 sealed = sealed ∧
    -- ... and the memtable it set out to replace is frozen.
    1 ∈ sealed.frozen := by
  -- Every part is a computation on small lists.
  refine ⟨?_, ?_, ?_⟩
  -- The views differ, so the swap loses.
  · simp [cas, rotate, init]
  -- Memtable 2 is active, not 1, so the update keeps the view.
  · simp [replaceActive, rotate, init]
  -- The seal put memtable 1 at the end of the frozen list.
  · simp [rotate, init]

-- The end of this file's names.
end Regolith.ReadViewPublishers
