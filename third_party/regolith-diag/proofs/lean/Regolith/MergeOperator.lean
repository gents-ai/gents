/-!
# MergeOperator: the merge-operator contract (plan 3.3)

## The story, for a reader who has never seen the code

A "merge" is a write that does not say what a key should hold, only how to
change it: "add 1 to this counter". The database keeps such changes
("operands") in a pile on top of the key's last full value, and a read works
out the answer by applying them one after another, oldest first. The caller
supplies the rule for applying one operand (`MergeOperator::full_merge`,
src/options.rs). To keep the pile short, background compaction may squash
two neighbouring operands into one with the caller's `partial_merge`.

Tiny example: a counter holds 5 and three operands "+1", "+1", "+1" sit on
it. A read answers 8. Compaction may squash the first two into "+2", and a
read must still answer 8.

What can go wrong: a `partial_merge` that does not mean the same as applying
the two operands in turn. Then squashing changes what a read returns, and
the answer depends on when compaction happened to run. What is proved here:
if the caller's operator keeps a short contract, no squashing, in any
grouping, ever changes a read; and the same for the contract of `touches`,
which lets a reader of one part of a value ignore operands that only change
other parts.

## Which model this backs

This file backs the TLA+ model `proofs/tla/RepeatableRead.tla` (with its
commit rule in `RepeatableReadCommit.tla`). Two of its rules rest on the
laws proved here:

* the blind-merge rule (`MergeDecider`; configurations
  `MC_DefraLevel_Green_Counters`, `MC_DefraLevel_Red_MergeIgnoresReplacement`
  and the other counter Reds, invariant `INV_CounterExact`): operands are
  folded onto the key's value in commit order, and compaction may combine
  adjacent operands with `partial_merge` without changing what a read
  returns;
* the projected-read rule (`Decisive`; configurations
  `MC_DefraLevel_Green_Parts` and its Reds, invariant `INV_PartsCurrent`):
  an operand for which `touches` answers false leaves the named parts as
  they were.

TLC checks those rules for a few operands. The theorems here hold for every
operator that keeps the contract, every number of operands and every way of
grouping them. `Regolith/Validation.lean` uses `TouchesSound` from here for
its projected-read theorem.

What is proved, in plain words:

1. `fold_eq_grouping` and `fold_eq_any_grouping`: when `partial_merge` is
   exact (applying two adjacent operands one after the other equals applying
   their partial merge, from every value), folding the operands one by one
   (`full_merge`) gives the same value as first combining them in any
   grouping. `steps_preserve_fold` says the same for compaction seen as a
   sequence of steps, each combining two adjacent operands.
2. `groupings_agree`: when `partial_merge` is also associative, any two
   groupings of the same operands combine to the same operand.
3. `touches_law`: an operand that touches no part in a set leaves those
   parts' values unchanged, and so does any run of such operands.
   `touches_grouping` and `touches_survives_compaction`: when `touches` of a
   partial merge is the `or` of its two inputs, the law holds for a combined
   operand too.
4. `inexact_breaks_fold`: the RED case. An operator that rounds after every
   step, like a floating-point sum, with "add the operands" as its partial
   merge: one rounding differs from two, so compaction would change what a
   read returns. Such an operator must not declare `partial_merge`.
5. `part_operator_exact`, `part_operator_assoc`, `part_operator_touches_sound`
   and `part_operator_touches_partial`: the operand RepeatableRead.tla puts on
   its part key (one more on each part it touches) keeps the whole contract.

How to read the Lean: `def` defines a function or a property, `structure`
groups named fields, `theorem` states a fact and its proof follows `:= by`.
Lines starting with `--` are comments; each proof step is commented.
-/

-- Everything below is named Regolith.MergeOperator.<name>.
namespace Regolith.MergeOperator

/-! ## The operator and full_merge -/

/-- A merge operator over values `V` and operands `O`. In the engine both are
bytes; here they are any types. `apply v o` folds one operand onto a value:
what the engine's `full_merge` does for each operand in turn. -/
structure Operator (V O : Type) where
  /-- Fold one operand onto a value. -/
  apply : V → O → V

/-- `full_merge(base, operands)`: the operands folded onto the base, oldest
first. This is the value a read returns for a key whose newest replacement
left `base` and whose operands since are `os`. -/
def fullMerge {V O : Type} (M : Operator V O) (base : V) (os : List O) : V :=
  -- Start from the base and apply each operand in turn, first operand first.
  os.foldl M.apply base

/-- `partial_merge` is exact: from every value, applying `a` then `b` equals
applying their partial merge. This is the law that relates two adjacent
operands to the one compaction replaces them with; the plan calls it
"agrees with full_merge for every grouping". -/
def Exact {V O : Type} (M : Operator V O) (pm : O → O → O) : Prop :=
  -- For every value v and operands a and b: a then b on v equals their partial merge on v.
  ∀ v a b, M.apply (M.apply v a) b = M.apply v (pm a b)

/-- `partial_merge` is associative on adjacent operands: combining `a` with
`b` first, or `b` with `c` first, gives the same operand. -/
def Assoc {O : Type} (pm : O → O → O) : Prop :=
  -- For every three operands: (a with b) with c equals a with (b with c).
  ∀ a b c, pm (pm a b) c = pm a (pm b c)

/-! ## Groupings -/

/-- A grouping of operands: a binary tree whose leaves, left to right, are
the operands in order, and whose inner nodes are partial merges. Every way
compaction can combine a run of adjacent operands into one is such a
tree. -/
inductive Group (O : Type) where
  /-- One operand, not combined with anything. -/
  | leaf (o : O)
  /-- The partial merge of everything on the left with everything on the
  right. -/
  | node (l r : Group O)

/-- The operands of a grouping, oldest first. -/
def Group.leaves {O : Type} : Group O → List O
  -- A single operand: just that operand.
  | .leaf o => [o]
  -- A node: the left side's operands, then the right side's.
  | .node l r => l.leaves ++ r.leaves

/-- The single operand a grouping combines its leaves into. -/
def Group.eval {O : Type} (pm : O → O → O) : Group O → O
  -- A single operand: itself.
  | .leaf o => o
  -- A node: the partial merge of what the left side combines into with what the right side combines
  -- into.
  | .node l r => pm (l.eval pm) (r.eval pm)

/-- **One grouping.** With an exact `partial_merge`, applying the operand a
grouping combines into gives the same value as folding its leaves one by
one, from every value. -/
theorem fold_eq_grouping {V O : Type} {M : Operator V O} {pm : O → O → O}
    -- `partial_merge` is exact.
    (hx : Exact M pm) :
    -- The statement: for every grouping g and every value v, applying g's combined operand to v
    -- equals folding g's leaves onto v.
    ∀ (g : Group O) (v : V), M.apply v (g.eval pm) = fullMerge M v g.leaves
  -- Case: a single operand.
  | .leaf o, v => by
    -- One operand: both sides apply it once.
    simp [Group.eval, Group.leaves, fullMerge]
  -- Case: a node with a left and a right part.
  | .node l r, v => by
    -- Apply the left part, then the right part (exactness, read right to
    -- left); each part is a fold of its leaves (the claim for the smaller
    -- trees); and two folds in a row are one fold of the joined list.
    -- The claim for the left part, from v.
    have hl := fold_eq_grouping hx l v
    -- The claim for the right part, from the value the left part's leaves give.
    have hr := fold_eq_grouping hx r (fullMerge M v l.leaves)
    -- Unfold the definitions everywhere; a fold over a joined list is two folds in a row.
    simp only [Group.eval, Group.leaves, fullMerge, List.foldl_append] at *
    -- Split the node's operand into left then right (exactness, read right to left), and replace
    -- each by its fold.
    rw [← hx, hl, hr]

/-- **Any grouping.** Compaction turns a run of operands into a shorter run,
each output operand combining a group of adjacent inputs. With an exact
`partial_merge`, folding the output gives the value folding the input
gives, from every base. -/
theorem fold_eq_any_grouping {V O : Type} {M : Operator V O} {pm : O → O → O}
    -- `partial_merge` is exact.
    (hx : Exact M pm) :
    -- The statement: for every list of groupings and every value, folding the combined operands
    -- equals folding all the leaves.
    ∀ (gs : List (Group O)) (v : V),
      fullMerge M v (gs.map (·.eval pm)) = fullMerge M v (gs.flatMap (·.leaves))
  -- No groupings: nothing to fold on either side.
  | [], v => rfl
  -- A first grouping g and the rest gs.
  | g :: gs, v => by
    -- The first group's operand equals the fold of its leaves; the rest is
    -- the claim for the shorter list, started from that value.
    -- The first grouping's operand equals the fold of its leaves.
    have h1 := fold_eq_grouping hx g v
    -- The claim for the rest, started from the value the first grouping gives.
    have h2 := fold_eq_any_grouping hx gs (M.apply v (g.eval pm))
    -- Unfold the folds on both sides, one grouping at a time.
    simp only [fullMerge, List.map_cons, List.foldl_cons, List.flatMap_cons,
      List.foldl_append] at *
    -- Rewrite with the two facts: both sides are the same.
    rw [h2, h1]

/-- Combining `x` with a left fold equals folding from `x` combined with the
fold's start: associativity, pushed along a list. -/
theorem pm_foldl {O : Type} {pm : O → O → O}
    -- `partial_merge` is associative.
    (ha : Assoc pm) :
    -- The statement: for every list rs and operands x and b, x combined with (rs folded from b)
    -- equals rs folded from (x combined with b).
    ∀ (rs : List O) (x b : O), pm x (rs.foldl pm b) = rs.foldl pm (pm x b)
  -- Nothing to fold: both sides are x combined with b.
  | [], _, _ => rfl
  -- A first operand c and the rest rs.
  | c :: rs, x, b => by
    -- One more operand `c`: regroup `pm x (pm b c)` as `pm (pm x b) c`.
    -- Unfold one step of each fold.
    simp only [List.foldl_cons]
    -- Use the claim for the rest, then regroup by associativity.
    rw [pm_foldl ha rs x (pm b c), ha]

/-- Every grouping combines its leaves `a :: rest` into the left fold
`rest.foldl pm a`, when `partial_merge` is associative. -/
theorem eval_eq_foldl {O : Type} {pm : O → O → O}
    -- `partial_merge` is associative.
    (ha : Assoc pm) :
    -- The statement: every grouping has a first leaf a and further leaves rest, and combines into
    -- rest folded from a.
    ∀ g : Group O, ∃ a rest, g.leaves = a :: rest ∧ g.eval pm = rest.foldl pm a
  -- A single operand o: it is the first leaf, nothing follows, and it combines into itself.
  | .leaf o => ⟨o, [], rfl, rfl⟩
  -- A node with a left and a right part.
  | .node l r => by
    -- Each side is a left fold of its own leaves; joining them is one left
    -- fold, by `pm_foldl`.
    -- The left part: first leaf a, further leaves ls, combining into ls folded from a.
    obtain ⟨a, ls, hl, el⟩ := eval_eq_foldl ha l
    -- The right part: first leaf b, further leaves rs, combining into rs folded from b.
    obtain ⟨b, rs, hr, er⟩ := eval_eq_foldl ha r
    -- The node's first leaf is a, and its further leaves are ls, then b, then rs. Two things to
    -- show.
    refine ⟨a, ls ++ b :: rs, ?_, ?_⟩
    -- The leaves are as claimed: the left leaves followed by the right leaves.
    · simp [Group.leaves, hl, hr]
    -- The combined operand: unfold both sides.
    · simp only [Group.eval, el, er, List.foldl_append, List.foldl_cons]
      -- What remains is pm_foldl: combining with a fold is folding from the combination.
      exact pm_foldl ha rs _ b

/-- **Any two groupings agree.** With an associative `partial_merge`, two
groupings of the same operands combine to the same operand. -/
theorem groupings_agree {O : Type} {pm : O → O → O}
    -- `partial_merge` is associative.
    (ha : Assoc pm) (g₁ g₂ : Group O)
    -- The two groupings have the same operands in the same order.
    (h : g₁.leaves = g₂.leaves) :
    -- The statement: the two groupings combine into the same operand.
    g₁.eval pm = g₂.eval pm := by
  -- Both are the left fold of the same list.
  -- The first grouping: first leaf a, further leaves r, combining into r folded from a.
  obtain ⟨a, r, h1, e1⟩ := eval_eq_foldl ha g₁
  -- The second grouping: first leaf b, further leaves s, combining into s folded from b.
  obtain ⟨b, s, h2, e2⟩ := eval_eq_foldl ha g₂
  -- The two leaf lists are equal: a :: r = b :: s.
  rw [h1, h2] at h
  -- So a = b and r = s.
  simp only [List.cons.injEq] at h
  -- Replace b by a and s by r.
  obtain ⟨rfl, rfl⟩ := h
  -- Both combine into r folded from a.
  rw [e1, e2]

/-! ## Compaction as steps -/

/-- One compaction step: two adjacent operands are replaced by their partial
merge. -/
inductive Step {O : Type} (pm : O → O → O) : List O → List O → Prop
  /-- Operands `a` and `b`, adjacent between `xs` and `ys`, become one. -/
  | merge (xs ys : List O) (a b : O) : Step pm (xs ++ a :: b :: ys) (xs ++ pm a b :: ys)

/-- Any number of compaction steps. -/
inductive Steps {O : Type} (pm : O → O → O) : List O → List O → Prop
  /-- No step. -/
  | refl (l : List O) : Steps pm l l
  /-- Some steps, then one more. -/
  | tail {l₁ l₂ l₃ : List O} : Steps pm l₁ l₂ → Step pm l₂ l₃ → Steps pm l₁ l₃

/-- One step changes no fold, from any base, when `partial_merge` is exact. -/
theorem step_preserves_fold {V O : Type} {M : Operator V O} {pm : O → O → O}
    -- `partial_merge` is exact.
    (hx : Exact M pm) {l₁ l₂ : List O}
    -- One compaction step.
    (h : Step pm l₁ l₂) (v : V) :
    -- The statement: folding the list before the step equals folding the list after it.
    fullMerge M v l₁ = fullMerge M v l₂ := by
  -- Look at the step: only one kind exists.
  cases h with
  -- Case: a and b, adjacent between xs and ys, became their partial merge.
  | merge xs ys a b =>
    -- The prefix folds the same; then `a` and `b` in turn equal their
    -- partial merge (exactness); the suffix folds the same from there.
    -- Unfold both folds: the prefix xs, then a and b (or their merge), then the suffix ys.
    simp only [fullMerge, List.foldl_append, List.foldl_cons]
    -- Exactness turns "a then b" into "their partial merge": both sides are the same.
    rw [hx]

/-- **Compaction keeps every read.** Any sequence of compaction steps, each
combining two adjacent operands with an exact `partial_merge`, leaves the
fold of the operands unchanged from every base. -/
theorem steps_preserve_fold {V O : Type} {M : Operator V O} {pm : O → O → O}
    -- `partial_merge` is exact.
    (hx : Exact M pm) {l₁ l₂ : List O}
    -- Any number of compaction steps.
    (h : Steps pm l₁ l₂) (v : V) :
    -- The statement: folding the list before the steps equals folding the list after them.
    fullMerge M v l₁ = fullMerge M v l₂ := by
  -- Go through the steps one at a time.
  induction h with
  -- No step: the same list.
  | refl => rfl
  -- Some steps, then one more: the earlier steps keep the fold (the claim so far), and so does the
  -- last one.
  | tail _ hs ih => rw [ih, step_preserves_fold hx hs]

/-! ## The touches law -/

/-- What a projected read sees of a value: `proj v p` is the value of part
`p`, and `touches o S` is the operator's answer to "does operand `o` change
any part in `S`" (`MergeOperator::touches`, default true). -/
structure Parts (V O P W : Type) where
  /-- The value of one part of a value. -/
  proj : V → P → W
  /-- Whether an operand changes any of the named parts. -/
  touches : O → List P → Bool

/-- The contract `touches` keeps: when it answers false for a set of parts,
applying the operand to any value leaves every one of those parts as it
was. Answering true is always safe; that is the default. -/
def TouchesSound {V O P W : Type} (M : Operator V O) (T : Parts V O P W) : Prop :=
  -- For every value, operand and set of parts: if touches answers false, every part of the set
  -- reads the same after the operand as before.
  ∀ v o S, T.touches o S = false → ∀ p ∈ S, T.proj (M.apply v o) p = T.proj v p

/-- **The touches law.** A run of operands none of which touches a part in
`S` leaves every part in `S` as it was: what a projected read over `S`
decided on is unchanged. -/
theorem touches_law {V O P W : Type} {M : Operator V O} {T : Parts V O P W}
    -- `touches` keeps its contract.
    (hs : TouchesSound M T) {S : List P} :
    -- The statement: for every run of operands and every value,
    ∀ (os : List O) (v : V),
      -- No operand touches a part in `S`.
      (∀ o ∈ os, T.touches o S = false) →
      ∀ p ∈ S, T.proj (fullMerge M v os) p = T.proj v p
  -- An empty run: nothing changed.
  | [], _, _, _, _ => rfl
  -- A first operand o and the rest os.
  | o :: os, v, hos, p, hp => by
    -- The rest of the run leaves part `p` as the first operand left it
    -- (the claim for the shorter run, from the new value), and the first
    -- operand left it as it was (the contract).
    -- The claim for the rest of the run, from the value the first operand left
    have ih := touches_law hs os (M.apply v o)
      -- (no later operand touches S, and p is in S).
      (fun o' h' => hos o' (List.mem_cons_of_mem _ h')) p hp
    -- The first operand left part p as it was (the contract).
    have h1 := hs v o S (hos o List.mem_cons_self) p hp
    -- Unfold one step of the fold, in that fact and in the goal.
    simp only [fullMerge, List.foldl_cons] at ih ⊢
    -- Chain the two facts.
    rw [ih, h1]

/-- The contract `touches` keeps across a partial merge (plan 3.3): the
combined operand touches a set of parts exactly when one of its two inputs
does. -/
def TouchesPartial {V O P W : Type} (T : Parts V O P W) (pm : O → O → O) : Prop :=
  -- For every two operands and every set of parts: the merged operand touches the set exactly when
  -- a or b does.
  ∀ a b S, T.touches (pm a b) S = (T.touches a S || T.touches b S)

/-- With that contract, the operand a grouping combines into touches a set
of parts exactly when one of its leaves does. -/
theorem touches_grouping {V O P W : Type} {T : Parts V O P W} {pm : O → O → O}
    -- `touches` of a partial merge is the `or` of its inputs.
    (hp : TouchesPartial T pm) (S : List P) :
    -- The statement: for every grouping, its combined operand touches S exactly when some leaf
    -- does.
    ∀ g : Group O, T.touches (g.eval pm) S = g.leaves.any (fun o => T.touches o S)
  -- A single operand: it is its own only leaf.
  | .leaf o => by simp [Group.eval, Group.leaves]
  -- A node with a left and a right part.
  | .node l r => by
    -- The node's answer is the `or` of its two sides' answers.
    simp only [Group.eval, Group.leaves, List.any_append]
    -- Use the partial-merge contract at the node, and the claim for each side.
    rw [hp, touches_grouping hp S l, touches_grouping hp S r]

/-- **The touches law survives compaction.** A combined operand whose
`touches` answers false for `S` leaves every part in `S` as it was, when
`partial_merge` is exact and `touches` keeps both contracts. -/
theorem touches_survives_compaction {V O P W : Type} {M : Operator V O}
    -- (for any operator, parts and partial merge)
    {T : Parts V O P W} {pm : O → O → O}
    -- `partial_merge` is exact.
    (hx : Exact M pm)
    -- `touches` keeps its contract on single operands.
    (hs : TouchesSound M T)
    -- `touches` of a partial merge is the `or` of its inputs.
    (hp : TouchesPartial T pm) (g : Group O) (S : List P)
    -- The combined operand touches no part in `S`.
    (hg : T.touches (g.eval pm) S = false) (v : V) :
    -- The statement: every part of S reads the same after the combined operand as before.
    ∀ p ∈ S, T.proj (M.apply v (g.eval pm)) p = T.proj v p := by
  -- Take any part p of S.
  intro p hpS
  -- No leaf touches `S`, since their `or` is false.
  have hleaves : ∀ o ∈ g.leaves, T.touches o S = false := by
    -- Take any leaf o.
    intro o ho
    -- The combined operand's answer is the "or" over the leaves, and it is false.
    rw [touches_grouping hp S g] at hg
    -- Split on o's own answer.
    cases h : T.touches o S with
    -- False: that is the claim.
    | false => rfl
    -- True:
    | true =>
      -- then the "or" over the leaves is true,
      have : g.leaves.any (fun o => T.touches o S) = true := List.any_eq_true.mpr ⟨o, ho, h⟩
      -- which contradicts it being false:
      rw [this] at hg
      -- no such case exists.
      cases hg
  -- The combined operand acts as its leaves folded one by one, and those
  -- leave `p` as it was.
  -- Replace the combined operand by its leaves folded one by one.
  rw [fold_eq_grouping hx g v]
  -- And a run of operands none of which touches S leaves p as it was (the touches law).
  exact touches_law hs g.leaves v hleaves p hpS

/-! ## The RED case: an inexact partial merge -/

/-- An operator that rounds down to an even number after every operand, as a
floating-point sum rounds after every addition. -/
def roundSum : Operator Nat Nat := ⟨fun v o => (v + o) / 2 * 2⟩

/-- **`inexact_breaks_fold`.** Declaring "add the operands" as this
operator's partial merge breaks fold equality: folding operands 1 and 1
onto 0 rounds twice and gives 0, while their partial merge 2 applied to 0
gives 2. So compaction with it would change what a read returns. -/
theorem inexact_breaks_fold :
    -- The statement: folding 1 and 1 onto 0 gives 0, but applying their sum 2 to 0 gives 2.
    fullMerge roundSum 0 [1, 1] = 0 ∧
      roundSum.apply 0 ((Group.node (.leaf 1) (.leaf 1)).eval (· + ·)) = 2 := by
  -- Both sides are closed arithmetic.
  decide

/-- The same fact as a failure of the contract: "add the operands" is not an
exact partial merge for `roundSum`. -/
theorem roundSum_not_exact : ¬ Exact roundSum (· + ·) := by
  -- Suppose it were exact.
  intro h
  -- Exactness at value 0 with operands 1 and 1 would say 0 = 2.
  -- Use exactness at value 0 with operands 1 and 1.
  have := h 0 1 1
  -- Put that fact back into the goal, so it can be computed.
  revert this
  -- Computing both sides shows 0 = 2 is false.
  decide

/-! ## The part operand of RepeatableRead.tla keeps the contract -/

/-- The part key of RepeatableRead.tla as a pair of parts, and its operands as
pairs of increments: an operand adds its first number to part 1 and its
second to part 2. The model's operands add one to each part they touch. -/
def partOperator : Operator (Nat × Nat) (Nat × Nat) :=
  -- Add the first increment to part 1 and the second to part 2.
  ⟨fun v o => (v.1 + o.1, v.2 + o.2)⟩

/-- Its partial merge adds the increments. -/
def partMerge (a b : Nat × Nat) : Nat × Nat := (a.1 + b.1, a.2 + b.2)

/-- An operand touches part 1 when it adds to part 1, part 2 when it adds to
part 2, and no other part. -/
def touchesPart (o : Nat × Nat) (p : Nat) : Bool :=
  -- Part 1 and a nonzero first increment, or part 2 and a nonzero second increment.
  (p == 1 && o.1 != 0) || (p == 2 && o.2 != 0)

/-- Parts 1 and 2 of the pair; any other part reads 0. `touches` asks each
named part. -/
def partParts : Parts (Nat × Nat) (Nat × Nat) Nat Nat :=
  -- Reading a part: part 1 is the first number, part 2 the second, any other part 0.
  ⟨fun v p => if p = 1 then v.1 else if p = 2 then v.2 else 0,
   fun o S => S.any (touchesPart o)⟩

/-- Adding increments is an exact partial merge. -/
theorem part_operator_exact : Exact partOperator partMerge := by
  -- Take any value and two operands.
  intro v a b
  -- Componentwise, (x + a) + b = x + (a + b).
  simp [partOperator, partMerge, Nat.add_assoc]

/-- Adding increments is associative. -/
theorem part_operator_assoc : Assoc partMerge := by
  -- Take any three operands.
  intro a b c
  -- Componentwise, (a + b) + c = a + (b + c).
  simp [partMerge, Nat.add_assoc]

/-- Its `touches` keeps the contract: an operand that touches none of the
named parts adds nothing to any of them. -/
theorem part_operator_touches_sound : TouchesSound partOperator partParts := by
  -- Take a value, an operand, a set S of parts on which touches answers false, and a part p of S.
  intro v o S h p hp
  -- `p` is named and the operand touches no named part, so it does not
  -- touch `p`.
  have hp' : touchesPart o p = false := by
    -- Split on whether the operand touches p.
    cases ht : touchesPart o p with
    -- It does not: that is the claim.
    | false => rfl
    -- It does:
    | true =>
      -- then some named part is touched,
      have : S.any (touchesPart o) = true := List.any_eq_true.mpr ⟨p, hp, ht⟩
      -- unfold what touches means for this operator,
      simp only [partParts] at h
      -- so the answer for S would be true,
      rw [this] at h
      -- against it being false.
      cases h
  -- Spell that out: p is not 1 or the first increment is 0, and p is not 2 or the second increment
  -- is 0.
  simp only [touchesPart, Bool.or_eq_false_iff, Bool.and_eq_false_iff,
    bne_eq_false_iff_eq] at hp'
  -- Part 1 untouched means the operand adds 0 to it, and the same for part 2.
  simp only [partParts, partOperator]
  -- Split on whether p is part 1.
  by_cases h1 : p = 1
  -- It is: the first increment is 0, so part 1 is unchanged.
  · subst h1; simp at hp' ⊢; omega
  -- It is not: split on whether p is part 2.
  · by_cases h2 : p = 2
    -- It is: the second increment is 0, so part 2 is unchanged.
    · subst h2; simp at hp' ⊢; omega
    -- It is neither: such a part reads 0 before and after.
    · simp [h1, h2]

/-- A sum of two naturals is nonzero exactly when one of them is. -/
theorem bne_zero_add (x y : Nat) : (x + y != 0) = ((x != 0) || (y != 0)) := by
  -- As propositions: x + y ≠ 0 exactly when x ≠ 0 or y ≠ 0.
  rw [Bool.eq_iff_iff]
  -- Unfold "differs from 0" on both sides.
  simp only [bne_iff_ne, ne_eq, Bool.or_eq_true]
  -- Arithmetic: a sum is nonzero exactly when one of its terms is.
  omega

/-- A combined operand touches one part exactly when one of its inputs does. -/
theorem touchesPart_merge (a b : Nat × Nat) (p : Nat) :
    -- The statement: the merged operand touches p exactly when a or b does.
    touchesPart (partMerge a b) p = (touchesPart a p || touchesPart b p) := by
  -- Unfold; the merged increments are sums, and a sum is nonzero when a term is.
  simp only [touchesPart, partMerge, bne_zero_add]
  -- Every combination of the six booleans involved.
  -- Name "p is part 1" c1.
  generalize (p == 1) = c1
  -- Name "p is part 2" c2.
  generalize (p == 2) = c2
  -- Name "a adds to part 1" x1.
  generalize (a.1 != 0) = x1
  -- Name "b adds to part 1" y1.
  generalize (b.1 != 0) = y1
  -- Name "a adds to part 2" x2.
  generalize (a.2 != 0) = x2
  -- Name "b adds to part 2" y2.
  generalize (b.2 != 0) = y2
  -- Try all 64 combinations of true and false: the two sides agree in each.
  cases c1 <;> cases c2 <;> cases x1 <;> cases y1 <;> cases x2 <;> cases y2 <;> rfl

/-- Its `touches` keeps the partial-merge contract: a combined operand adds to
a part exactly when one of its inputs does. -/
theorem part_operator_touches_partial : TouchesPartial partParts partMerge := by
  -- Take two operands and a set of parts.
  intro a b S
  -- Unfold what touches means for this operator: some named part is touched.
  simp only [partParts]
  -- Part by part, by `touchesPart_merge`.
  induction S with
  -- No parts named: false on both sides.
  | nil => rfl
  -- A first part p and the rest S.
  | cons p S ih =>
    -- Unfold one part; use the claim for the rest and the fact for one part.
    simp only [List.any_cons, ih, touchesPart_merge]
    -- Name "a touches p" x.
    generalize touchesPart a p = x
    -- Name "b touches p" y.
    generalize touchesPart b p = y
    -- Name "a touches some part of the rest" u.
    generalize S.any (touchesPart a) = u
    -- Name "b touches some part of the rest" w.
    generalize S.any (touchesPart b) = w
    -- Try all 16 combinations: the two sides agree in each.
    cases x <;> cases y <;> cases u <;> cases w <;> rfl

-- The end of this file's names.
end Regolith.MergeOperator
