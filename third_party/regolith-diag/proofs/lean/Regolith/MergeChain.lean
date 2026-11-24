/-!
# MergeChain: a key's versions held first-in-place read exactly as a list

THE STORY. A database that has a merge operator answers a read of a key by
collecting the key's visible versions, newest first, down to the first one
that is a whole value or a deletion: those are the "chain". Merge operands
above that version are then folded onto it, oldest first
(`RegolithEngine::get_with_merge`, src/engine/mod.rs:1885). The chain used
to be a plain growable list (`Vec<KeyVersion>`), which allocates memory for
every read. Nearly every key's chain is one version long (its value, with
no operand above it), so the chain now keeps its first version in place and
only spills later ones into a list (`MergeChain`, src/engine/merge_chain.rs:11):
`first` holds the newest version, `rest` the ones after it.

Tiny example: versions 9, 7, 5 pushed in that order sit as `first = 9`,
`rest = [7, 5]`; the list they stand for is `[9, 7, 5]`. Truncating to 2
keeps `[9, 7]`; popping takes 7, the oldest.

The read also gained a fast path (src/engine/mod.rs:1896): when the chain
holds at most one version and it is not a merge operand, that version is the
answer (its value, or nothing for a deletion or an empty chain), and the
operand fold is skipped.

What must never happen: the in-place chain reading differently from the list
it replaced (a version lost, duplicated or out of order), or the fast path
answering differently from the full path.

WHAT IS PROVED, for every chain and every sequence of operations:

1. `run_reads_as_vector`: from the empty chain, after any sequence of
   pushes, truncations and pops, the chain stands for exactly the list the
   same operations make of an empty list, and its length, oldest version,
   popped version and walk all read as that list's. `Inv` (an empty `first`
   means an empty `rest`) holds all along; `push_without_inv_reorders`
   shows a push needs it.
2. `skip_position`: the range-tombstone check's rewrite, from searching the
   list's slice `chain[start..]` to skipping `start` versions of the walk,
   searches the same versions.
3. `new_read_is_old_read`: the read as it is now (the in-place chain, the
   fast path, then the full path with its operands built by taking and then
   reversing) answers exactly what the read before did on the list (operands
   built by walking backwards), for every chain whose versions carry one of
   the three value types the internal key encoding writes (deletion 0,
   value 1, merge 2; src/engine/internal_key.rs:27-33).
   `unknown_type_reads_differently` shows that last condition is needed: a
   type byte outside the three, which no writer produces, is folded as an
   operand by the full path and answered as nothing by the fast path.

NO TLA+ MODEL: a chain is a value one read builds and drops on its own
thread, never shared, so nothing here is temporal or concurrent. The unit
test `the_chain_reads_as_the_vector_of_its_versions` (merge_chain.rs:94)
checks one run; this file covers every run.

How to read the Lean: `def` defines a function or a property, `structure`
groups named fields, `inductive` lists the shapes a value can take,
`theorem` states a fact and its proof follows `:= by`. Lines starting with
`--` are comments; each proof step is commented.
-/

-- Everything below is named Regolith.MergeChain.<name>.
namespace Regolith.MergeChain

/-! ## The chain and the list it stands for -/

/-- A `MergeChain` (merge_chain.rs:11): the newest version in place, if
any, and the versions after it, newest first. `α` is what a version is. -/
structure Chain (α : Type) where
  /-- `MergeChain::first` (merge_chain.rs:12). -/
  first : Option α
  /-- `MergeChain::rest` (merge_chain.rs:14). -/
  rest : List α

/-- The rule the code keeps (merge_chain.rs:13, "Empty while `first` is"):
when nothing is in place, nothing has spilled either. -/
def Chain.Inv {α : Type} (c : Chain α) : Prop :=
  -- No first version means no later ones.
  c.first = none → c.rest = []

/-- The list a chain stands for: the version in place, then the rest. This
is the `Vec<KeyVersion>` the chain replaced. -/
def Chain.toList {α : Type} (c : Chain α) : List α :=
  -- Zero or one version in place, followed by the spilled ones.
  c.first.toList ++ c.rest

/-- `MergeChain::default()`: nothing in place, nothing spilled. -/
def Chain.empty {α : Type} : Chain α :=
  -- Both fields empty.
  ⟨none, []⟩

/-- `MergeChain::push` (merge_chain.rs:19): the first version goes in
place, every later one onto the end of `rest`. -/
def Chain.push {α : Type} (c : Chain α) (v : α) : Chain α :=
  -- Look at what is in place.
  match c.first with
  -- Nothing: the new version goes in place, `rest` is left as it is.
  | none => ⟨some v, c.rest⟩
  -- Something: the new version goes after the spilled ones.
  | some _ => ⟨c.first, c.rest ++ [v]⟩

/-- `MergeChain::len` (merge_chain.rs:27): one for a version in place, plus
the spilled ones. -/
def Chain.len {α : Type} (c : Chain α) : Nat :=
  -- `usize::from(first.is_some()) + rest.len()`.
  (if c.first.isSome then 1 else 0) + c.rest.length

/-- `MergeChain::truncate` (merge_chain.rs:32): keep the newest `n`. -/
def Chain.truncate {α : Type} (c : Chain α) : Nat → Chain α
  -- Keep none: empty both fields (`checked_sub(1)` is `None`).
  | 0 => ⟨none, []⟩
  -- Keep `k + 1`: the one in place and the first `k` spilled.
  | k + 1 => ⟨c.first, c.rest.take k⟩

/-- `MergeChain::last` (merge_chain.rs:43): the oldest version, which is the
last spilled one, or else the one in place. -/
def Chain.last {α : Type} (c : Chain α) : Option α :=
  -- Look at the last spilled version.
  match c.rest.getLast? with
  -- There is one: it is the oldest.
  | some v => some v
  -- None spilled: the one in place, if any (`.or(first)`).
  | none => c.first

/-- `MergeChain::pop` (merge_chain.rs:48): take the oldest version out,
returning the chain left and the version taken. -/
def Chain.pop {α : Type} (c : Chain α) : Chain α × Option α :=
  -- Look at the last spilled version.
  match c.rest.getLast? with
  -- There is one: `rest.pop()` takes it off the end.
  | some v => (⟨c.first, c.rest.dropLast⟩, some v)
  -- None spilled: `first.take()` takes the one in place, if any.
  | none => (⟨none, c.rest⟩, c.first)

/-- `MergeChain::iter` (merge_chain.rs:53): the version in place, then the
spilled ones (`first.iter().chain(&rest)`). -/
def Chain.iter {α : Type} (c : Chain α) : List α :=
  -- The two walks one after the other.
  c.first.toList ++ c.rest

/-! ## Each operation reads as the same operation on the list -/

/-- A push onto a chain that keeps the rule adds the version at the end of
the list it stands for. -/
theorem push_toList {α : Type} (c : Chain α) (v : α) (h : c.Inv) :
    -- The statement: the list after the push is the list before, then `v`.
    (c.push v).toList = c.toList ++ [v] := by
  -- Split on what is in place.
  cases hf : c.first with
  -- Case: nothing in place.
  | none =>
    -- By the rule, nothing is spilled either.
    have hr := h hf
    -- Both sides are `[v]`.
    simp [Chain.push, Chain.toList, hf, hr]
  -- Case: a version in place.
  | some x =>
    -- Both sides are `x :: rest ++ [v]`.
    simp [Chain.push, Chain.toList, hf]

/-- Without the rule a push can put a version out of order: the reason the
code keeps `rest` empty while `first` is. -/
theorem push_without_inv_reorders :
    -- The statement: pushing 2 onto "nothing in place, 1 spilled" stands for `[2, 1]`, not `[1, 2]`.
    (Chain.push ⟨none, [1]⟩ 2).toList = [2, 1] ∧ (Chain.toList ⟨none, [1]⟩) ++ [2] = [1, 2] := by
  -- Both halves compute to the lists named.
  constructor <;> rfl

/-- Every push keeps the rule: afterwards something is in place. -/
theorem push_inv {α : Type} (c : Chain α) (v : α) : (c.push v).Inv := by
  -- Split on what was in place.
  cases hf : c.first with
  -- Case: nothing: the push puts `v` in place, so the rule holds trivially.
  | none => simp [Chain.push, Chain.Inv, hf]
  -- Case: something: it stays in place, so the rule holds trivially.
  | some x => simp [Chain.push, Chain.Inv, hf]

/-- The chain's length is the length of the list it stands for. -/
theorem len_eq {α : Type} (c : Chain α) : c.len = c.toList.length := by
  -- Split on what is in place.
  cases hf : c.first with
  -- Case: nothing: both are the number spilled.
  | none => simp [Chain.len, Chain.toList, hf]
  -- Case: something: one plus the number spilled on both sides, up to the order of the sum.
  | some x => simp [Chain.len, Chain.toList, hf]; omega

/-- Truncating a chain that keeps the rule keeps the first `n` of its list. -/
theorem truncate_toList {α : Type} (c : Chain α) (n : Nat) (h : c.Inv) :
    -- The statement: the list after truncating is the list's first `n`.
    (c.truncate n).toList = c.toList.take n := by
  -- Split on how many are kept.
  cases n with
  -- Case: none kept: both sides are empty.
  | zero => simp [Chain.truncate, Chain.toList]
  -- Case: `k + 1` kept.
  | succ k =>
    -- Split on what is in place.
    cases hf : c.first with
    -- Case: nothing in place.
    | none =>
      -- By the rule, nothing is spilled either.
      have hr := h hf
      -- Both sides are empty.
      simp [Chain.truncate, Chain.toList, hf, hr]
    -- Case: `x` in place: both sides are `x` then the first `k` spilled.
    | some x => simp [Chain.truncate, Chain.toList, hf]

/-- Truncating keeps the rule. -/
theorem truncate_inv {α : Type} (c : Chain α) (n : Nat) (h : c.Inv) : (c.truncate n).Inv := by
  -- Split on how many are kept.
  cases n with
  -- Case: none kept: both fields are empty, so the rule holds.
  | zero => simp [Chain.truncate, Chain.Inv]
  -- Case: some kept: `first` is unchanged.
  | succ k =>
    -- Suppose nothing is in place after the truncation.
    intro hf
    -- Then nothing was in place before it, so nothing was spilled.
    have hr := h hf
    -- Unfold the truncation in the assumption and the goal.
    simp [Chain.truncate] at hf ⊢
    -- The first `k` of an empty list is empty.
    simp [hr]

/-- The chain's oldest version is the last of its list. -/
theorem last_eq {α : Type} (c : Chain α) : c.last = c.toList.getLast? := by
  -- Split on the last spilled version.
  cases hr : c.rest.getLast? with
  -- Case: none spilled.
  | none =>
    -- Then the spilled list is empty.
    have : c.rest = [] := List.getLast?_eq_none_iff.mp hr
    -- Both sides are the version in place, whether or not there is one.
    cases hf : c.first <;> simp [Chain.last, Chain.toList, hf, this]
  -- Case: `v` is the last spilled: it is the last of the whole list too.
  | some v =>
    -- The last of `first ++ rest` is the last of `rest` when `rest` has one.
    simp [Chain.last, Chain.toList, hr, List.getLast?_append]

/-- The version a pop takes is the last of the list. -/
theorem pop_value {α : Type} (c : Chain α) : c.pop.2 = c.toList.getLast? := by
  -- Split on the last spilled version.
  cases hr : c.rest.getLast? with
  -- Case: none spilled.
  | none =>
    -- Then the spilled list is empty.
    have : c.rest = [] := List.getLast?_eq_none_iff.mp hr
    -- Both sides are the version in place, whether or not there is one.
    cases hf : c.first <;> simp [Chain.pop, Chain.toList, hf, this]
  -- Case: `v` is the last spilled: the pop takes it, and it ends the list.
  | some v =>
    -- The last of `first ++ rest` is the last of `rest` when `rest` has one.
    simp [Chain.pop, Chain.toList, hr, List.getLast?_append]

/-- What a pop leaves is the list without its last version. -/
theorem pop_toList {α : Type} (c : Chain α) : c.pop.1.toList = c.toList.dropLast := by
  -- Split on the last spilled version.
  cases hr : c.rest.getLast? with
  -- Case: none spilled.
  | none =>
    -- Then the spilled list is empty.
    have : c.rest = [] := List.getLast?_eq_none_iff.mp hr
    -- Both sides are empty: a list of at most one version loses it.
    cases hf : c.first <;> simp [Chain.pop, Chain.toList, hf, this]
  -- Case: something spilled.
  | some v =>
    -- The spilled list is not empty, since it has a last version.
    have hne : c.rest ≠ [] := by
      -- Suppose it were empty.
      intro h0
      -- Then it would have no last version, against `hr`.
      rw [h0] at hr; simp at hr
    -- Dropping the last of `first ++ rest` drops the last of `rest`.
    simp [Chain.pop, Chain.toList, hr, List.dropLast_append_of_ne_nil hne]

/-- A pop keeps the rule. -/
theorem pop_inv {α : Type} (c : Chain α) (h : c.Inv) : c.pop.1.Inv := by
  -- Split on the last spilled version.
  cases hr : c.rest.getLast? with
  -- Case: none spilled: the pop empties `first`, and `rest` is empty already.
  | none =>
    -- Unfold the pop and the rule: what is left to show is that `rest` is empty.
    simp [Chain.pop, hr, Chain.Inv]
    -- It is: it has no last version.
    exact List.getLast?_eq_none_iff.mp hr
  -- Case: something spilled: `first` is unchanged.
  | some v =>
    -- Suppose nothing is in place after the pop.
    intro hf
    -- Unfold the pop: nothing was in place before it either.
    simp [Chain.pop, hr] at hf ⊢
    -- So nothing was spilled, and nothing is after dropping the last.
    simp [h hf]

/-! ## Any run of operations reads as the same run on the list -/

/-- The operations a read makes on a chain: `push` (each version a key walk
collects, merge_chain.rs:66 `push_version`, and the range tombstone's stand-in,
src/engine/mod.rs:1859), `truncate` (mod.rs:1858) and `pop` (mod.rs:1903, 1936). -/
inductive Op (α : Type) where
  /-- Push one version. -/
  | push (v : α)
  /-- Keep the newest `n`. -/
  | truncate (n : Nat)
  /-- Take the oldest. -/
  | pop

/-- One operation on a chain. -/
def Chain.step {α : Type} (c : Chain α) : Op α → Chain α
  -- A push.
  | .push v => c.push v
  -- A truncation.
  | .truncate n => c.truncate n
  -- A pop: the chain left.
  | .pop => c.pop.1

/-- The same operation on the plain list (`Vec`): push onto the end,
`Vec::truncate` keeps the first `n`, `Vec::pop` drops the last. -/
def vecStep {α : Type} (l : List α) : Op α → List α
  -- A push onto the end.
  | .push v => l ++ [v]
  -- The first `n`.
  | .truncate n => l.take n
  -- All but the last.
  | .pop => l.dropLast

/-- One operation keeps the rule and reads as the same operation on the
list. -/
theorem step_reads_as_vector {α : Type} (c : Chain α) (o : Op α) (h : c.Inv) :
    -- The statement: the rule holds after, and the list after is the list operation's result.
    (c.step o).Inv ∧ (c.step o).toList = vecStep c.toList o := by
  -- One case per operation.
  cases o with
  -- A push: the two push facts.
  | push v => exact ⟨push_inv c v, push_toList c v h⟩
  -- A truncation: the two truncation facts.
  | truncate n => exact ⟨truncate_inv c n h, truncate_toList c n h⟩
  -- A pop: the two pop facts.
  | pop => exact ⟨pop_inv c h, pop_toList c⟩

/-- Any number of operations, from any chain that keeps the rule, keep it
and read as the same operations on its list. -/
theorem steps_read_as_vector {α : Type} (ops : List (Op α)) :
    -- The statement: for every chain keeping the rule, after `ops` it keeps it and reads as the list.
    ∀ c : Chain α, c.Inv →
      (ops.foldl Chain.step c).Inv ∧
        (ops.foldl Chain.step c).toList = ops.foldl vecStep c.toList := by
  -- One operation at a time.
  induction ops with
  -- No operation: nothing changes.
  | nil =>
    -- Take the chain and its rule.
    intro c h
    -- The rule as given, and the same list on both sides.
    exact ⟨h, rfl⟩
  -- Operation `o`, then the rest `os` (for which the claim is `ih`).
  | cons o os ih =>
    -- Take the chain and its rule.
    intro c h
    -- After `o`: the rule holds and the list is `o` applied to the list.
    have ⟨hi, ht⟩ := step_reads_as_vector c o h
    -- The rest, from the chain after `o`.
    have := ih (c.step o) hi
    -- A fold over `o :: os` is the fold over `os` from the result of `o`.
    simp only [List.foldl_cons]
    -- Replace the list operation's result by the chain's list.
    rw [← ht]
    -- That is the claim for the rest.
    exact this

/-- The empty chain keeps the rule. -/
theorem empty_inv {α : Type} : (Chain.empty : Chain α).Inv := by
  -- Nothing is spilled in the empty chain, whatever is in place.
  intro _; rfl

/-- **The chain reads as the vector.** From the empty chain, after any
operations, the chain keeps the rule, stands for the list the same
operations make, and its length, oldest version, pop and walk all read as
that list's. -/
theorem run_reads_as_vector {α : Type} (ops : List (Op α)) :
    -- The statement, with `c` the chain and `l` the list after the operations.
    let c := ops.foldl Chain.step Chain.empty
    -- The list the same operations make of an empty list.
    let l := ops.foldl vecStep []
    -- The rule, the same list, then each read the code makes.
    c.Inv ∧ c.toList = l ∧ c.len = l.length ∧ c.last = l.getLast? ∧ c.pop.2 = l.getLast? ∧
      c.pop.1.toList = l.dropLast ∧ c.iter = l := by
  -- The run keeps the rule and reads as the list.
  have ⟨hi, ht⟩ := steps_read_as_vector ops Chain.empty empty_inv
  -- Unfold the two names in the goal.
  simp only []
  -- The rule and the list are proved; five reads are left.
  refine ⟨hi, ht, ?_, ?_, ?_, ?_, ?_⟩
  -- The length: the chain's is its list's, which is `l`.
  · rw [len_eq, ht]; rfl
  -- The oldest version: the chain's is its list's last, which is `l`'s.
  · rw [last_eq, ht]; rfl
  -- The version a pop takes: its list's last.
  · rw [pop_value, ht]; rfl
  -- What a pop leaves: its list without the last.
  · rw [pop_toList, ht]; rfl
  -- The walk is the list itself.
  · exact ht

/-- The range-tombstone check (src/engine/mod.rs:1853) searched the list's
slice `chain[start..]` for the first version a tombstone hides; it now
skips the first `start` versions of the chain's walk. Both search the same
versions, so they find the same position. (`start` is the chain's length
before the source was walked, so the old slice never ran past its end.) -/
theorem skip_position {α : Type} (c : Chain α) (start : Nat) (p : α → Bool) :
    -- The statement: searching the walk after `start` is searching the list after `start`.
    (c.iter.drop start).findIdx? p = (c.toList.drop start).findIdx? p := by
  -- The walk is the list, by definition.
  rfl

/-! ## The read: the fast path and the full path answer alike -/

/-- One version of a key: its sequence, its value type byte and its value
(`KeyVersion`, `(u64, u8, DbSlice)`). `V` is what a value is. -/
structure Version (V : Type) where
  /-- The sequence number. -/
  seq : Nat
  /-- The value type byte. -/
  vt : Nat
  /-- The bytes. -/
  value : V

/-- `VALUE_TYPE_DELETION` (src/engine/internal_key.rs:29). -/
def deletionT : Nat := 0
/-- `VALUE_TYPE_VALUE` (internal_key.rs:27). -/
def valueT : Nat := 1
/-- `VALUE_TYPE_MERGE` (internal_key.rs:33). -/
def mergeT : Nat := 2

/-- What the full path makes of the oldest version (mod.rs:1911-1915): a
value is the base and ends the operands, a deletion ends them with no base,
anything else (an operand, or no version) leaves no base and ends nothing. -/
def baseOf {V : Type} : Option (Version V) → Option V × Bool
  -- A value: it is the base, and a terminator.
  | some ⟨_, 1, v⟩ => (some v, true)
  -- A deletion: no base, but a terminator.
  | some ⟨_, 0, _⟩ => (none, true)
  -- Anything else: no base, no terminator.
  | _ => (none, false)

/-- The answer a popped version gives (mod.rs:1903-1906 and 1936-1939): a
value's bytes, or nothing. -/
def answerOf {V : Type} : Option (Version V) → Option V
  -- A value: its bytes.
  | some ⟨_, 1, v⟩ => some v
  -- Anything else, or no version: nothing.
  | _ => none

/-- The full path as it was, on the list `l` (the `Vec` chain): the base
from the oldest version; the operands are every version above the
terminator, walked backwards, so oldest first; no operands means the oldest
version answers alone; otherwise the operator (`merge_call::apply_merge`,
src/engine/merge_call.rs:50, here `merge`, which may fail) folds them. -/
def fullReadVec {V E : Type} (merge : Option V → List V → Except E V) (l : List (Version V)) :
    -- The answer: a value, nothing, or the operator's failure.
    Except E (Option V) :=
  -- The base and whether the chain ends in a terminator.
  let bt := baseOf l.getLast?
  -- How many versions are operands: all but a terminator.
  let mergeEnd := if bt.2 then l.length - 1 else l.length
  -- `chain[..merge_end].iter().rev()`: those versions, oldest first, their bytes.
  let operands := (l.take mergeEnd).reverse.map Version.value
  -- No operands: the oldest version answers; else the operator folds them.
  if operands.isEmpty then .ok (answerOf l.getLast?)
  -- The operator's result, as a found value.
  else (merge bt.1 operands).map some

/-- The full path as it is now (mod.rs:1911-1943), on the list: the same,
with the operands taken in walk order and then reversed
(`operands_owned.extend(..take(merge_end)..)` then `reverse()`). -/
def fullRead {V E : Type} (merge : Option V → List V → Except E V) (l : List (Version V)) :
    -- The answer: a value, nothing, or the operator's failure.
    Except E (Option V) :=
  -- The base and whether the chain ends in a terminator.
  let bt := baseOf l.getLast?
  -- How many versions are operands: all but a terminator.
  let mergeEnd := if bt.2 then l.length - 1 else l.length
  -- Their bytes in walk order, then reversed: oldest first.
  let operands := ((l.take mergeEnd).map Version.value).reverse
  -- No operands: the oldest version answers; else the operator folds them.
  if operands.isEmpty then .ok (answerOf l.getLast?)
  -- The operator's result, as a found value.
  else (merge bt.1 operands).map some

/-- When the fast path is taken (mod.rs:1898-1902): at most one version, and
it is not a merge operand (or there is none). -/
def fastApplies {V : Type} (l : List (Version V)) : Bool :=
  -- `chain.len() <= 1` and `chain.last().is_none_or(|v| v.1 != VALUE_TYPE_MERGE)`.
  decide (l.length ≤ 1) && (match l.getLast? with
    | none => true
    | some v => v.vt != mergeT)

/-- The read now, on the list: the fast path's answer (mod.rs:1903-1906)
when it applies, else the full path. -/
def read {V E : Type} (merge : Option V → List V → Except E V) (l : List (Version V)) :
    -- The answer: a value, nothing, or the operator's failure.
    Except E (Option V) :=
  -- The fast path when it applies, else the full path.
  if fastApplies l then .ok (answerOf l.getLast?) else fullRead merge l

/-- Every version carries one of the three value types the encoding writes. -/
def WellTyped {V : Type} (l : List (Version V)) : Prop :=
  -- Deletion, value or merge, for each version.
  ∀ v, v ∈ l → v.vt = deletionT ∨ v.vt = valueT ∨ v.vt = mergeT

/-- Where the fast path applies, its answer is the full path's. -/
theorem fast_eq_full {V E : Type} (merge : Option V → List V → Except E V)
    -- The chain, its versions well typed, and the fast path applying to it:
    (l : List (Version V)) (hw : WellTyped l) (hf : fastApplies l = true) :
    -- The statement: the fast answer equals the full path's.
    Except.ok (answerOf l.getLast?) = fullRead merge l := by
  -- Split on the chain's shape: empty, one version, or more.
  match l, hw, hf with
  -- No version: both answer nothing.
  | [], _, _ => simp [fullRead, baseOf, answerOf]
  -- One version, of type `t`.
  | [⟨s, t, x⟩], hw, hf =>
    -- Its type is one of the three.
    have ht := hw ⟨s, t, x⟩ (by simp)
    -- The fast path applies, so it is not a merge operand.
    simp [fastApplies, mergeT] at hf
    -- Write the three types as numbers.
    simp [deletionT, valueT, mergeT] at ht
    -- So it is a deletion or a value (a merge is ruled out).
    rcases ht with h0 | h1 | h2
    -- A deletion: a terminator with no operands above it, so both answer nothing.
    · subst h0; simp [fullRead, baseOf, answerOf]
    -- A value: a terminator with no operands above it, so both answer its bytes.
    · subst h1; simp [fullRead, baseOf, answerOf]
    -- A merge operand contradicts the fast path applying.
    · exact absurd h2 hf
  -- Two or more versions: the fast path does not apply, against `hf`.
  | _ :: _ :: _, _, hf => simp [fastApplies] at hf

/-- With the fast path, the read answers what the full path alone would. -/
theorem read_eq_full {V E : Type} (merge : Option V → List V → Except E V)
    -- For a chain whose versions are well typed:
    (l : List (Version V)) (hw : WellTyped l) : read merge l = fullRead merge l := by
  -- Unfold the read: it is the fast path or the full path.
  unfold read
  -- Split on whether the fast path applies.
  split
  -- It applies: its answer is the full path's.
  · next h => exact fast_eq_full merge l hw h
  -- It does not: the read is the full path.
  · rfl

/-- The type condition is needed: one version of type 3, which no writer
produces, is an operand to the full path (which folds it) and nothing to the
fast path. With an operator answering the number of operands, the full path
gives 1 and the read gives nothing. -/
theorem unknown_type_reads_differently :
    -- The statement: the read answers nothing, the full path answers 1.
    read (E := Unit) (fun _ os => .ok os.length) [(⟨0, 3, 7⟩ : Version Nat)] = .ok none ∧
      fullRead (E := Unit) (fun _ os => .ok os.length) [(⟨0, 3, 7⟩ : Version Nat)] = .ok (some 1) := by
  -- Both sides compute to the answers named.
  constructor <;> rfl

/-- The operands' order: walking backwards and taking the bytes (before)
is taking the bytes and reversing (now). -/
theorem fullRead_vec {V E : Type} (merge : Option V → List V → Except E V)
    -- For every chain:
    (l : List (Version V)) : fullRead merge l = fullReadVec merge l := by
  -- Mapping a reversed list is reversing the mapped list.
  simp only [fullRead, fullReadVec, List.map_reverse]

/-- The full path as the code runs it on the in-place chain (mod.rs:1911-1943):
`chain.last()`, `chain.len()`, `chain.iter().take(..)` and `chain.pop()`. -/
def fullReadChain {V E : Type} (merge : Option V → List V → Except E V)
    -- The in-place chain, and the answer: a value, nothing, or the operator's failure.
    (c : Chain (Version V)) : Except E (Option V) :=
  -- The base and whether the chain ends in a terminator.
  let bt := baseOf c.last
  -- How many versions are operands: all but a terminator.
  let mergeEnd := if bt.2 then c.len - 1 else c.len
  -- Their bytes in walk order, then reversed: oldest first.
  let operands := ((c.iter.take mergeEnd).map Version.value).reverse
  -- No operands: the popped version answers; else the operator folds them.
  if operands.isEmpty then .ok (answerOf c.pop.2)
  -- The operator's result, as a found value.
  else (merge bt.1 operands).map some

/-- The read as the code runs it on the in-place chain (mod.rs:1895-1943):
the fast path through `len`, `last` and `pop`, else the full path. -/
def readChain {V E : Type} (merge : Option V → List V → Except E V)
    -- The in-place chain, and the answer: a value, nothing, or the operator's failure.
    (c : Chain (Version V)) : Except E (Option V) :=
  -- At most one version, and not a merge operand.
  if decide (c.len ≤ 1) && (match c.last with
      | none => true
      | some v => v.vt != mergeT) then .ok (answerOf c.pop.2)
  -- Otherwise the full path.
  else fullReadChain merge c

/-- The read on the chain is the read on the list it stands for. -/
theorem readChain_list {V E : Type} (merge : Option V → List V → Except E V)
    -- For every in-place chain:
    (c : Chain (Version V)) : readChain merge c = read merge c.toList := by
  -- Replace each chain read (length, oldest, pop) by the list's.
  simp only [readChain, read, fastApplies, fullReadChain, fullRead, len_eq, last_eq, pop_value]
  -- What is left is the same on both sides, the walk being the list.
  rfl

/-- **The new read is the old read.** For every chain of well-typed
versions and every operator, the read as it is now (the in-place chain, the
fast path, the operands taken then reversed) answers exactly what the read
before did on the plain list. -/
theorem new_read_is_old_read {V E : Type} (merge : Option V → List V → Except E V)
    -- For every in-place chain whose versions are well typed:
    (c : Chain (Version V)) (hw : WellTyped c.toList) :
    -- The statement: the same answer, value, nothing or failure alike.
    readChain merge c = fullReadVec merge c.toList := by
  -- The chain's read is the list's; the fast path changes nothing; the operands' order agrees.
  rw [readChain_list, read_eq_full merge _ hw, fullRead_vec]

-- The end of the names starting Regolith.MergeChain.
end Regolith.MergeChain
