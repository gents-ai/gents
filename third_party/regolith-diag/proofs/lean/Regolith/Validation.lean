-- The merge operator's laws (touches, parts) come from MergeOperator.lean.
import Regolith.MergeOperator

/-!
# Validation: every key class's commit rule gives a serial history (plan 3.1 to 3.15)

This file backs the TLA+ model `proofs/tla/RepeatableRead.tla`, whose commit
rule is in `RepeatableReadCommit.tla` (`KeptReads`, `ReadDecider`,
`MergeDecider`, `ScanDecider`, `CaExempt`, `ValidatesNothing`). TLC checks
the rules over every interleaving of a few transactions, with invariants
about what each workload decided (`INV_NoStaleDefinition`,
`INV_PartsCurrent`, `INV_ValueReadsCurrent`, `INV_ScanDecisionsCurrent`,
`INV_HoldsCurrent`, `INV_NoDanglingReference`, `INV_WriteFreeConsistent`,
`INV_LogDense`, `INV_CounterExact`). The theorems here hold for every number
of transactions, keys and writes.

## The story, for a reader who has never seen the code

Transactions run at the same time, each reading from a frozen picture of
the database (its snapshot). When one asks to commit, the engine checks the
keys it read against the writes that committed since that snapshot
(`RegolithEngine::validate`, src/engine/commit/txn.rs). If the check passes,
its writes are applied; if not, it is refused.

Tiny example: two transactions read a counter at 5 and both want to write 6.
Run one after the other, the second would read 6 and write 7. So letting
both commit loses an increment: no one-at-a-time order gives that result.
The check refuses the second, because the key it read has a newer write.

What could go wrong is a check that lets through a commit no one-at-a-time
order explains. What is proved here: every check the engine uses (one per
class of read) lets through only commits that give exactly the state the
same transactions would reach run one at a time, in the order they
committed.

## The model

A state maps every key to a value; both are natural numbers and 0 is an
absent key. A transaction reads a snapshot: the state after the commits it
saw. It decides its writes from what it read, and its writes are applied at
its commit, in commit order, on whatever the state is then: a put stores a
value, a merge folds an operand onto the current value, an append takes the
log's next position. That is how the engine applies a commit.

"The history is serial in commit order" means: the state after every commit
equals the state the same transactions reach run one at a time in commit
order, each reading the state just before its own commit (`serial`). A
history keeps that when every transaction's decision at its snapshot has the
same effect as its decision at its commit point would have (`lift`).

A transaction's reads are recorded by class (`Read`), each with the rule the
commit checks against the writes committed since its snapshot (`Ok`) and
what the rule guarantees about the read between the snapshot and the commit
(`Agree`):

* `full`: validated by sequence, as every level validates a read: no newer
  write of the key. The key's value is unchanged.
* `value`: DefraLevel (plan 3.5): the newest newer write of the key is a put
  of exactly the value read, or a delete when it read nothing. The value is
  unchanged; an identical rewrite (ABA) is not a change.
* `presence`: a content-addressed key that held a value (E17): the newest
  newer write is not a removal. Only presence is guaranteed: the class's
  relaxation is that the bytes never differ, which the caller promises.
* `parts`: a projected read (plan 3.4): walking the newer writes newest
  first, the first that changes a named part (a put, a delete, a range
  delete, or an operand `touches` says touches the parts) must be a put of
  exactly the value read. Existence and the named parts are unchanged.
* `scan`: a validated scan (plan 3.15): no newer write inside the range.
  Every key of the range is unchanged, so no key left and none appeared.
* `exempt`: never validated: a Log read, a scan stretch inside a commutative
  prefix, `get_parts` with no part. Nothing is guaranteed; the caller must
  not decide on it.

The caller's side is `Contract`: a transaction's writes depend only on what
its validated reads guarantee. Content-addressed creates are the one
exception the classes allow: a transaction may put a content-addressed key
because it read it as absent, and that read is dropped (D7). The class's
relaxation makes that safe: the key's bytes are fixed (`Env.bytes`), so a
put of them over a key that holds them changes nothing.

Choices this model makes, each recorded here because the plan leaves it
open:

* The fallbacks of a projected read (in full when it found nothing or when
  the transaction puts the key) are which `Read` the engine records; the
  theorems hold for whichever it records, as long as the decision uses only
  what that read's `Agree` covers. A put derived from the read uses every
  part, which is why the engine must record it in full.
* Write-write checks (first committer wins, the validated content-addressed
  delete, the blind-merge rule) are not needed for the equivalence proved
  here: a blind write applies at commit, which is its serial position. They
  protect callers' protocols, which the TLA+ model checks
  (`INV_NoDanglingReference`, `INV_CounterExact`), and the blind-merge rule
  is what lets a merge also be placed at its snapshot
  (`Relaxations.lean`, `blindMerge_commutes`).

## What is proved, in plain words

1. One theorem per class: if every committed transaction's reads are of
   that class and passed its rule, the history is serial in commit order:
   `ordinary_serial`, `value_serial`, `contentAddressed_serial`,
   `parts_serial`, `scan_serial`, `exempt_serial` (Log reads, commutative
   stretches, empty parts) and `blindMerge_serial`. Each rests on its rule's
   soundness lemma (`full_current`, `value_current`, `presence_current`,
   `parts_current`, `scan_current`).
2. `all_classes_serial`: the same for any mix of classes, creates included.
3. Content-addressed puts are idempotent: `ca_put_idempotent`, and
   `ca_commit_point`, the step `contentAddressed_serial` rests on.

`Regolith/Relaxations.lean` continues this file with every other class's
relaxation stated exactly (blind merges commute, Log appends are
commit-ordered, an identical rewrite is not a change, commutative writers
leave the union, a write-free transaction reads one point in time) and with
the RED cases as counterexamples.

How to read the Lean: `def` defines a function or a property, `structure`
groups named fields, `inductive` lists the forms a value can take, and
`theorem` states a fact whose proof follows `:= by`. Lines starting with
`--` are comments; each proof step is commented.
-/

-- Everything below is named Regolith.Validation.<name>.
namespace Regolith.Validation

-- Use the merge operator's names (Operator, Parts, TouchesSound) without their prefix.
open Regolith.MergeOperator

/-! ## States, writes and the environment -/

/-- A database state: the value of every key. Keys and values are natural
numbers, and value 0 is an absent key. -/
abbrev State := Nat → Nat

/-- What the classes and the operator are, fixed for a database. -/
structure Env where
  /-- The merge operator: `op.apply v o` folds operand `o` onto value `v`. -/
  op : Operator Nat Nat
  /-- An operand always leaves a value: a merge never deletes. -/
  apply_ne_zero : ∀ v o, op.apply v o ≠ 0
  /-- The parts of a value and the operator's `touches`. -/
  parts : Parts Nat Nat Nat Nat
  /-- `touches` keeps its contract (MergeOperator.lean). -/
  touches_sound : TouchesSound op parts
  /-- The log's head key: it holds the newest assigned position. -/
  head : Nat
  /-- The key of the log entry at position `p`. -/
  entry : Nat → Nat
  /-- The keys the classifier declares Log. -/
  isLog : Nat → Bool
  /-- The head is a Log key. -/
  isLog_head : isLog head = true
  /-- Every entry is a Log key. -/
  isLog_entry : ∀ p, isLog (entry p) = true
  /-- Two positions never share a key. -/
  entry_inj : ∀ p q, entry p = entry q → p = q
  /-- No entry is the head. -/
  entry_ne_head : ∀ p, entry p ≠ head
  /-- The keys the classifier declares content-addressed. -/
  isCA : Nat → Bool
  /-- A key has one class. -/
  ca_not_log : ∀ k, isCA k = true → isLog k = false
  /-- The bytes a content-addressed key determines. -/
  bytes : Nat → Nat
  /-- Those bytes are a value, never absent. -/
  bytes_ne_zero : ∀ k, bytes k ≠ 0
  /-- The state before any commit. -/
  init : State

/-- One write of a commit. -/
inductive Write where
  /-- Store value `v` under key `k`. -/
  | put (k v : Nat)
  /-- Remove key `k`. -/
  | del (k : Nat)
  /-- Fold operand `o` onto key `k`'s value. -/
  | merge (k o : Nat)
  /-- Remove every key `j` with `lo ≤ j < hi` (`Db::delete_range`). -/
  | rangeDel (lo hi : Nat)
  /-- Append entry `e` to the log at the next position (plan 3.6). -/
  | append (e : Nat)

/-- State `σ` with key `k` set to `v`. -/
def upd (σ : State) (k v : Nat) : State := fun j => if j = k then v else σ j

/-- One write applied to a state, at commit. An append reads the head the
state holds now: that is where the ordered step assigns positions. -/
def applyWrite (E : Env) (σ : State) : Write → State
  -- A put: key k now holds v.
  | .put k v => upd σ k v
  -- A delete: key k now holds 0, which means absent.
  | .del k => upd σ k 0
  -- A merge: key k now holds the operator's fold of the operand onto what it held.
  | .merge k o => upd σ k (E.op.apply (σ k) o)
  -- A range delete: every key from lo up to, not including, hi becomes absent; the others stay.
  | .rangeDel lo hi => fun j => if lo ≤ j ∧ j < hi then 0 else σ j
  -- An append: the next position is the head plus one; the entry key of that position gets e, and
  -- the head moves up by one.
  | .append e => upd (upd σ (E.entry (σ E.head + 1)) e) E.head (σ E.head + 1)

/-- A list of writes applied in order: a commit in operation order, or many
commits in commit order. -/
def applyWrites (E : Env) (σ : State) (ws : List Write) : State :=
  -- Fold the writes over the state from the left: first write first.
  ws.foldl (applyWrite E) σ

/-- No writes leave the state. -/
theorem applyWrites_nil (E : Env) (σ : State) : applyWrites E σ [] = σ := rfl

/-- The first write, then the rest. -/
theorem applyWrites_cons (E : Env) (σ : State) (w : Write) (ws : List Write) :
    -- The statement: applying w :: ws is applying ws to the state w left. `rfl`: both sides are the
    -- same once the definition is unfolded.
    applyWrites E σ (w :: ws) = applyWrites E (applyWrite E σ w) ws := rfl

/-- Whether a write can change key `k`. An append can change any Log key,
since its position depends on the head at commit. -/
def affects (E : Env) (k : Nat) : Write → Bool
  -- A put of j changes k only when j is k.
  | .put j _ => decide (j = k)
  -- The same for a delete.
  | .del j => decide (j = k)
  -- The same for a merge.
  | .merge j _ => decide (j = k)
  -- A range delete changes k when k lies in its range.
  | .rangeDel lo hi => decide (lo ≤ k ∧ k < hi)
  -- An append changes k when k is a Log key.
  | .append _ => E.isLog k

/-- A write that cannot change `k` leaves it as it was. -/
theorem applyWrite_unaffected (E : Env) (σ : State) (w : Write) (k : Nat)
    -- `w` does not affect `k`.
    (h : affects E k w = false) :
    -- The statement: the state after w holds at k what it held before.
    applyWrite E σ w k = σ k := by
  -- Split on the kind of write.
  cases w with
  -- Case: a put of key j.
  | put j v =>
    -- `j ≠ k`, so the update is elsewhere.
    -- Turn the assumption "does not affect k" into the plain fact j ≠ k.
    simp only [affects, decide_eq_false_iff_not] at h
    -- Unfold the write: the update is at j, and j is not k, so the goal closes.
    simp [applyWrite, upd, Ne.symm h]
  -- Case: a delete of key j.
  | del j =>
    -- The assumption becomes j ≠ k.
    simp only [affects, decide_eq_false_iff_not] at h
    -- The update is at j, not at k: the goal closes.
    simp [applyWrite, upd, Ne.symm h]
  -- Case: a merge into key j.
  | merge j o =>
    -- The assumption becomes j ≠ k.
    simp only [affects, decide_eq_false_iff_not] at h
    -- The update is at j, not at k: the goal closes.
    simp [applyWrite, upd, Ne.symm h]
  -- Case: a range delete.
  | rangeDel lo hi =>
    -- `k` lies outside the range.
    -- The assumption becomes: k is not inside the range.
    simp only [affects, decide_eq_false_iff_not] at h
    -- So the range delete takes the branch that leaves k alone.
    simp [applyWrite, h]
  -- Case: an append.
  | append e =>
    -- `k` is not a Log key, so it is neither the head nor an entry.
    simp only [affects] at h
    -- k is not the head: the head is a Log key and k is not.
    have h1 : k ≠ E.head := fun hk => by rw [hk, E.isLog_head] at h; cases h
    -- k is not the new entry's key either, for the same reason:
    have h2 : k ≠ E.entry (σ E.head + 1) := fun hk => by
      -- were it that key, the assumption would say a Log key is not a Log key.
      rw [hk, E.isLog_entry] at h; cases h
    -- Both updates of the append miss k: the goal closes.
    simp [applyWrite, upd, h1, h2]

/-- Writes that cannot change `k` leave it as it was. -/
theorem applyWrites_unaffected (E : Env) (k : Nat) :
    -- The statement: for every list of writes and every state, if no write affects k, then k holds
    -- after them what it held before.
    ∀ (ws : List Write) (σ : State), (∀ w ∈ ws, affects E k w = false) →
      applyWrites E σ ws k = σ k
  -- No writes: nothing changed.
  | [], _, _ => rfl
  -- A first write w and the rest ws.
  | w :: ws, σ, h => by
    -- The first write leaves `k`; then the rest do.
    simp only [applyWrites_cons] at *
    -- Rewrite the rest with this same theorem on the shorter list ws (no write of ws affects k).
    rw [applyWrites_unaffected E k ws _ (fun w' hw' => h w' (List.mem_cons_of_mem _ hw'))]
    -- What remains is the first write leaving k, which is the lemma above.
    exact applyWrite_unaffected E σ w k (h w List.mem_cons_self)

/-- Applying two lists in turn is applying them joined. -/
theorem applyWrites_append (E : Env) (σ : State) (a b : List Write) :
    -- The statement: the writes a ++ b applied equal b applied after a.
    applyWrites E σ (a ++ b) = applyWrites E (applyWrites E σ a) b := by
  -- Unfold to the library's fold, which splits over an appended list.
  simp [applyWrites, List.foldl_append]

/-! ## The newest write that matters, newest first -/

/-- The newest write of `ws` that satisfies `q`, if any: the first one a walk
from the newest end meets. -/
def lastWhere (q : Write → Bool) : List Write → Option Write
  -- No writes: nothing found.
  | [] => none
  -- A write w with the writes ws after it (the list is oldest first, so ws is newer).
  | w :: ws =>
    -- Look in the newer writes first.
    match lastWhere q ws with
    -- Found there: that is the answer.
    | some x => some x
    -- Not found there: w itself if it satisfies q, else nothing.
    | none => if q w then some w else none

/-- When no write satisfies `q`, the walk finds nothing. -/
theorem lastWhere_none {q : Write → Bool} :
    -- The statement: if the walk finds nothing, every write of the list fails q.
    ∀ {ws : List Write}, lastWhere q ws = none → ∀ x ∈ ws, q x = false
  -- No writes: there is no x to speak of.
  | [], _, _, hx => absurd hx List.not_mem_nil
  -- A write w followed by ws.
  | w :: ws, h, x, hx => by
    -- The tail found nothing, and the head did not satisfy `q` either.
    simp only [lastWhere] at h
    -- Split on what the walk found in the rest ws.
    cases ht : lastWhere q ws with
    -- It found something: then the whole walk found it too, against the assumption.
    | some y => rw [ht] at h; cases h
    -- It found nothing in the rest.
    | none =>
      -- Use that in the assumption.
      rw [ht] at h
      -- Simplify the assumption: it now says q w is not true.
      simp only at h
      -- x is either w itself or a member of the rest.
      rcases List.mem_cons.mp hx with rfl | hm
      -- x is w: split on q w.
      · cases hq : q x with
        -- False: that is the claim.
        | false => rfl
        -- True: the walk would have found w, against the assumption.
        | true => rw [hq] at h; cases h
      -- x is in the rest: the claim for the shorter list.
      · exact lastWhere_none ht x hm

/-- When the walk finds `w`, the list is some writes, then `w`, then newer
writes none of which satisfies `q`. -/
theorem lastWhere_some {q : Write → Bool} :
    -- The statement: if the walk finds w, then w satisfies q, and the list splits into a prefix,
    -- then w, then writes that all fail q.
    ∀ {ws : List Write} {w : Write}, lastWhere q ws = some w →
      q w = true ∧ ∃ pre post, ws = pre ++ w :: post ∧ ∀ x ∈ post, q x = false
  -- No writes: the walk cannot have found anything.
  | [], _, h => by cases h
  -- A write v followed by ws.
  | v :: ws, w, h => by
    -- Unfold one step of the walk in the assumption.
    simp only [lastWhere] at h
    -- Split on what the walk found in the rest ws.
    cases ht : lastWhere q ws with
    -- It found y in the rest.
    | some y =>
      -- Found in the tail: put `v` in front of the tail's prefix.
      rw [ht] at h
      -- The assumption now says y is w.
      simp only [Option.some.injEq] at h
      -- Replace w by y everywhere.
      subst h
      -- The claim for the shorter list gives the split of ws.
      obtain ⟨hq, pre, post, hws, hpost⟩ := lastWhere_some ht
      -- Put v in front of that prefix: the same w and the same tail.
      exact ⟨hq, v :: pre, post, by rw [hws]; rfl, hpost⟩
    -- It found nothing in the rest.
    | none =>
      -- Not in the tail: it is `v` itself, and nothing after it qualifies.
      rw [ht] at h
      -- Simplify the assumption.
      simp only at h
      -- Split on q v.
      cases hq : q v with
      -- False: the walk found nothing, against the assumption.
      | false => rw [hq] at h; cases h
      -- True.
      | true =>
        -- The assumption now says v is w.
        simp [hq] at h
        -- Replace w by v.
        subst h
        -- v satisfies q, the prefix is empty, and nothing in ws satisfies q.
        exact ⟨hq, [], ws, rfl, lastWhere_none ht⟩

/-- When the newest write affecting `k` is `w`, the value of `k` after all
the writes is what `w` left, from some state before it. -/
theorem value_at_last (E : Env) {k : Nat} {ws : List Write} {w : Write}
    -- The newest write affecting `k` is `w`.
    (h : lastWhere (affects E k) ws = some w) (σ : State) :
    -- The statement: some state σ' exists such that k's value after all the writes is k's value
    -- after w applied to σ'.
    ∃ σ', applyWrites E σ ws k = applyWrite E σ' w k := by
  -- Split the list into a prefix, w, and later writes that do not affect k.
  obtain ⟨-, pre, post, rfl, hpost⟩ := lastWhere_some h
  -- The writes before `w` give some state; the writes after it leave `k`.
  refine ⟨applyWrites E σ pre, ?_⟩
  -- Apply the prefix first, then the rest.
  rw [applyWrites_append]
  -- Then w, then the later writes.
  simp only [applyWrites_cons]
  -- The later writes leave k alone.
  exact applyWrites_unaffected E k post _ hpost

/-! ## The read classes and their rules -/

/-- A write leaves exactly `v` under `k`: a put of the nonzero `v`, or, when
`v` is 0, a delete or a covering range delete. -/
def SetsTo (k v : Nat) (w : Write) : Prop :=
  -- Either w is a put of the nonzero v, or v is 0 and w is a delete of k or a range delete covering
  -- k.
  (w = .put k v ∧ v ≠ 0) ∨ (v = 0 ∧ (w = .del k ∨ ∃ lo hi, w = .rangeDel lo hi ∧ lo ≤ k ∧ k < hi))

/-- A write leaves `k` present: a put of a value, or a merge. -/
def Keeps (k : Nat) (w : Write) : Prop :=
  -- Either w puts a nonzero value under k, or w merges into k.
  (∃ v, w = .put k v ∧ v ≠ 0) ∨ (∃ o, w = .merge k o)

/-- A write that leaves exactly `v` does so from any state. -/
theorem setsTo_value (E : Env) {k v : Nat} {w : Write} (h : SetsTo k v w) (σ : State) :
    -- The statement: after w, key k holds v.
    applyWrite E σ w k = v := by
  -- Split on which kind of write leaves v: a put, a delete, or a covering range delete.
  rcases h with ⟨rfl, -⟩ | ⟨rfl, rfl | ⟨lo, hi, rfl, hlo, hhi⟩⟩
  -- The put stores v under k.
  · simp [applyWrite, upd]
  -- The delete leaves 0, and v is 0.
  · simp [applyWrite, upd]
  -- The range covers k, so k becomes 0, and v is 0.
  · simp [applyWrite, hlo, hhi]

/-- A write that keeps `k` present leaves a value there. -/
theorem keeps_present (E : Env) {k : Nat} {w : Write} (h : Keeps k w) (σ : State) :
    -- The statement: after w, key k is not absent.
    applyWrite E σ w k ≠ 0 := by
  -- Split: a put of a nonzero value, or a merge.
  rcases h with ⟨v, rfl, hv⟩ | ⟨o, rfl⟩
  -- The put stores a nonzero value.
  · simpa [applyWrite, upd] using hv
  -- A merge never leaves an absent key (the operator's law apply_ne_zero).
  · simpa [applyWrite, upd] using E.apply_ne_zero (σ k) o

/-- An operand on `k` that `touches` says touches none of the parts `P`. -/
def nonTouching (E : Env) (k : Nat) (P : List Nat) : Write → Bool
  -- A merge into j: it is on k, and the operator says it touches none of P.
  | .merge j o => decide (j = k) && !(E.parts.touches o P)
  -- Any other write: no.
  | _ => false

/-- A write that changes one of the parts `P` of `k`, as a projected read
sees it (plan 3.4): anything that affects `k` except an operand that touches
none of `P`. -/
def decisive (E : Env) (k : Nat) (P : List Nat) (w : Write) : Bool :=
  -- It affects k and is not such a non-touching operand.
  affects E k w && !(nonTouching E k P w)

/-- A read the commit validates, by class. -/
inductive Read where
  /-- Key `k`, validated by sequence (every level but DefraLevel). -/
  | full (k : Nat)
  /-- Key `k`, validated by value (DefraLevel, plan 3.5). -/
  | value (k : Nat)
  /-- Content-addressed key `k`, found: validated for presence (E17). -/
  | presence (k : Nat)
  /-- `get_parts(k, P)` that found the key (plan 3.4). -/
  | parts (k : Nat) (P : List Nat)
  /-- A validated scan (`ScanCheck::Range`) over keys `lo ≤ j < hi` (plan 3.15). -/
  | scan (lo hi : Nat)
  /-- Never validated: a Log read, a commutative stretch, `get_parts(k, [])`. -/
  | exempt

/-- The commit's check for read `r`, given the snapshot `σ` it was served at
and the writes `ws` committed since, oldest first. -/
def Ok (E : Env) : Read → State → List Write → Prop
  -- full: no newer write affects the key.
  | .full k, _, ws => lastWhere (affects E k) ws = none
  -- value:
  | .value k, σ, ws =>
    -- look at the newest newer write of the key;
    match lastWhere (affects E k) ws with
    -- none: fine;
    | none => True
    -- one: it must leave exactly the value read.
    | some w => SetsTo k (σ k) w
  -- presence:
  | .presence k, σ, ws =>
    -- the read found the key, and the newest newer write of the key,
    σ k ≠ 0 ∧
      match lastWhere (affects E k) ws with
      -- if there is none, is fine;
      | none => True
      -- if there is one, keeps the key present.
      | some w => Keeps k w
  -- parts:
  | .parts k P, σ, ws =>
    -- the read found the key, and the newest newer write that changes a named part,
    σ k ≠ 0 ∧
      match lastWhere (decisive E k P) ws with
      -- if there is none, is fine;
      | none => True
      -- if there is one, leaves exactly the value read.
      | some w => SetsTo k (σ k) w
  -- scan: no newer write affects any key of the range.
  | .scan lo hi, _, ws => ∀ w ∈ ws, ∀ j, lo ≤ j → j < hi → affects E j w = false
  -- exempt: nothing is checked.
  | .exempt, _, _ => True

/-- What a passed check guarantees about read `r` between state `σ` (the
snapshot) and state `τ` (the commit point): what a decision on it may use. -/
def Agree (E : Env) : Read → State → State → Prop
  -- full: the key holds the same value at both.
  | .full k, σ, τ => σ k = τ k
  -- value: the same.
  | .value k, σ, τ => σ k = τ k
  -- presence: the key is absent at both or present at both.
  | .presence k, σ, τ => (σ k = 0 ↔ τ k = 0)
  -- parts:
  | .parts k P, σ, τ =>
    -- absent at both or present at both, and every named part reads the same at both.
    (σ k = 0 ↔ τ k = 0) ∧ ∀ p ∈ P, E.parts.proj (σ k) p = E.parts.proj (τ k) p
  -- scan: every key of the range holds the same value at both.
  | .scan lo hi, σ, τ => ∀ j, lo ≤ j → j < hi → σ j = τ j
  -- exempt: nothing is promised.
  | .exempt, _, _ => True

/-! ## Each rule is sound -/

/-- **Ordinary keys, by sequence.** No newer write of `k`: its value at commit
is the value read. -/
theorem full_current (E : Env) {k : Nat} {σ : State} {ws : List Write}
    -- The commit's check for the read passed.
    (h : Ok E (.full k) σ ws) : Agree E (.full k) σ (applyWrites E σ ws) := by
  -- Unfold the check: no newer write affects k.
  simp only [Ok] at h
  -- Unfold the promise: the value at the snapshot equals the value at commit.
  simp only [Agree]
  -- Writes that do not affect k leave it as it was.
  exact (applyWrites_unaffected E k ws σ (lastWhere_none h)).symm

/-- **Ordinary keys at DefraLevel, by value.** The newest newer write leaves
exactly the value read: its value at commit is the value read, however many
writes came between (an identical rewrite, or a value that came back). -/
theorem value_current (E : Env) {k : Nat} {σ : State} {ws : List Write}
    -- The commit's check for the read passed.
    (h : Ok E (.value k) σ ws) : Agree E (.value k) σ (applyWrites E σ ws) := by
  -- Unfold the check.
  simp only [Ok] at h
  -- Unfold the promise.
  simp only [Agree]
  -- Split on the newest newer write of k.
  cases hl : lastWhere (affects E k) ws with
  -- Case: there is none.
  | none =>
    -- No newer write of `k`.
    exact (applyWrites_unaffected E k ws σ (lastWhere_none hl)).symm
  -- Case: the newest is w.
  | some w =>
    -- The newest leaves exactly the value read.
    rw [hl] at h
    -- k's value at commit is what w left, from some state σ'.
    obtain ⟨σ', h'⟩ := value_at_last E hl σ
    -- And w leaves exactly the value read.
    rw [h', setsTo_value E h σ']

/-- **Content-addressed keys, found (E17).** The newest newer write is not a
removal: the key is still present at commit. -/
theorem presence_current (E : Env) {k : Nat} {σ : State} {ws : List Write}
    -- The commit's check for the read passed.
    (h : Ok E (.presence k) σ ws) : Agree E (.presence k) σ (applyWrites E σ ws) := by
  -- Unfold the check.
  simp only [Ok] at h
  -- Take it apart: the key was found, and the rule on the newest write.
  obtain ⟨hσ, h⟩ := h
  -- Unfold the promise.
  simp only [Agree]
  -- Both sides are false: present at the snapshot, present at commit.
  have hρ : applyWrites E σ ws k ≠ 0 := by
    -- Split on the newest newer write of k.
    cases hl : lastWhere (affects E k) ws with
    -- None: the key is as at the snapshot, present.
    | none => rw [applyWrites_unaffected E k ws σ (lastWhere_none hl)]; exact hσ
    -- One, w.
    | some w =>
      -- The rule says w keeps the key present.
      rw [hl] at h
      -- k's value at commit is what w left.
      obtain ⟨σ', h'⟩ := value_at_last E hl σ
      -- Rewrite the goal with that.
      rw [h']
      -- And w leaves a value there.
      exact keeps_present E h σ'
  -- Both directions hold for lack of a case: the key is absent at neither.
  exact ⟨fun h0 => absurd h0 hσ, fun h0 => absurd h0 hρ⟩

/-- Writes that change no part `P` of `k`, from a state where `k` is
present, leave it present with the same parts. -/
theorem untouched_run (E : Env) {k : Nat} {P : List Nat} :
    -- The statement: for every list of writes and state, if no write changes a part of P and k is
    -- present, then k is still present afterwards, with the same parts of P.
    ∀ (ws : List Write) (σ : State), (∀ w ∈ ws, decisive E k P w = false) → σ k ≠ 0 →
      applyWrites E σ ws k ≠ 0 ∧
        ∀ p ∈ P, E.parts.proj (applyWrites E σ ws k) p = E.parts.proj (σ k) p
  -- No writes: the state is unchanged.
  | [], σ, _, hσ => ⟨hσ, fun _ _ => rfl⟩
  -- A first write w and the rest ws.
  | w :: ws, σ, h, hσ => by
    -- Peel the first write off.
    simp only [applyWrites_cons] at *
    -- The first write keeps `k` present with the same parts.
    have hw : applyWrite E σ w k ≠ 0 ∧
        ∀ p ∈ P, E.parts.proj (applyWrite E σ w k) p = E.parts.proj (σ k) p := by
      -- The first write changes no part of P.
      have hd := h w List.mem_cons_self
      -- Split on whether it affects k.
      cases ha : affects E k w with
      -- Case: it does not.
      | false =>
        -- It does not affect `k` at all.
        rw [applyWrite_unaffected E σ w k ha]
        -- So k is present with its parts, as before.
        exact ⟨hσ, fun _ _ => rfl⟩
      -- Case: it does.
      | true =>
        -- It affects `k` but is not decisive: an operand on `k` touching none
        -- of `P`, which leaves `k` present (a merge never deletes) and its
        -- parts as they were (the touches contract).
        simp only [decisive, ha, Bool.true_and, Bool.not_eq_false'] at hd
        -- Split on the kind of write.
        cases w with
        -- A merge into j.
        | merge j o =>
          -- "Changes no part" now means: j is k, and the operator says it touches none of P.
          simp only [nonTouching, Bool.and_eq_true, decide_eq_true_eq, Bool.not_eq_true'] at hd
          -- Take those two facts; j becomes k.
          obtain ⟨rfl, ht⟩ := hd
          -- Unfold the merge.
          simp only [applyWrite, upd]
          -- Present because a merge never deletes; the parts are unchanged by the touches law.
          exact ⟨E.apply_ne_zero _ _, fun p hp => E.touches_sound (σ j) o P ht p hp⟩
        -- A put affecting k would change the parts: impossible here.
        | put _ _ => simp [nonTouching] at hd
        -- The same for a delete.
        | del _ => simp [nonTouching] at hd
        -- The same for a range delete.
        | rangeDel _ _ => simp [nonTouching] at hd
        -- The same for an append.
        | append _ => simp [nonTouching] at hd
    -- Then the rest do too, from there.
    obtain ⟨h1, h2⟩ := untouched_run E ws (applyWrite E σ w)
      -- (given that no later write changes a part and that k is still present)
      (fun w' hw' => h w' (List.mem_cons_of_mem _ hw')) hw.1
    -- Chain the two: the rest keeps what the first write kept.
    exact ⟨h1, fun p hp => (h2 p hp).trans (hw.2 p hp)⟩

/-- **Projected reads (plan 3.4).** The first newer write, newest first, that
changes a named part is a put of exactly the value read, or there is none:
the key is present at commit with the same named parts. -/
theorem parts_current (E : Env) {k : Nat} {P : List Nat} {σ : State} {ws : List Write}
    -- The commit's check for the read passed.
    (h : Ok E (.parts k P) σ ws) : Agree E (.parts k P) σ (applyWrites E σ ws) := by
  -- Unfold the check.
  simp only [Ok] at h
  -- Take it apart: the key was found, and the rule on the newest write that changes a named part.
  obtain ⟨hσ, h⟩ := h
  -- Unfold the promise.
  simp only [Agree]
  -- Present at commit with the parts read, in either case below.
  have key : applyWrites E σ ws k ≠ 0 ∧
      ∀ p ∈ P, E.parts.proj (applyWrites E σ ws k) p = E.parts.proj (σ k) p := by
    -- Split on the newest newer write that changes a named part.
    cases hl : lastWhere (decisive E k P) ws with
    -- Case: there is none.
    | none =>
      -- No newer write changes a named part.
      exact untouched_run E ws σ (lastWhere_none hl) hσ
    -- Case: it is w.
    | some w =>
      -- The newest decisive write is a put of the value read; the writes
      -- after it change no named part.
      rw [hl] at h
      -- Split the list into a prefix, w, and later writes that change no named part.
      obtain ⟨-, pre, post, rfl, hpost⟩ := lastWhere_some hl
      -- After w, key k holds exactly the value read.
      have hw : applyWrite E (applyWrites E σ pre) w k = σ k := setsTo_value E h _
      -- Apply the prefix first, then the rest.
      rw [applyWrites_append]
      -- Then w, then the later writes.
      simp only [applyWrites_cons]
      -- The later writes keep k present with the same parts, starting from the value w left.
      have := untouched_run E post (applyWrite E (applyWrites E σ pre) w) hpost (by rw [hw]; exact hσ)
      -- That value is the value read.
      rw [hw] at this
      -- Which is the claim.
      exact this
  -- Presence: absent at neither. Parts: equal, read the other way round.
  exact ⟨⟨fun h0 => absurd h0 hσ, fun h0 => absurd h0 key.1⟩, fun p hp => (key.2 p hp).symm⟩

/-- **Validated scans (plan 3.15).** No newer write inside the range: every
key of it holds at commit what the scan saw, so no key left and none
appeared. -/
theorem scan_current (E : Env) {lo hi : Nat} {σ : State} {ws : List Write}
    -- The commit's check for the read passed.
    (h : Ok E (.scan lo hi) σ ws) : Agree E (.scan lo hi) σ (applyWrites E σ ws) := by
  -- Unfold the check.
  simp only [Ok] at h
  -- Unfold the promise.
  simp only [Agree]
  -- Take any key j of the range.
  intro j hlo hhi
  -- No newer write affects j, so it is as the scan saw it.
  exact (applyWrites_unaffected E j ws σ (fun w hw => h w hw j hlo hhi)).symm

/-- Every rule is sound: a passed check guarantees its agreement. -/
theorem ok_current (E : Env) {r : Read} {σ : State} {ws : List Write}
    -- The commit's check for the read passed.
    (h : Ok E r σ ws) : Agree E r σ (applyWrites E σ ws) := by
  -- Split on the class of the read.
  cases r with
  -- A full read: its lemma.
  | full k => exact full_current E h
  -- A value read: its lemma.
  | value k => exact value_current E h
  -- A presence read: its lemma.
  | presence k => exact presence_current E h
  -- A read by parts: its lemma.
  | parts k P => exact parts_current E h
  -- A validated scan: its lemma.
  | scan lo hi => exact scan_current E h
  -- An exempt read promises nothing, so there is nothing to show.
  | exempt => trivial

/-! ## Transactions, the history, and the serial run -/

/-- A committed transaction. -/
structure Txn where
  /-- How many commits landed between its snapshot and its own commit. -/
  missed : Nat
  /-- Its validated reads, by class. -/
  reads : List Read
  /-- The content-addressed keys it may create, whose reads are dropped (D7). -/
  creates : List Nat
  /-- Its writes other than creates, from the state it read. -/
  body : State → List Write
  /-- Which of `creates` it puts, from the state it read. -/
  made : State → List Nat

/-- Everything a transaction writes, from the state it read: its body, then
a put of each created key's bytes. -/
def txnWrites (E : Env) (T : Txn) (σ : State) : List Write :=
  -- The body's writes, then one put per created key, storing that key's bytes.
  T.body σ ++ (T.made σ).map (fun k => Write.put k (E.bytes k))

/-- The commit passed: every validated read passed its rule, and every
create key it read and did not put was found and passed the presence rule
(a key the transaction puts has its read dropped). -/
def Passed (E : Env) (T : Txn) (σ : State) (ws : List Write) : Prop :=
  -- Every validated read passed, and every create key it did not put passed the presence rule.
  (∀ r ∈ T.reads, Ok E r σ ws) ∧ (∀ k ∈ T.creates, k ∉ T.made σ → Ok E (.presence k) σ ws)

/-- A write keeps the content-addressed contract: on a content-addressed key
it can only remove (creates put the bytes). -/
def CASafe (E : Env) (w : Write) : Prop :=
  -- For every content-addressed key the write affects, the write is a delete of it or a range
  -- delete.
  ∀ k, E.isCA k = true → affects E k w = true → w = .del k ∨ ∃ lo hi, w = .rangeDel lo hi

/-- Every content-addressed key is absent or holds its bytes. -/
def CAConsistent (E : Env) (σ : State) : Prop :=
  -- For every content-addressed key: absent, or holding its bytes.
  ∀ k, E.isCA k = true → σ k = 0 ∨ σ k = E.bytes k

/-- The caller's side: a decision depends only on what its validated reads
guarantee, and content-addressed creates follow the class's contract. -/
structure Contract (E : Env) (T : Txn) : Prop where
  /-- The body is the same from any two states its reads agree on. -/
  body_agree : ∀ σ τ, (∀ r ∈ T.reads, Agree E r σ τ) → T.body σ = T.body τ
  /-- It only puts keys it declared as creates. -/
  made_sub : ∀ σ, ∀ k ∈ T.made σ, k ∈ T.creates
  /-- It puts every create key it reads as absent. -/
  made_absent : ∀ σ, ∀ k ∈ T.creates, σ k = 0 → k ∈ T.made σ
  /-- Create keys are content-addressed. -/
  creates_ca : ∀ k ∈ T.creates, E.isCA k = true
  /-- The body leaves create keys alone. -/
  body_avoids : ∀ σ, ∀ w ∈ T.body σ, ∀ k ∈ T.creates, affects E k w = false
  /-- The body only removes content-addressed keys. -/
  body_safe : ∀ σ, ∀ w ∈ T.body σ, CASafe E w

/-- The state after a history, newest commit first. Each transaction decided
on the state after the commits it saw (the history without its `missed`
newest ones) and its writes apply at its commit. -/
def run (E : Env) : List Txn → State
  -- No commit: the starting state.
  | [] => E.init
  -- Newest commit T on older history h: the state after h, with T's writes applied. T decided them
  -- on the state after h without the commits it missed.
  | T :: h => applyWrites E (run E h) (txnWrites E T (run E (h.drop T.missed)))
-- This definition calls itself only on shorter histories; Lean is told to measure the history's
-- length.
termination_by h => h.length
-- The proof that the history gets shorter:
decreasing_by
  -- unfold the lengths,
  all_goals simp only [List.length_cons, List.length_drop]
  -- and arithmetic settles it.
  all_goals omega

/-- The writes committed after the first `m` newest commits were cut off,
oldest first: what a transaction that missed `m` commits did not see. -/
def newer (E : Env) : List Txn → Nat → List Write
  -- Cut nothing off: nothing was missed.
  | _, 0 => []
  -- No history: nothing to miss.
  | [], _ + 1 => []
  -- The newest commit T and m more were missed: the writes missed among the older ones, then T's
  -- own writes.
  | T :: h, m + 1 => newer E h m ++ txnWrites E T (run E (h.drop T.missed))

/-- The same transactions one at a time in commit order, each reading the
state just before its commit. -/
def serial (E : Env) : List Txn → State
  -- No commit: the starting state.
  | [] => E.init
  -- Newest commit T: the serial state after h, with T's writes decided on that very state.
  | T :: h => applyWrites E (serial E h) (txnWrites E T (serial E h))

/-- A history in which every commit passed: each transaction missed no more
commits than there were, kept the contract, and passed its checks against
the writes it missed. -/
def Valid (E : Env) : List Txn → Prop
  -- The empty history is valid.
  | [] => True
  -- Newest commit T on h.
  | T :: h =>
    -- h is valid, T missed no more commits than exist, T keeps the contract,
    Valid E h ∧ T.missed ≤ h.length ∧ Contract E T ∧
      -- and T passed its checks against the state at its snapshot and the writes it missed.
      Passed E T (run E (h.drop T.missed)) (newer E h T.missed)

/-- The state after `T :: h`. -/
theorem run_cons (E : Env) (T : Txn) (h : List Txn) :
    -- The statement: the definition of run, one step.
    run E (T :: h) = applyWrites E (run E h) (txnWrites E T (run E (h.drop T.missed))) := by
  -- Unfold run once.
  rw [run]

/-- The state after a history is the state at a transaction's snapshot with
the writes it missed applied on top. -/
theorem run_split (E : Env) :
    -- The statement: for every history h and every m not above its length, the state after h is the
    -- state after dropping the m newest commits, with their writes applied back.
    ∀ (h : List Txn) (m : Nat), m ≤ h.length → run E h = applyWrites E (run E (h.drop m)) (newer E h m)
  -- m is 0: nothing dropped, nothing to apply.
  | h, 0, _ => by simp [newer, applyWrites]
  -- No history, but m at least 1: impossible, m would be above the length.
  | [], _ + 1, hm => by simp at hm
  -- Newest commit T, and m + 1 dropped.
  | T :: h, m + 1, hm => by
    -- Cut one fewer commit off the tail, then apply the newest commit.
    simp only [List.length_cons] at hm
    -- Unfold the state after T :: h, and use the claim for h and m.
    rw [run_cons, run_split E h m (by omega)]
    -- Both sides are now the same writes applied in the same order.
    simp only [List.drop_succ_cons, newer, applyWrites_append]

/-! ## From commit points to a serial history -/

/-- **The lift.** If every commit of a history, from any state satisfying an
invariant `I`, has the effect its decision at the commit point would have,
and keeps `I`, then the history equals the serial run and keeps `I`. `Q` is
the class restriction a per-class theorem imposes. -/
theorem lift (E : Env) (Q : Txn → Prop) (I : State → Prop)
    -- The invariant holds at the start.
    (hinit : I E.init)
    -- Every commit of a transaction in the class has its commit-point effect
    -- and keeps the invariant.
    (hstep : ∀ T h, Q T → Valid E (T :: h) → I (run E h) →
      applyWrites E (run E h) (txnWrites E T (run E (h.drop T.missed))) =
          applyWrites E (run E h) (txnWrites E T (run E h)) ∧
        I (applyWrites E (run E h) (txnWrites E T (run E (h.drop T.missed))))) :
    -- The conclusion: for every history whose transactions are all in the class and which is valid,
    -- the engine's state equals the serial state, and the invariant holds.
    ∀ h : List Txn, (∀ T ∈ h, Q T) → Valid E h → run E h = serial E h ∧ I (run E h)
  -- The empty history: both states are the starting state, where the invariant holds.
  | [], _, _ => ⟨by rw [run, serial], by rw [run]; exact hinit⟩
  -- Newest commit T on h.
  | T :: h, hQ, hv => by
    -- The older commits are serial (the claim for the shorter history); the
    -- newest one has its commit-point effect, which is the serial step.
    have hv' := hv
    -- Unfold what valid means for T :: h.
    simp only [Valid] at hv'
    -- The claim for the older history h
    obtain ⟨heq, hI⟩ := lift E Q I hinit hstep h
      -- (its transactions are in the class, and it is valid).
      (fun T' hT' => hQ T' (List.mem_cons_of_mem _ hT')) hv'.1
    -- The newest commit has its commit-point effect and keeps the invariant.
    obtain ⟨hcp, hI'⟩ := hstep T h (hQ T List.mem_cons_self) hv hI
    -- Two things to show.
    refine ⟨?_, ?_⟩
    -- The states: unfold one step on each side; they match by the two facts above.
    · rw [run_cons, hcp, heq]; simp only [serial]
    -- The invariant: it holds after the newest commit.
    · rw [run_cons]; exact hI'

/-- From a valid commit: the state at its snapshot, the writes it missed,
its contract and its passed checks, with the commit state as the snapshot
plus those writes. -/
theorem valid_head (E : Env) {T : Txn} {h : List Txn} (hv : Valid E (T :: h)) :
    -- The statement: the contract, the passed checks, and the commit state split as the snapshot
    -- plus the missed writes.
    Contract E T ∧ Passed E T (run E (h.drop T.missed)) (newer E h T.missed) ∧
      run E h = applyWrites E (run E (h.drop T.missed)) (newer E h T.missed) := by
  -- Unfold what valid means.
  simp only [Valid] at hv
  -- Take the pieces: the bound on missed, the contract, the passed checks.
  obtain ⟨-, hm, hc, hp⟩ := hv
  -- The split is the lemma above.
  exact ⟨hc, hp, run_split E h T.missed hm⟩

/-- A transaction that creates nothing writes exactly its body. -/
theorem txnWrites_no_creates (E : Env) {T : Txn} (hc : Contract E T) (hcr : T.creates = [])
    -- The statement: for any state it reads, its writes are its body alone.
    (σ : State) : txnWrites E T σ = T.body σ := by
  -- It puts no create key:
  have : T.made σ = [] := by
    -- Everything it puts is a create, and it declared none.
    apply List.eq_nil_iff_forall_not_mem.mpr
    -- Take any key it would put.
    intro k hk
    -- It must be a declared create.
    have := hc.made_sub σ k hk
    -- But none is declared.
    rw [hcr] at this
    -- A member of the empty list: impossible.
    exact List.not_mem_nil this
  -- With no puts, the writes are the body.
  simp [txnWrites, this]

/-- The commit-point step for a transaction that creates nothing and whose
reads are all of a class whose rule is sound. -/
theorem step_of_class (E : Env) (C : Read → Prop)
    -- The class's rule guarantees its agreement.
    (hsound : ∀ r σ ws, C r → Ok E r σ ws → Agree E r σ (applyWrites E σ ws))
    -- (the transaction and the older history)
    (T : Txn) (h : List Txn)
    -- Every read of the transaction is of the class, and it creates nothing.
    (hC : ∀ r ∈ T.reads, C r) (hcr : T.creates = [])
    -- The commit is valid.
    (hv : Valid E (T :: h)) :
    -- The conclusion: applying the writes it decided at its snapshot equals applying those it would
    -- decide at its commit.
    applyWrites E (run E h) (txnWrites E T (run E (h.drop T.missed))) =
      applyWrites E (run E h) (txnWrites E T (run E h)) := by
  -- From validity: the contract, the passed checks, the split.
  obtain ⟨hc, hp, hsplit⟩ := valid_head E hv
  -- Every read agrees between the snapshot and the commit point, so the body
  -- is the same from both.
  have hag : ∀ r ∈ T.reads, Agree E r (run E (h.drop T.missed)) (run E h) := by
    -- Take any read.
    intro r hr
    -- See the commit state as the snapshot plus the missed writes.
    rw [hsplit]
    -- The class's rule, which passed, gives the agreement.
    exact hsound r _ _ (hC r hr) (hp.1 r hr)
  -- With no creates the writes are the body, and the body is the same from both states.
  rw [txnWrites_no_creates E hc hcr, txnWrites_no_creates E hc hcr, hc.body_agree _ _ hag]

/-! ## One theorem per class -/

/-- **Ordinary keys, every level.** A history whose transactions read only
keys validated by sequence, and passed, is serial in commit order. -/
theorem ordinary_serial (E : Env) (h : List Txn)
    -- Every transaction's reads are of this class, and it creates nothing.
    (hcls : ∀ T ∈ h, (∀ r ∈ T.reads, ∃ k, r = .full k) ∧ T.creates = [])
    -- Every commit of the history passed its checks.
    (hv : Valid E h) : run E h = serial E h :=
  -- Use the lift, with the class as its restriction and no invariant needed.
  (lift E (fun T => (∀ r ∈ T.reads, ∃ k, r = .full k) ∧ T.creates = []) (fun _ => True)
    -- (The trivial invariant holds at the start.)
    trivial
    -- Each commit has its commit-point effect, by step_of_class with this class's soundness lemma.
    (fun T h hQ hv _ => ⟨step_of_class E (fun r => ∃ k, r = .full k)
      (fun r _ _ hr hok => by obtain ⟨k, rfl⟩ := hr; exact full_current E hok)
      T h hQ.1 hQ.2 hv, trivial⟩)
    -- Take the first half of the lift's conclusion: the two states are equal.
    h hcls hv).1

/-- **Ordinary keys at DefraLevel, by value.** A history whose transactions'
reads are value-validated, and passed, is serial in commit order, although a
read survives any newer writes that leave the value it read. -/
theorem value_serial (E : Env) (h : List Txn)
    -- Every transaction's reads are of this class, and it creates nothing.
    (hcls : ∀ T ∈ h, (∀ r ∈ T.reads, ∃ k, r = .value k) ∧ T.creates = [])
    -- Every commit of the history passed its checks.
    (hv : Valid E h) : run E h = serial E h :=
  -- Use the lift, with the class as its restriction and no invariant needed.
  (lift E (fun T => (∀ r ∈ T.reads, ∃ k, r = .value k) ∧ T.creates = []) (fun _ => True)
    -- (The trivial invariant holds at the start.)
    trivial
    -- Each commit has its commit-point effect, by step_of_class with this class's soundness lemma.
    (fun T h hQ hv _ => ⟨step_of_class E (fun r => ∃ k, r = .value k)
      (fun r _ _ hr hok => by obtain ⟨k, rfl⟩ := hr; exact value_current E hok)
      T h hQ.1 hQ.2 hv, trivial⟩)
    -- Take the first half of the lift's conclusion: the two states are equal.
    h hcls hv).1

/-- **Projected reads.** A history whose transactions' reads are projected,
and passed, is serial in commit order: each decision uses only existence and
the parts it named, and those are current at its commit. -/
theorem parts_serial (E : Env) (h : List Txn)
    -- Every transaction's reads are of this class, and it creates nothing.
    (hcls : ∀ T ∈ h, (∀ r ∈ T.reads, ∃ k P, r = .parts k P) ∧ T.creates = [])
    -- Every commit of the history passed its checks.
    (hv : Valid E h) : run E h = serial E h :=
  -- Use the lift, with the class as its restriction and no invariant needed.
  (lift E (fun T => (∀ r ∈ T.reads, ∃ k P, r = .parts k P) ∧ T.creates = []) (fun _ => True)
    -- (The trivial invariant holds at the start.)
    trivial
    -- Each commit has its commit-point effect, by step_of_class with this class's soundness lemma.
    (fun T h hQ hv _ => ⟨step_of_class E (fun r => ∃ k P, r = .parts k P)
      (fun r _ _ hr hok => by obtain ⟨k, P, rfl⟩ := hr; exact parts_current E hok)
      T h hQ.1 hQ.2 hv, trivial⟩)
    -- Take the first half of the lift's conclusion: the two states are equal.
    h hcls hv).1

/-- **Validated scans.** A history whose transactions' reads are validated
scans, and passed, is serial in commit order: no revocation and no phantom
lands under a decision on a range. -/
theorem scan_serial (E : Env) (h : List Txn)
    -- Every transaction's reads are of this class, and it creates nothing.
    (hcls : ∀ T ∈ h, (∀ r ∈ T.reads, ∃ lo hi, r = .scan lo hi) ∧ T.creates = [])
    -- Every commit of the history passed its checks.
    (hv : Valid E h) : run E h = serial E h :=
  -- Use the lift, with the class as its restriction and no invariant needed.
  (lift E (fun T => (∀ r ∈ T.reads, ∃ lo hi, r = .scan lo hi) ∧ T.creates = []) (fun _ => True)
    -- (The trivial invariant holds at the start.)
    trivial
    -- Each commit has its commit-point effect, by step_of_class with this class's soundness lemma.
    (fun T h hQ hv _ => ⟨step_of_class E (fun r => ∃ lo hi, r = .scan lo hi)
      (fun r _ _ hr hok => by obtain ⟨lo, hi, rfl⟩ := hr; exact scan_current E hok)
      T h hQ.1 hQ.2 hv, trivial⟩)
    -- Take the first half of the lift's conclusion: the two states are equal.
    h hcls hv).1

/-- **Exempt reads: Log reads, commutative stretches, `get_parts` with no
part.** Nothing is validated, and the history is serial in commit order
exactly when no decision depends on them: the contract's `body_agree` with
an agreement that says nothing forces the body to ignore them. That is the
relaxation these classes declare; `exempt_decision_breaks_serial` is what
happens when a caller decides on one. Appends in the body are applied at
commit, in commit order. -/
theorem exempt_serial (E : Env) (h : List Txn)
    -- Every transaction's reads are of this class, and it creates nothing.
    (hcls : ∀ T ∈ h, (∀ r ∈ T.reads, r = .exempt) ∧ T.creates = [])
    -- Every commit of the history passed its checks.
    (hv : Valid E h) : run E h = serial E h :=
  -- Use the lift, with the class as its restriction and no invariant needed.
  (lift E (fun T => (∀ r ∈ T.reads, r = .exempt) ∧ T.creates = []) (fun _ => True)
    -- (The trivial invariant holds at the start.)
    trivial
    -- Each commit has its commit-point effect, by step_of_class: an exempt read promises nothing,
    -- so its "agreement" is trivially true.
    (fun T h hQ hv _ => ⟨step_of_class E (fun r => r = .exempt)
      (fun r _ _ hr _ => by subst hr; trivial)
      T h hQ.1 hQ.2 hv, trivial⟩)
    -- Take the first half of the lift's conclusion: the two states are equal.
    h hcls hv).1

/-- **Blind merges.** A history of transactions that read nothing (the
blind-merge path: each merge applies at commit on top of whatever landed) is
serial in commit order. Two concurrent blind merges both commit; that is
the relaxation of snapshot isolation, and `blindMerge_commutes` states when
the merge may also be placed at its snapshot. -/
theorem blindMerge_serial (E : Env) (h : List Txn)
    -- No transaction reads anything or creates anything.
    (hcls : ∀ T ∈ h, T.reads = [] ∧ T.creates = [])
    -- Every commit of the history passed its checks.
    (hv : Valid E h) : run E h = serial E h :=
  -- Use the lift, with "reads nothing, creates nothing" as its restriction and no invariant needed.
  (lift E (fun T => T.reads = [] ∧ T.creates = []) (fun _ => True)
    -- (The trivial invariant holds at the start.)
    trivial
    -- Each commit has its commit-point effect, by step_of_class over the empty class: there is no
    -- read to agree on.
    (fun T h hQ hv _ => ⟨step_of_class E (fun _ => False)
      (fun _ _ _ hr _ => hr.elim)
      -- (A transaction with no reads has all its reads in the empty class.)
      T h (by rw [hQ.1]; intro r hr; exact absurd hr List.not_mem_nil) hQ.2 hv, trivial⟩)
    -- Take the first half of the lift's conclusion: the two states are equal.
    h hcls hv).1

/-! ## Content-addressed keys: creates are idempotent -/

/-- **`ca_put_idempotent`.** Putting a content-addressed key's bytes where the
key already holds them changes nothing. -/
theorem ca_put_idempotent (E : Env) (σ : State) (k : Nat) (h : σ k = E.bytes k) :
    -- The statement: the state is unchanged.
    applyWrite E σ (.put k (E.bytes k)) = σ := by
  -- Compare the two states key by key.
  funext j
  -- Unfold the put.
  simp only [applyWrite, upd]
  -- Split on whether j is k.
  split
  -- j is k: it holds the bytes already.
  · subst_vars; exact h.symm
  -- j is another key: untouched.
  · rfl

/-- Puts of the bytes of keys `M`: key `j` ends with its bytes if it is in
`M`, as it was otherwise. -/
theorem puts_at (E : Env) :
    -- The statement: for every list M of keys, every state and every key j, after the puts j holds
    -- its bytes if it is in M and its old value otherwise.
    ∀ (M : List Nat) (σ : State) (j : Nat),
      applyWrites E σ (M.map (fun k => Write.put k (E.bytes k))) j =
        if j ∈ M then E.bytes j else σ j
  -- No keys: nothing was put.
  | [], σ, j => by simp [applyWrites_nil]
  -- A first key k and the rest M.
  | k :: M, σ, j => by
    -- Peel the first put off.
    simp only [List.map_cons, applyWrites_cons] at *
    -- Use the claim for the rest.
    rw [puts_at E M]
    -- Split on whether j is among the rest.
    by_cases hj : j ∈ M
    -- It is: it ends with its bytes.
    · simp [hj]
    -- It is not: split on whether j is the first key.
    · by_cases hk : j = k
      -- It is: the first put stored its bytes.
      · subst hk; simp [applyWrite, upd]
      -- It is not: nothing touched it.
      · simp [hj, hk, applyWrite, upd]

/-- A write that keeps the content-addressed contract and affects a
content-addressed key removes it. -/
theorem caSafe_removes (E : Env) {w : Write} (hw : CASafe E w) {k : Nat}
    -- (the key is content-addressed, the write affects it, from any state)
    (hk : E.isCA k = true) (ha : affects E k w = true) (σ : State) :
    -- The statement: the key is absent afterwards.
    applyWrite E σ w k = 0 := by
  -- The contract says the write is a delete of k or a range delete.
  rcases hw k hk ha with rfl | ⟨lo, hi, rfl⟩
  -- A delete leaves 0.
  · simp [applyWrite, upd]
  -- A range delete that affects k covers k,
  · simp only [affects, decide_eq_true_eq] at ha
    -- so k becomes 0.
    simp [applyWrite, ha.1, ha.2]

/-- Writes that keep the content-addressed contract keep every
content-addressed key absent or at its bytes. -/
theorem caSafe_consistent (E : Env) :
    -- The statement: for every list of writes and every state where the content-addressed keys are
    -- consistent, if every write keeps the contract, they are still consistent afterwards.
    ∀ (ws : List Write) (σ : State), CAConsistent E σ → (∀ w ∈ ws, CASafe E w) →
      CAConsistent E (applyWrites E σ ws)
  -- No writes: as before.
  | [], σ, hσ, _ => hσ
  -- A first write w and the rest.
  | w :: ws, σ, hσ, hws => by
    -- Peel the first write off.
    simp only [applyWrites_cons]
    -- It is enough that the state after the first write is consistent: the rest follows from the
    -- claim for the shorter list.
    apply caSafe_consistent E ws _ _ (fun w' hw' => hws w' (List.mem_cons_of_mem _ hw'))
    -- The first write removes a content-addressed key or leaves it.
    intro k hk
    -- Split on whether w affects k.
    cases ha : affects E k w with
    -- It does not: k is as before, which was consistent.
    | false => rw [applyWrite_unaffected E σ w k ha]; exact hσ k hk
    -- It does: a contract-keeping write that affects k removes it, so k is absent.
    | true => exact Or.inl (caSafe_removes E (hws w List.mem_cons_self) hk ha σ)

/-- A transaction's writes keep every content-addressed key absent or at its
bytes. -/
theorem txn_consistent (E : Env) {T : Txn} (hc : Contract E T) {ρ : State}
    -- (from any commit state ρ where the keys are consistent, whatever state σ it decided on)
    (hρ : CAConsistent E ρ) (σ : State) :
    -- The statement: the keys are still consistent after its writes.
    CAConsistent E (applyWrites E ρ (txnWrites E T σ)) := by
  -- Its writes are the body, then the creates.
  simp only [txnWrites, applyWrites_append]
  -- After the body the keys are consistent (the body keeps the contract).
  have hb := caSafe_consistent E (T.body σ) ρ hρ (hc.body_safe σ)
  -- Take any content-addressed key k.
  intro k hk
  -- After the creates, k holds its bytes if it was created, and otherwise what the body left.
  rw [puts_at]
  -- A created key gets its bytes; any other is as the body left it.
  split
  -- Created: it holds its bytes.
  · exact Or.inr rfl
  -- Not created: consistent after the body.
  · exact hb k hk

/-- **The content-addressed commit point.** A transaction that passed its
checks, from a commit state where every content-addressed key is absent or
at its bytes, has the effect its decision at the commit point would have,
although its reads of the keys it creates were dropped: a create it made at
its snapshot puts bytes the key holds at commit or that the commit-point
decision would put too. -/
theorem ca_commit_point (E : Env) {T : Txn} (hc : Contract E T) {σs : State}
    -- (it passed its checks against the snapshot σs and the missed writes ws,
    {ws : List Write} (hp : Passed E T σs ws)
    -- and the commit state is consistent)
    (hca : CAConsistent E (applyWrites E σs ws)) :
    -- The statement: its writes decided at the snapshot have the same effect as those decided at
    -- the commit state.
    applyWrites E (applyWrites E σs ws) (txnWrites E T σs) =
      applyWrites E (applyWrites E σs ws) (txnWrites E T (applyWrites E σs ws)) := by
  -- Write ρ for the commit state.
  generalize hρdef : applyWrites E σs ws = ρ at hca ⊢
  -- The validated reads agree, so the body is the same from both states.
  have hag : ∀ r ∈ T.reads, Agree E r σs ρ := by
    -- Each read passed its rule, and each rule is sound.
    intro r hr; rw [← hρdef]; exact ok_current E (hp.1 r hr)
  -- Split the writes into body and creates; the two bodies are equal.
  simp only [txnWrites, applyWrites_append, hc.body_agree _ _ hag]
  -- Key by key, the creates agree.
  funext j
  -- After the creates, a key holds its bytes if created and otherwise what the body left, on both
  -- sides.
  rw [puts_at, puts_at]
  -- Split on whether j is a create key.
  by_cases hj : j ∈ T.creates
  -- A create key: the body leaves it, and both sides end with its bytes.
  ·
    -- The body does not touch j
    have hbody : applyWrites E ρ (T.body ρ) j = ρ j :=
      -- (every write of the body avoids create keys).
      applyWrites_unaffected E j _ ρ (fun w hw => hc.body_avoids ρ w hw j hj)
    -- If j is present at commit, it holds its bytes:
    have hbytes : ρ j ≠ 0 → ρ j = E.bytes j := by
      -- suppose it is present;
      intro h0
      -- consistency says absent or at its bytes;
      rcases hca j (hc.creates_ca j hj) with h | h
      -- absent contradicts present;
      · exact absurd h h0
      -- so it is at its bytes.
      · exact h
    -- Replace what the body left by ρ j.
    rw [hbody]
    -- The commit-point decision puts it unless it is present, and then it
    -- holds its bytes.
    have rhs : (if j ∈ T.made ρ then E.bytes j else ρ j) = E.bytes j := by
      -- Split on whether the commit-point decision puts j.
      split
      -- It does: the bytes.
      · rfl
      -- It does not; name that fact hn.
      · rename_i hn
        -- Enough to show j is present at commit.
        apply hbytes
        -- Suppose it were absent.
        intro h0
        -- Then the contract would make the decision put it: a contradiction.
        exact hn (hc.made_absent ρ j hj h0)
    -- The snapshot decision puts it, or found it present, and then the
    -- presence check guarantees it is still present at commit.
    have lhs : (if j ∈ T.made σs then E.bytes j else ρ j) = E.bytes j := by
      -- Split on whether the snapshot decision puts j.
      split
      -- It does: the bytes.
      · rfl
      -- It does not; name that fact hn.
      · rename_i hn
        -- Then j was present at the snapshot (an absent create key is always put).
        have hfound : σs j ≠ 0 := fun h0 => hn (hc.made_absent σs j hj h0)
        -- Its dropped read is covered by the presence rule, which passed.
        have hpres := presence_current E (hp.2 j hj hn)
        -- State that for the commit state ρ.
        rw [hρdef] at hpres
        -- Unfold the promise: absent at both or present at both.
        simp only [Agree] at hpres
        -- Present at the snapshot, so present at commit, so at its bytes.
        exact hbytes (fun h0 => hfound (hpres.mpr h0))
    -- Both sides are the bytes.
    rw [lhs, rhs]
  -- Not a create key: neither side puts it.
  ·
    -- The snapshot decision does not put it (it only puts creates).
    have h1 : j ∉ T.made σs := fun hm => hj (hc.made_sub σs j hm)
    -- Nor does the commit-point decision.
    have h2 : j ∉ T.made ρ := fun hm => hj (hc.made_sub ρ j hm)
    -- So both sides are what the body left.
    simp [h1, h2]

/-- **Content-addressed keys (E17, D7).** A history whose transactions read
content-addressed keys for presence (or in full, when they found nothing)
and create them with their bytes, and passed, is serial in commit order,
provided every content-addressed key starts absent or at its bytes. Two
creates of one key both commit; the relaxation that makes it serial is that
a create is idempotent (`ca_put_idempotent`). -/
theorem contentAddressed_serial (E : Env)
    -- Every content-addressed key starts absent or at its bytes.
    (hinit : CAConsistent E E.init) (h : List Txn)
    -- Every read is a presence read, or a full one (a read that found nothing).
    (hcls : ∀ T ∈ h, ∀ r ∈ T.reads, (∃ k, r = .presence k) ∨ (∃ k, r = .value k) ∨
      (∃ k, r = .full k))
    -- Every commit of the history passed its checks.
    (hv : Valid E h) : run E h = serial E h :=
  -- Use the lift, with the consistency of content-addressed keys as the invariant.
  (lift E (fun T => ∀ r ∈ T.reads, (∃ k, r = .presence k) ∨ (∃ k, r = .value k) ∨
      (∃ k, r = .full k)) (CAConsistent E) hinit
    -- Each commit:
    (fun T h _ hv hI => by
      -- from validity, the contract, the passed checks and the split;
      obtain ⟨hc, hp, hsplit⟩ := valid_head E hv
      -- two things to show; consistency after the commit is txn_consistent;
      refine ⟨?_, txn_consistent E hc hI _⟩
      -- see the commit state as the snapshot plus the missed writes;
      rw [hsplit] at hI ⊢
      -- the commit-point effect is ca_commit_point.
      exact ca_commit_point E hc hp hI)
    -- Take the first half of the lift's conclusion: the two states are equal.
    h hcls hv).1

/-! ## The combined theorem -/

/-- **Every class at once.** A history in which every commit passed its
checks, whatever classes its reads are and whatever it creates, is serial in
commit order, provided every content-addressed key starts absent or at its
bytes. -/
theorem all_classes_serial (E : Env)
    -- Every content-addressed key starts absent or at its bytes.
    (hinit : CAConsistent E E.init) (h : List Txn)
    -- Every commit of the history passed its checks.
    (hv : Valid E h) : run E h = serial E h :=
  -- Use the lift, with no class restriction and the consistency invariant.
  (lift E (fun _ => True) (CAConsistent E) hinit
    -- Each commit:
    (fun T h _ hv hI => by
      -- from validity, the contract, the passed checks and the split;
      obtain ⟨hc, hp, hsplit⟩ := valid_head E hv
      -- two things to show; consistency after the commit is txn_consistent;
      refine ⟨?_, txn_consistent E hc hI _⟩
      -- see the commit state as the snapshot plus the missed writes;
      rw [hsplit] at hI ⊢
      -- the commit-point effect is ca_commit_point.
      exact ca_commit_point E hc hp hI)
    -- Take the first half of the lift's conclusion: the two states are equal.
    h (fun _ _ => trivial) hv).1

/-! ## A commit that writes nothing, checked at the published horizon (plan 3.8, E50)

A transaction that writes nothing commits outside the commit pipeline, at
every level (`RegolithEngine::commit_write_free`,
src/engine/commit/write_free.rs). It looks at two points: the published
horizon, the commits readers see, which is the history `h` here; and the
decided frontier, which adds the commits decided and not yet visible, the
writes `later` below. It checks each read its level promises to check, and
when every check passes it is placed right after the last commit of `h`.
Its commit changes no state.

The worked example (the same in the TLA+ model, the Rust tests and the
docs): a checking account holds 30 and a savings account 0. A report reads
(30, 20) after a deposit of 20 into savings; a withdrawal that read (30, 0)
then takes 25 and a fee of 1 from checking, leaving 4. When the report's
check runs after that, it finds checking changed and refuses the report,
instead of letting it claim a sheet (30, 20) that the database, with the
withdrawal in it, no longer shows (`read_only_anomaly` in Relaxations.lean).

`writeFree_at_horizon` says the placement is right: every read that passed
holds at the horizon what it held where it was served, and the state at the
horizon is the serial state of the commits published by then. Each read may
be served at its own point: an optimistic transaction serves every read at
its snapshot, a pessimistic one serves a `get_for_update` later, at the
newest published point once it holds the key's lock. The theorem takes each
read with the number of published commits it missed, so one snapshot for all
of them is not assumed.

A read is also checked against the decided writes that are not yet visible,
so the commit is refused whenever a commit through the pipeline, decided
after them, would be. `full_ok_of_later` and `scan_ok_of_later` show that
this only refuses more: a full read or a validated scan that passes against
the published writes and the later ones together passes against the
published ones alone, so the theorem below still applies. The same two
lemmas cover the engine's other stricter case, a version above a point that
it counts against the read because compaction may have folded an older one
under it. A read checked by value, presence or parts is judged by its rule
at the published point on its own, so the theorem applies to it directly.
-/

/-- When every write of the list fails `q`, the walk finds nothing: the
other direction of `lastWhere_none`, which goes from "the walk finds
nothing" to "every write fails". -/
theorem lastWhere_none_of {q : Write → Bool} :
    -- The statement: if every write of the list fails q, the walk from the newest end finds nothing.
    ∀ {ws : List Write}, (∀ x ∈ ws, q x = false) → lastWhere q ws = none
  -- No writes: the walk finds nothing, by its definition.
  | [], _ => rfl
  -- A write w followed by the newer writes ws.
  | w :: ws, h => by
    -- The newer writes all fail q too, so this same theorem on the shorter list finds nothing there.
    have ht : lastWhere q ws = none :=
      -- (each write of ws is a write of w :: ws, which fails q)
      lastWhere_none_of (fun x hx => h x (List.mem_cons_of_mem _ hx))
    -- And w itself fails q.
    have hw : q w = false := h w List.mem_cons_self
    -- Unfold one step of the walk: nothing among the newer writes, and w fails q, so nothing.
    simp [lastWhere, ht, hw]

/-- **`full_ok_of_later`.** A full read that no write touched among the
published writes and the later ones together is untouched among the
published ones: a check that also counts writes decided and not yet visible
(or a version compaction may have folded) refuses more, never less. -/
theorem full_ok_of_later (E : Env) {k : Nat} {σ : State} {ws later : List Write}
    -- The check passed against the published writes followed by the later ones.
    (h : Ok E (.full k) σ (ws ++ later)) : Ok E (.full k) σ ws := by
  -- Unfold the check on both sides: the walk for a write affecting k finds nothing.
  simp only [Ok] at h ⊢
  -- Every write of the longer list fails to affect k, so every write of the published part does.
  exact lastWhere_none_of (fun x hx => lastWhere_none h x (List.mem_append_left later hx))

/-- **`scan_ok_of_later`.** The same for a validated scan: no write in its
range among all the writes means none among the published ones. -/
theorem scan_ok_of_later (E : Env) {lo hi : Nat} {σ : State} {ws later : List Write}
    -- The check passed against the published writes followed by the later ones.
    (h : Ok E (.scan lo hi) σ (ws ++ later)) : Ok E (.scan lo hi) σ ws := by
  -- Unfold the check on both sides: no write affects any key of the range.
  simp only [Ok] at h ⊢
  -- A published write is also a write of the longer list, where it affects no key of the range.
  exact fun w hw => h w (List.mem_append_left later hw)

/-- **`writeFree_at_horizon` (E50).** A commit that writes nothing, each of
whose checked reads passed its rule against the writes published between
the point the read was served at and the published horizon, read at the
horizon what it read, and the state at the horizon is the serial state of
the commits published by then. So the commit, placed right after them, is
serial: it read the one-at-a-time state there, and it changes nothing a
later commit could depend on. This is why it needs no slot of the commit
pipeline. Each read comes with its own point (how many published commits it
missed), so a pessimistic `get_for_update` served after the snapshot is
covered as well as a read at the snapshot. -/
theorem writeFree_at_horizon (E : Env)
    -- Every content-addressed key starts absent or at its bytes (the combined theorem needs it).
    (hinit : CAConsistent E E.init) {h : List Txn}
    -- (`rs`: the reads its level checks, each with how many of the published commits it missed)
    {rs : List (Read × Nat)}
    -- The commits published by the horizon form a valid history,
    (hv : Valid E h)
    -- each read missed no more of them than there are,
    (hm : ∀ p ∈ rs, p.2 ≤ h.length)
    -- and each read passed its rule against the writes published since it was served.
    (hpass : ∀ p ∈ rs, Ok E p.1 (run E (h.drop p.2)) (newer E h p.2)) :
    -- The statement: every checked read holds at the horizon what it held where it was served, and
    -- the state at the horizon is the serial state of the published commits.
    (∀ p ∈ rs, Agree E p.1 (run E (h.drop p.2)) (run E h)) ∧ run E h = serial E h := by
  -- Two things to show; the second is the combined theorem above, applied to the published history.
  refine ⟨fun p hp => ?_, all_classes_serial E hinit h hv⟩
  -- See the state at the horizon as the state the read was served at, with the writes published
  -- since applied on top.
  rw [run_split E h p.2 (hm p hp)]
  -- The read's rule passed against those writes, and every rule is sound.
  exact ok_current E (hpass p hp)

-- The end of this file's names.
end Regolith.Validation
