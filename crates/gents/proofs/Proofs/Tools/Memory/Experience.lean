import Mathlib.Data.String.Basic
import Mathlib.Data.Finset.Card
import Mathlib.Data.List.Dedup
import Mathlib.Data.Nat.Find
import Mathlib.Logic.Function.Iterate
import Proofs.Shared.TextNormalization

namespace Tools.Memory.Experience

/-- A problem row (`AgentMemoryProblem`): one root-cause hypothesis of the agent.
Decision (arXiv 2611.18856): memory is organized by root-cause problem, and a
problem row is never rewritten or deleted.
External premise: its key is a SHA-256 hash of the agent's DID and
`normalize name`, which is collision-free, so two rows of one agent share a key
exactly when their normalized root causes are equal. -/
structure ProblemRow where
  problemKey : String
  name : String
  carriedFrom : Option String := none
  createdAt : Nat
  deriving DecidableEq, Repr

/-- An attempt row (`AgentMemoryAttempt`): one committed change, filed under
the problem key it cites, possibly a merged one, with the fixes it predicted
before the change was tested and the executed model of the request that
recorded it, when that request named one. Decision (arXiv 2611.18856): attempts
are append-only, so failures, regressions and later-replaced edits stay in the
record, and only a committed edit records an attempt
(`SelfConfig.StructuredConfig.experienceStaged`). -/
structure AttemptRow where
  attemptKey : String
  problemKey : String
  predicted : List String := []
  accepted : Bool := true
  executedModelName : Option String
  carriedFrom : Option String := none
  createdAt : Nat
  deriving DecidableEq, Repr

/-- `passed` of `total` checks. -/
structure Score where
  passed : Nat
  total : Nat
  deriving DecidableEq, Repr

/-- An outcome row (`AgentMemoryOutcome`): one observation of an attempt, with
its score when it has one and the executed model of the request that observed
it, when that request named one. Decision: an attempt may be observed again,
for example after a model change, so its outcomes are rows of their own; a
score is optional (`scoreAdmitted`), and an outcome recorded without one is
unscored, never 0 of 0. -/
structure OutcomeRow where
  outcomeKey : String
  attemptKey : String
  fixed : List String := []
  missed : List String := []
  score : Option Score := none
  executedModelName : Option String
  carriedFrom : Option String := none
  createdAt : Nat
  deriving DecidableEq, Repr

/-- What `memory outcome` answers for the score it is given. -/
inductive ScoreDecision where
  | scored
  | unscored
  | unpaired
  | negative
  | exceedsTotal
  deriving DecidableEq, Repr

/-- The answer for a score, and the score an admitted outcome records. -/
structure ScoreAdmission where
  decision : ScoreDecision
  score : Option Score
  deriving DecidableEq, Repr

/-- Whether `memory outcome` may record `passed` of `total`, as the tool's
integers give them. Decision (operator, C1): the score is optional, so `passed`
and `total` come together or not at all, and an outcome without them is
`unscored`; one without the other is `unpaired`. A count below zero is
`negative` and more passed than total `exceedsTotal`; each refusal records no
outcome. So a recorded score always has `passed ≤ total`. -/
def scoreAdmitted (passed total : Option Int) : ScoreAdmission :=
  match passed, total with
  | none, none => ⟨.unscored, none⟩
  | some passed, some total =>
    if passed < 0 ∨ total < 0 then ⟨.negative, none⟩
    else if total < passed then ⟨.exceedsTotal, none⟩
    else ⟨.scored, some ⟨passed.toNat, total.toNat⟩⟩
  | _, _ => ⟨.unpaired, none⟩

/-- An outcome records a score exactly when it was given both counts, neither
negative and `passed` at most `total`; the score is those counts. -/
theorem scoreAdmitted_score_iff (passed total : Option Int) (score : Score) :
    (scoreAdmitted passed total).score = some score ↔
      passed = some (score.passed : Int) ∧ total = some (score.total : Int) ∧
        score.passed ≤ score.total := by
  obtain ⟨p, t⟩ := score
  cases passed with
  | none => cases total <;> simp [scoreAdmitted]
  | some passed =>
    cases total with
    | none => simp [scoreAdmitted]
    | some total =>
      simp only [scoreAdmitted]
      split_ifs with hnegative hexceeds
      · simp only [Option.some.injEq, reduceCtorEq, false_iff, not_and]
        omega
      · simp only [Option.some.injEq, reduceCtorEq, false_iff, not_and]
        omega
      · simp only [Option.some.injEq, Score.mk.injEq]
        omega

/-- An outcome is unscored exactly when it was given neither count. -/
theorem scoreAdmitted_unscored_iff (passed total : Option Int) :
    (scoreAdmitted passed total).decision = .unscored ↔ passed = none ∧ total = none := by
  cases passed <;> cases total <;> simp only [scoreAdmitted] <;> (try split_ifs) <;> simp

/-- A refused score records nothing: only `scored` carries a score. -/
theorem scoreAdmitted_scored_iff (passed total : Option Int) :
    (scoreAdmitted passed total).decision = .scored ↔ (scoreAdmitted passed total).score.isSome := by
  cases passed <;> cases total <;> simp only [scoreAdmitted] <;> (try split_ifs) <;> simp

/-- The rows an agent's experience is folded from; `aliases` holds the
`AgentMemoryAlias` rows as (`from_problem_key`, `to_problem_key`). -/
structure Rows where
  problems : List ProblemRow := []
  attempts : List AttemptRow := []
  outcomes : List OutcomeRow := []
  aliases : List (String × String) := []

/-- The identity text of a root cause. Decision (arXiv 2611.18856: identical
root-cause text merges deterministically): whitespace runs become one space,
leading and trailing whitespace is dropped and ASCII letters are lowercased;
nothing else is folded, so text differing in any other character, a non-ASCII
letter's case included, is another problem. Whitespace is Rust's
`char::is_whitespace` (`TextNormalization.objectiveWhitespace`), so the Rust
owner is `split_whitespace` joined by one space, then `to_ascii_lowercase`. -/
def normalize (text : String) : String :=
  (" ".intercalate ((text.split TextNormalization.objectiveWhitespace).filter
    fun piece => !piece.isEmpty)).toLower

/-- The existing problem a root cause lands on, among the names the agent's
problems were opened with: the first whose normalized text equals the root
cause's; `none` opens a new problem. Decision (arXiv 2611.18856): the agent
decides which problem a diagnosis belongs to and the runtime never merges by
similarity; only identical normalized text merges. External premise
(`ProblemRow`): production derives a problem's key from the agent's DID and
`normalize name`, collision-free, and opens a problem under that key, so a
root cause lands on an existing problem exactly when it derives that
problem's key. -/
def landsOn (existing : List String) (rootCause : String) : Option String :=
  existing.find? fun name => normalize name == normalize rootCause

theorem landsOn_some {existing : List String} {rootCause name : String}
    (h : landsOn existing rootCause = some name) :
    name ∈ existing ∧ normalize name = normalize rootCause :=
  ⟨List.mem_of_find?_eq_some h, by simpa using List.find?_some h⟩

/-- A root cause opens a new problem exactly when no existing name normalizes
to its text. -/
theorem landsOn_none_iff (existing : List String) (rootCause : String) :
    landsOn existing rootCause = none ↔ ∀ name ∈ existing, normalize name ≠ normalize rootCause := by
  simp [landsOn, List.find?_eq_none]

/-- The key an alias row merges `key` into: the first row naming it. The store
holds at most one, since `from_problem_key` is unique. -/
def next (aliases : List (String × String)) (key : String) : Option String :=
  (aliases.find? (·.1 == key)).map (·.2)

/-- One alias step; a key no row merges stays where it is. -/
def step (aliases : List (String × String)) (key : String) : String :=
  (next aliases key).getD key

/-- The least key of the cycle a walk closed by coming back to `key`: `key` and
every key visited after it. -/
def cycleLeast (seen : List String) (key : String) : String :=
  (seen.dropWhile (· != key)).foldl min key

def resolveSeen (aliases : List (String × String)) : Nat → List String → String → String
  | 0, _, key => key
  | fuel + 1, seen, key =>
    if key ∈ seen then cycleLeast seen key
    else match next aliases key with
      | some target => resolveSeen aliases fuel (seen ++ [key]) target
      | none => key

/-- The problem a cited key stands for. Decision: follow the alias rows with
fuel `aliases.length + 1`, as `ClientShell.SessionTurn.resolveSeen` does; a walk
that comes back to a key lands on the least key of the cycle it closed, so the
keys of one merged problem resolve alike even when concurrent merges left a
cycle. The fuel never runs out (`resolve_leastRecurrent`). Keys compare by code
point, which is Rust's `str` order. TLA+ `MemoryAliasMerge` restates this walk. -/
def resolve (aliases : List (String × String)) (key : String) : String :=
  resolveSeen aliases (aliases.length + 1) [] key

/-- `target` is visited infinitely often by the walk `s` from `key`. -/
def Recurrent (s : String → String) (key target : String) : Prop :=
  ∀ bound, ∃ n, bound ≤ n ∧ s^[n] key = target

/-- `least` is the least key the walk `s` from `key` visits infinitely often:
the key no alias row merges that it ends on, or the least key of the cycle it
enters. -/
def LeastRecurrent (s : String → String) (key least : String) : Prop :=
  Recurrent s key least ∧ ∀ target, Recurrent s key target → least ≤ target

theorem LeastRecurrent.unique {s : String → String} {key first second : String}
    (h₁ : LeastRecurrent s key first) (h₂ : LeastRecurrent s key second) : first = second :=
  le_antisymm (h₁.2 _ h₂.1) (h₂.2 _ h₁.1)

theorem iterate_shift {s : String → String} {key : String} {i c : Nat}
    (h : s^[i + c] key = s^[i] key) (m : Nat) : s^[i + m + c] key = s^[i + m] key := by
  rw [show i + m + c = m + (i + c) by omega, Function.iterate_add_apply, h,
    ← Function.iterate_add_apply, Nat.add_comm m i]

theorem iterate_shift_mul {s : String → String} {key : String} {i c : Nat}
    (h : s^[i + c] key = s^[i] key) (m t : Nat) : s^[i + m + c * t] key = s^[i + m] key := by
  induction t generalizing m with
  | zero => simp
  | succ t ih =>
    rw [Nat.mul_succ, show i + m + (c * t + c) = i + (m + c * t) + c by omega,
      iterate_shift h, ← Nat.add_assoc, ih]

/-- Once a walk repeats after `c` steps from step `i`, the keys it visits
infinitely often are the `c` keys from step `i` on. -/
theorem recurrent_iff {s : String → String} {key : String} {i c : Nat} (hc : 0 < c)
    (h : s^[i + c] key = s^[i] key) (target : String) :
    Recurrent s key target ↔ ∃ t, t < c ∧ s^[i + t] key = target := by
  constructor
  · intro hrec
    obtain ⟨n, hn, rfl⟩ := hrec i
    refine ⟨(n - i) % c, Nat.mod_lt _ hc, ?_⟩
    have hsplit := Nat.mod_add_div (n - i) c
    have hshift := iterate_shift_mul h ((n - i) % c) ((n - i) / c)
    rw [show i + (n - i) % c + c * ((n - i) / c) = n by omega] at hshift
    exact hshift.symm
  · rintro ⟨t, -, rfl⟩ bound
    refine ⟨i + t + c * bound, ?_, iterate_shift_mul h t bound⟩
    have := Nat.le_mul_of_pos_left bound hc
    omega

theorem recurrent_iterate {s : String → String} {key target : String} (m : Nat) :
    Recurrent s (s^[m] key) target ↔ Recurrent s key target := by
  constructor
  · intro h bound
    obtain ⟨n, hn, hy⟩ := h bound
    exact ⟨n + m, by omega, by rw [Function.iterate_add_apply]; exact hy⟩
  · intro h bound
    obtain ⟨n, hn, hy⟩ := h (bound + m)
    refine ⟨n - m, by omega, ?_⟩
    rw [← Function.iterate_add_apply, Nat.sub_add_cancel (by omega)]
    exact hy

theorem foldl_min_mem (first : String) (rest : List String) :
    rest.foldl min first ∈ first :: rest := by
  induction rest generalizing first with
  | nil => simp
  | cons x rest ih =>
    rw [List.foldl_cons]
    rcases min_choice first x with hm | hm <;> rw [hm]
    · rcases List.mem_cons.mp (ih first) with h | h <;> simp [h]
    · rcases List.mem_cons.mp (ih x) with h | h <;> simp [h]

theorem foldl_min_le (first : String) (rest : List String) :
    ∀ target ∈ first :: rest, rest.foldl min first ≤ target := by
  induction rest generalizing first with
  | nil =>
    intro target htarget
    rw [List.mem_singleton] at htarget
    simp [htarget]
  | cons x rest ih =>
    intro target htarget
    rw [List.foldl_cons]
    rcases List.mem_cons.mp htarget with rfl | htarget
    · exact (ih (min target x) _ (List.mem_cons_self _ _)).trans (min_le_left _ _)
    · rcases List.mem_cons.mp htarget with rfl | htarget
      · exact (ih (min first target) _ (List.mem_cons_self _ _)).trans (min_le_right _ _)
      · exact ih (min first x) target (List.mem_cons_of_mem _ htarget)

theorem leastRecurrent_of_members {s : String → String} {key first : String}
    {rest : List String} (h : ∀ target, target ∈ first :: rest ↔ Recurrent s key target) :
    LeastRecurrent s key (rest.foldl min first) :=
  ⟨(h _).1 (foldl_min_mem first rest),
    fun target htarget => foldl_min_le first rest target ((h target).2 htarget)⟩

theorem mem_of_next {aliases : List (String × String)} {key : String}
    (h : next aliases key ≠ none) : key ∈ aliases.map (·.1) := by
  unfold next at h
  cases hfind : aliases.find? (·.1 == key) with
  | none => simp [hfind] at h
  | some entry =>
    have hkey := List.find?_some hfind
    simp only [beq_iff_eq] at hkey
    exact List.mem_map.mpr ⟨entry, List.mem_of_find?_eq_some hfind, hkey⟩

/-- A walk visits each merged key at most once before it repeats, so it visits
at most as many merged keys as there are alias rows. -/
theorem visited_le_aliases {aliases : List (String × String)} {seen : List String}
    (hnodup : seen.Nodup) (hmerged : ∀ key ∈ seen, next aliases key ≠ none) :
    seen.length ≤ aliases.length := by
  classical
  rw [← List.toFinset_card_of_nodup hnodup]
  calc seen.toFinset.card ≤ (aliases.map (·.1)).toFinset.card :=
        Finset.card_le_card fun key hkey => by
          rw [List.mem_toFinset] at hkey ⊢
          exact mem_of_next (hmerged key hkey)
    _ ≤ (aliases.map (·.1)).length := List.toFinset_card_le _
    _ = aliases.length := List.length_map _ _

theorem dropWhile_ne_of_not_mem {before after : List String} {key : String}
    (hkey : key ∉ before) : (before ++ key :: after).dropWhile (· != key) = key :: after := by
  induction before with
  | nil => simp
  | cons other before ih =>
    have hother : other ≠ key := fun e => hkey (e ▸ List.mem_cons_self _ _)
    rw [List.cons_append, List.dropWhile_cons_of_pos (by simpa using hother)]
    exact ih fun h => hkey (List.mem_cons_of_mem _ h)

theorem resolveSeen_leastRecurrent (aliases : List (String × String)) (key : String) :
    ∀ fuel n, fuel + n = aliases.length + 1 →
      ((List.range n).map fun t => (step aliases)^[t] key).Nodup →
      (∀ t < n, next aliases ((step aliases)^[t] key) ≠ none) →
      LeastRecurrent (step aliases) key
        (resolveSeen aliases fuel ((List.range n).map fun t => (step aliases)^[t] key)
          ((step aliases)^[n] key)) := by
  intro fuel
  induction fuel with
  | zero =>
    intro n hn hnodup hnext
    exfalso
    have hle := visited_le_aliases hnodup fun x hx => by
      obtain ⟨t, ht, rfl⟩ := List.mem_map.mp hx
      exact hnext t (List.mem_range.mp ht)
    simp only [List.length_map, List.length_range] at hle
    omega
  | succ fuel ih =>
    intro n hn hnodup hnext
    simp only [resolveSeen]
    split_ifs with hseen
    · obtain ⟨j, hjrange, hjn⟩ := List.mem_map.mp hseen
      have hj : j < n := List.mem_range.mp hjrange
      have hc : 0 < n - j := by omega
      have hper : (step aliases)^[j + (n - j)] key = (step aliases)^[j] key := by
        rw [Nat.add_sub_cancel' hj.le]
        exact hjn.symm
      have hsplit : ((List.range n).map fun t => (step aliases)^[t] key) =
          ((List.range j).map fun t => (step aliases)^[t] key) ++
            (List.range (n - j)).map fun t => (step aliases)^[j + t] key := by
        conv_lhs => rw [show n = j + (n - j) by omega, List.range_add]
        rw [List.map_append, List.map_map]
        rfl
      have hfirst : (step aliases)^[n] key ∉ (List.range j).map fun t => (step aliases)^[t] key := by
        intro hin
        rw [hsplit] at hnodup
        exact List.disjoint_of_nodup_append hnodup hin
          (List.mem_map.mpr ⟨0, List.mem_range.mpr hc, by simpa using hjn⟩)
      have hdrop : (((List.range n).map fun t => (step aliases)^[t] key).dropWhile
          (· != (step aliases)^[n] key)) =
          (List.range (n - j)).map fun t => (step aliases)^[j + t] key := by
        obtain ⟨m, hm⟩ : ∃ m, n - j = m + 1 := ⟨n - j - 1, by omega⟩
        rw [hsplit, hm, List.range_succ_eq_map, List.map_cons, Nat.add_zero, hjn]
        exact dropWhile_ne_of_not_mem hfirst
      unfold cycleLeast
      rw [hdrop]
      apply leastRecurrent_of_members
      intro target
      rw [recurrent_iff hc hper]
      simp only [List.mem_cons, List.mem_map, List.mem_range]
      constructor
      · rintro (rfl | ⟨t, ht, rfl⟩)
        · exact ⟨0, hc, by simpa using hjn⟩
        · exact ⟨t, ht, rfl⟩
      · rintro ⟨t, ht, rfl⟩
        exact Or.inr ⟨t, ht, rfl⟩
    · cases hnextn : next aliases ((step aliases)^[n] key) with
      | none =>
        have hper : (step aliases)^[n + 1] key = (step aliases)^[n] key := by
          rw [Function.iterate_succ_apply']
          simp [step, hnextn]
        show LeastRecurrent (step aliases) key ([].foldl min ((step aliases)^[n] key))
        apply leastRecurrent_of_members
        intro target
        rw [recurrent_iff Nat.one_pos hper]
        simp only [List.mem_singleton]
        constructor
        · rintro rfl
          exact ⟨0, Nat.one_pos, rfl⟩
        · rintro ⟨t, ht, rfl⟩
          obtain rfl : t = 0 := by omega
          rfl
      | some target =>
        have htarget : target = (step aliases)^[n + 1] key := by
          rw [Function.iterate_succ_apply']
          simp [step, hnextn]
        have hseen' : ((List.range n).map fun t => (step aliases)^[t] key) ++
            [(step aliases)^[n] key] = (List.range (n + 1)).map fun t => (step aliases)^[t] key := by
          rw [List.range_succ, List.map_append]
          rfl
        simp only
        rw [hseen', htarget]
        apply ih (n + 1) (by omega)
        · rw [← hseen']
          exact hnodup.append (List.nodup_singleton _) (List.disjoint_singleton.mpr hseen)
        · intro t ht
          rcases Nat.lt_succ_iff_lt_or_eq.mp ht with ht | rfl
          · exact hnext t ht
          · simp [hnextn]

/-- `resolve` lands on the least key the walk visits infinitely often: a key no
alias row merges, or the least key of the cycle the walk enters. Its fuel never
runs out. -/
theorem resolve_leastRecurrent (aliases : List (String × String)) (key : String) :
    LeastRecurrent (step aliases) key (resolve aliases key) := by
  have h := resolveSeen_leastRecurrent aliases key (aliases.length + 1) 0 (by omega)
    (by simp) (by simp)
  simpa [resolve] using h

theorem resolve_eq_of_recurrent {aliases other : List (String × String)} {key key' : String}
    (h : ∀ target, Recurrent (step aliases) key target ↔ Recurrent (step other) key' target) :
    resolve aliases key = resolve other key' :=
  (resolve_leastRecurrent aliases key).unique
    ⟨(h _).2 (resolve_leastRecurrent other key').1,
      fun target htarget => (resolve_leastRecurrent other key').2 target ((h target).1 htarget)⟩

/-- A key and every key its walk reaches resolve alike. -/
theorem resolve_iterate (aliases : List (String × String)) (key : String) (m : Nat) :
    resolve aliases ((step aliases)^[m] key) = resolve aliases key :=
  resolve_eq_of_recurrent fun _ => recurrent_iterate m

theorem resolve_idempotent (aliases : List (String × String)) (key : String) :
    resolve aliases (resolve aliases key) = resolve aliases key := by
  obtain ⟨n, -, hn⟩ := (resolve_leastRecurrent aliases key).1 0
  calc resolve aliases (resolve aliases key)
      = resolve aliases ((step aliases)^[n] key) := by rw [hn]
    _ = resolve aliases key := resolve_iterate aliases key n

/-- A walk that reaches a key no alias row merges lands on it. -/
theorem resolve_of_reaches {aliases : List (String × String)} {key target : String} {n : Nat}
    (hn : (step aliases)^[n] key = target) (htarget : next aliases target = none) :
    resolve aliases key = target := by
  have hper : (step aliases)^[n + 1] key = (step aliases)^[n] key := by
    rw [Function.iterate_succ_apply', hn]
    simp [step, htarget]
  obtain ⟨t, ht, heq⟩ :=
    (recurrent_iff Nat.one_pos hper _).1 (resolve_leastRecurrent aliases key).1
  obtain rfl : t = 0 := by omega
  rw [← heq, ← hn]
  rfl

theorem resolve_unmerged {aliases : List (String × String)} {key : String}
    (h : next aliases key = none) : resolve aliases key = key :=
  resolve_of_reaches (n := 0) rfl h

/-- Every key of a cycle of alias rows resolves to the least key of the
cycle. -/
theorem resolve_on_cycle (aliases : List (String × String)) (key : String) (c : Nat)
    (hc : 0 < c) (hcycle : (step aliases)^[c] key = key) (m : Nat) :
    resolve aliases ((step aliases)^[m] key) =
      ((List.range c).map fun t => (step aliases)^[t] key).foldl min key := by
  rw [resolve_iterate]
  have hper : (step aliases)^[0 + c] key = (step aliases)^[0] key := by simpa using hcycle
  refine (resolve_leastRecurrent aliases key).unique (leastRecurrent_of_members fun target => ?_)
  rw [recurrent_iff hc hper]
  simp only [List.mem_cons, List.mem_map, List.mem_range, Nat.zero_add]
  constructor
  · rintro (rfl | ⟨t, ht, rfl⟩)
    · exact ⟨0, hc, rfl⟩
    · exact ⟨t, ht, rfl⟩
  · rintro ⟨t, ht, rfl⟩
    exact Or.inr ⟨t, ht, rfl⟩

/-- What `memory merge` answers. -/
inductive MergeDecision where
  | admitted
  | unknownKey
  | sameKey
  | alreadyMerged
  | closesCycle
  deriving DecidableEq, Repr

/-- Whether `memory merge` may append the alias row `fromKey -> toKey`.
Decision (arXiv 2611.18856, merged ids are kept as aliases): a merge appends an
alias row and changes no attempt or problem. It is refused when a key names no
problem row, when both keys are one, when `fromKey` is already merged (its
unique `from_problem_key` row exists), and when `toKey` already resolves to
`fromKey`, since the row would close a cycle. The agent decides what to merge;
nothing merges by similarity. -/
def mergeAdmitted (problems : List String) (aliases : List (String × String))
    (fromKey toKey : String) : MergeDecision :=
  if !(problems.contains fromKey && problems.contains toKey) then .unknownKey
  else if fromKey == toKey then .sameKey
  else if (next aliases fromKey).isSome then .alreadyMerged
  else if resolve aliases toKey == fromKey then .closesCycle
  else .admitted

theorem mergeAdmitted_iff (problems : List String) (aliases : List (String × String))
    (fromKey toKey : String) :
    mergeAdmitted problems aliases fromKey toKey = .admitted ↔
      fromKey ∈ problems ∧ toKey ∈ problems ∧ fromKey ≠ toKey ∧
        next aliases fromKey = none ∧ resolve aliases toKey ≠ fromKey := by
  unfold mergeAdmitted
  by_cases hknown : fromKey ∈ problems ∧ toKey ∈ problems
  · obtain ⟨hfrom, hto⟩ := hknown
    by_cases hsame : fromKey = toKey
    · simp [hfrom, hto, hsame]
    · by_cases hmerged : next aliases fromKey = none
      · by_cases hcycle : resolve aliases toKey = fromKey
        · simp [hfrom, hto, hsame, hmerged, hcycle]
        · simp [hfrom, hto, hsame, hmerged, hcycle]
      · obtain ⟨target, htarget⟩ := Option.ne_none_iff_exists'.mp hmerged
        simp [hfrom, hto, hsame, htarget]
  · rw [not_and_or] at hknown
    rcases hknown with hfrom | hto
    · simp [hfrom]
    · simp [hto]

theorem next_merge {aliases : List (String × String)} {fromKey toKey key : String}
    (hkey : key ≠ fromKey) : next (aliases ++ [(fromKey, toKey)]) key = next aliases key := by
  unfold next
  rw [List.find?_append]
  have hnone : [(fromKey, toKey)].find? (·.1 == key) = none := by
    simp [Ne.symm hkey]
  rw [hnone, Option.or_none]

theorem step_merge {aliases : List (String × String)} {fromKey toKey : String}
    (hfrom : next aliases fromKey = none) (key : String) :
    step (aliases ++ [(fromKey, toKey)]) key = if key = fromKey then toKey else step aliases key := by
  by_cases hkey : key = fromKey
  · subst hkey
    have hnone : aliases.find? (·.1 == key) = none := by
      unfold next at hfrom
      cases h : aliases.find? (·.1 == key) <;> simp_all
    simp [step, next, List.find?_append, hnone]
  · simp [step, next_merge hkey, hkey]

theorem merge_iterate_of_avoids {aliases : List (String × String)} {fromKey toKey key : String}
    (hfrom : next aliases fromKey = none) {n : Nat}
    (h : ∀ j < n, (step aliases)^[j] key ≠ fromKey) :
    (step (aliases ++ [(fromKey, toKey)]))^[n] key = (step aliases)^[n] key := by
  induction n with
  | zero => rfl
  | succ n ih =>
    rw [Function.iterate_succ_apply', Function.iterate_succ_apply',
      ih fun j hj => h j (by omega), step_merge hfrom, if_neg (h n (by omega))]

theorem merge_iterate_of_avoids' {aliases : List (String × String)} {fromKey toKey key : String}
    (hfrom : next aliases fromKey = none) {n : Nat}
    (h : ∀ j < n, (step (aliases ++ [(fromKey, toKey)]))^[j] key ≠ fromKey) :
    (step (aliases ++ [(fromKey, toKey)]))^[n] key = (step aliases)^[n] key := by
  induction n with
  | zero => rfl
  | succ n ih =>
    rw [Function.iterate_succ_apply', Function.iterate_succ_apply', step_merge hfrom,
      if_neg (h n (by omega)), ih fun j hj => h j (by omega)]

theorem resolve_merge_of_ne {aliases : List (String × String)} {fromKey toKey key : String}
    (hfrom : next aliases fromKey = none) (h : resolve aliases key ≠ fromKey) :
    resolve (aliases ++ [(fromKey, toKey)]) key = resolve aliases key := by
  have hsame : ∀ n, (step (aliases ++ [(fromKey, toKey)]))^[n] key = (step aliases)^[n] key :=
    fun _ => merge_iterate_of_avoids hfrom fun j _ hj => h (resolve_of_reaches hj hfrom)
  exact resolve_eq_of_recurrent fun target => by simp only [Recurrent, hsame]

theorem resolve_merge_of_eq {aliases : List (String × String)} {fromKey toKey key : String}
    (hfrom : next aliases fromKey = none) (h : resolve aliases key = fromKey) :
    resolve (aliases ++ [(fromKey, toKey)]) key = resolve (aliases ++ [(fromKey, toKey)]) toKey := by
  have hex : ∃ n, (step aliases)^[n] key = fromKey := by
    obtain ⟨n, -, hn⟩ := (resolve_leastRecurrent aliases key).1 0
    exact ⟨n, hn.trans h⟩
  have hreach : (step (aliases ++ [(fromKey, toKey)]))^[Nat.find hex + 1] key = toKey := by
    rw [Function.iterate_succ_apply',
      merge_iterate_of_avoids hfrom fun j hj => Nat.find_min hex hj, Nat.find_spec hex,
      step_merge hfrom, if_pos rfl]
  calc resolve (aliases ++ [(fromKey, toKey)]) key
      = resolve (aliases ++ [(fromKey, toKey)])
          ((step (aliases ++ [(fromKey, toKey)]))^[Nat.find hex + 1] key) :=
        (resolve_iterate _ _ _).symm
    _ = resolve (aliases ++ [(fromKey, toKey)]) toKey := congrArg _ hreach

/-- An admitted merge files both problems under one: every key that resolved to
`fromKey` or alike `toKey` now resolves where `toKey` did. -/
theorem admitted_merge_joins_both_problems (problems : List String)
    (aliases : List (String × String)) (fromKey toKey key : String)
    (h : mergeAdmitted problems aliases fromKey toKey = .admitted)
    (hkey : resolve aliases key = fromKey ∨ resolve aliases key = resolve aliases toKey) :
    resolve (aliases ++ [(fromKey, toKey)]) key = resolve aliases toKey := by
  obtain ⟨-, -, -, hfrom, hcycle⟩ := (mergeAdmitted_iff problems aliases fromKey toKey).1 h
  have hto : resolve (aliases ++ [(fromKey, toKey)]) toKey = resolve aliases toKey :=
    resolve_merge_of_ne hfrom hcycle
  rcases hkey with hkey | hkey
  · rw [resolve_merge_of_eq hfrom hkey, hto]
  · rw [resolve_merge_of_ne hfrom (hkey ▸ hcycle), hkey]

/-- An admitted merge moves no key of any other problem. -/
theorem admitted_merge_moves_only_the_merged_problem (problems : List String)
    (aliases : List (String × String)) (fromKey toKey key : String)
    (h : mergeAdmitted problems aliases fromKey toKey = .admitted)
    (hkey : resolve aliases key ≠ fromKey) :
    resolve (aliases ++ [(fromKey, toKey)]) key = resolve aliases key :=
  resolve_merge_of_ne ((mergeAdmitted_iff problems aliases fromKey toKey).1 h).2.2.2.1 hkey

/-- No key comes back to itself through the alias rows. -/
def Acyclic (aliases : List (String × String)) : Prop :=
  ∀ key n, 0 < n → (step aliases)^[n] key = key → next aliases key = none

/-- From an acyclic alias set, an admitted merge leaves it acyclic. -/
theorem admitted_merge_keeps_aliases_acyclic (problems : List String)
    (aliases : List (String × String)) (fromKey toKey : String) (hacyclic : Acyclic aliases)
    (h : mergeAdmitted problems aliases fromKey toKey = .admitted) :
    Acyclic (aliases ++ [(fromKey, toKey)]) := by
  obtain ⟨-, -, -, hfrom, hcycle⟩ := (mergeAdmitted_iff problems aliases fromKey toKey).1 h
  intro key n hn hloop
  by_cases hvisit : ∃ j < n, (step aliases)^[j] key = fromKey
  · exfalso
    have hex : ∃ j, (step aliases)^[j] key = fromKey :=
      let ⟨j, _, hj⟩ := hvisit
      ⟨j, hj⟩
    have hmerged : (step (aliases ++ [(fromKey, toKey)]))^[Nat.find hex] key = fromKey := by
      rw [merge_iterate_of_avoids hfrom fun j hj => Nat.find_min hex hj, Nat.find_spec hex]
    have hback : (step (aliases ++ [(fromKey, toKey)]))^[n] fromKey = fromKey := by
      calc (step (aliases ++ [(fromKey, toKey)]))^[n] fromKey
          = (step (aliases ++ [(fromKey, toKey)]))^[n]
              ((step (aliases ++ [(fromKey, toKey)]))^[Nat.find hex] key) := by rw [hmerged]
        _ = (step (aliases ++ [(fromKey, toKey)]))^[Nat.find hex]
              ((step (aliases ++ [(fromKey, toKey)]))^[n] key) := by
          rw [← Function.iterate_add_apply, ← Function.iterate_add_apply, Nat.add_comm]
        _ = fromKey := by rw [hloop, hmerged]
    obtain ⟨n', rfl⟩ : ∃ n', n = n' + 1 := ⟨n - 1, by omega⟩
    rw [Function.iterate_succ_apply, step_merge hfrom, if_pos rfl] at hback
    have hex' : ∃ i, (step (aliases ++ [(fromKey, toKey)]))^[i] toKey = fromKey := ⟨n', hback⟩
    have hold : (step aliases)^[Nat.find hex'] toKey = fromKey := by
      rw [← merge_iterate_of_avoids' hfrom fun j hj => Nat.find_min hex' hj]
      exact Nat.find_spec hex'
    exact hcycle (resolve_of_reaches hold hfrom)
  · push_neg at hvisit
    have hold : (step aliases)^[n] key = key := by
      rw [← merge_iterate_of_avoids hfrom hvisit]
      exact hloop
    have hkey : key ≠ fromKey := by simpa using hvisit 0 hn
    rw [next_merge hkey]
    exact hacyclic key n hn hold

/-- Where an attempt or an outcome was recorded, relative to the reader. -/
inductive Provenance where
  | same
  | otherModel
  | unknownModel
  | otherAgent
  | otherModelAndAgent
  | unknownModelAndOtherAgent
  deriving DecidableEq, Repr

/-- The reader's setting: its own DID and the request's executed model, when
the request names one. -/
structure Current where
  agentDid : String
  executedModelName : Option String
  deriving DecidableEq, Repr

/-- A model name a relation can be decided on: present and not empty. -/
def knownModel (name : Option String) : Option String :=
  name.filter fun model => !model.isEmpty

/-- Where a row was recorded, relative to the reader. Decision (arXiv
2611.18856, reading rule D.2.8): evidence recorded under another policy model or
by another agent is flagged, never reweighted or hidden. The model is the
executed model name of the request that recorded the row, against the
reader's; the agent is the one the row was carried from. Decision (operator,
M4): a model whose name is absent or empty is unknown, and a relation with an
unknown model on either side is unknown, never the same model or another
one. -/
def provenance (current : Current) (model carriedFrom : Option String) : Provenance :=
  let otherAgent := carriedFrom.any (· != current.agentDid)
  match knownModel current.executedModelName, knownModel model with
  | some reader, some recorded =>
    match reader != recorded, otherAgent with
    | false, false => .same
    | true, false => .otherModel
    | false, true => .otherAgent
    | true, true => .otherModelAndAgent
  | _, _ => if otherAgent then .unknownModelAndOtherAgent else .unknownModel

theorem knownModel_empty : knownModel (some "") = none := rfl

/-- The relation is unknown exactly when either side's model is: two models
are compared only when both are named. -/
theorem provenance_unknown_iff (current : Current) (model carriedFrom : Option String) :
    (provenance current model carriedFrom = .unknownModel ∨
      provenance current model carriedFrom = .unknownModelAndOtherAgent) ↔
      knownModel current.executedModelName = none ∨ knownModel model = none := by
  unfold provenance
  cases knownModel current.executedModelName <;> cases knownModel model <;>
    simp only [reduceCtorEq, or_self, or_true, true_or, iff_true, iff_false] <;>
    split <;> simp_all

/-- One observation of an attempt as the reader sees it: its score, if it has
one, and where it was recorded. -/
structure Observation where
  outcomeKey : String
  score : Option Score
  provenance : Provenance
  deriving DecidableEq, Repr

structure FoldedAttempt where
  attemptKey : String
  citedProblemKey : String
  accepted : Bool
  predicted : List String
  fixed : List String
  missed : List String
  unresolved : List String
  outcomes : List Observation
  provenance : Provenance
  createdAt : Nat
  deriving DecidableEq, Repr

structure FoldedProblem where
  problemKey : String
  name : Option String
  carriedFrom : Option String
  merged : List String
  attempts : List FoldedAttempt
  latest : Option Observation
  lastActive : Nat
  deriving DecidableEq, Repr

set_option linter.dupNamespace false in
structure Experience where
  problems : List FoldedProblem
  unattached : List String
  deriving DecidableEq, Repr

def outcomeBefore (first second : OutcomeRow) : Bool :=
  first.createdAt < second.createdAt ||
    (first.createdAt == second.createdAt && first.outcomeKey ≤ second.outcomeKey)

def attemptBefore (first second : AttemptRow) : Bool :=
  first.createdAt < second.createdAt ||
    (first.createdAt == second.createdAt && first.attemptKey ≤ second.attemptKey)

/-- An attempt's outcomes, oldest first. -/
def observations (outcomes : List OutcomeRow) (attemptKey : String) : List OutcomeRow :=
  (outcomes.filter (·.attemptKey == attemptKey)).mergeSort outcomeBefore

def observe (current : Current) (outcome : OutcomeRow) : Observation :=
  { outcomeKey := outcome.outcomeKey
    score := outcome.score
    provenance := provenance current outcome.executedModelName outcome.carriedFrom }

/-- An attempt as the reader sees it. Decision: a prediction is unresolved while
no outcome names it fixed or missed; fixed and missed gather every observation,
so a prediction fixed under one model and missed under another shows both.
Each observation keeps its own score and where it was recorded, oldest
first. -/
def foldAttempt (current : Current) (outcomes : List OutcomeRow) (attempt : AttemptRow) :
    FoldedAttempt :=
  let observed := observations outcomes attempt.attemptKey
  let fixed := (observed.flatMap (·.fixed)).eraseDups
  let missed := (observed.flatMap (·.missed)).eraseDups
  { attemptKey := attempt.attemptKey
    citedProblemKey := attempt.problemKey
    accepted := attempt.accepted
    predicted := attempt.predicted
    fixed
    missed
    unresolved := attempt.predicted.eraseDups.filter fun prediction =>
      !(fixed.contains prediction || missed.contains prediction)
    outcomes := observed.map (observe current)
    provenance := provenance current attempt.executedModelName attempt.carriedFrom
    createdAt := attempt.createdAt }

/-- The problems the rows hold: every key a problem row or an attempt resolves
to, in key order. -/
def problemKeys (rows : Rows) : List String :=
  ((rows.problems.map fun problem => resolve rows.aliases problem.problemKey) ++
    rows.attempts.map fun attempt => resolve rows.aliases attempt.problemKey).dedup.mergeSort
    fun first second => first ≤ second

def foldProblem (current : Current) (rows : Rows) (key : String) : FoldedProblem :=
  let row := rows.problems.find? (·.problemKey == key)
  let filed := (rows.attempts.filter fun attempt =>
    resolve rows.aliases attempt.problemKey == key).mergeSort attemptBefore
  let observed := (rows.outcomes.filter fun outcome =>
    filed.any (·.attemptKey == outcome.attemptKey)).mergeSort outcomeBefore
  { problemKey := key
    name := row.map (·.name)
    carriedFrom := row.bind (·.carriedFrom)
    merged := ((rows.problems.map (·.problemKey)).filter fun other =>
      other != key && resolve rows.aliases other == key).dedup.mergeSort
      fun first second => first ≤ second
    attempts := filed.map (foldAttempt current rows.outcomes)
    latest := observed.getLast?.map (observe current)
    lastActive := ((row.map (·.createdAt)).toList ++ filed.map (·.createdAt) ++
      observed.map (·.createdAt)).foldl max 0 }

/-- The experience the rows record. Decision (arXiv 2611.18856): memory is
organized by root-cause problem; each attempt is filed under the problem its
cited key resolves to, merged keys included, oldest first. Nothing is dropped:
an attempt citing a key no problem row holds is filed under that key, and an
outcome whose attempt is not among the rows is listed unattached. -/
def fold (current : Current) (rows : Rows) : Experience :=
  { problems := (problemKeys rows).map (foldProblem current rows)
    unattached := ((rows.outcomes.filter fun outcome =>
      !rows.attempts.any (·.attemptKey == outcome.attemptKey)).mergeSort outcomeBefore).map
        (·.outcomeKey) }

theorem flatMap_filter_perm {α : Type} (key : α → String) (items : List α) :
    ∀ keys : List String, keys.Nodup →
      (keys.flatMap fun k => items.filter fun item => key item == k).Perm
        (items.filter fun item => keys.contains (key item))
  | [], _ => by simp
  | k :: keys, hnodup => by
    obtain ⟨hk, hnodup⟩ := List.nodup_cons.mp hnodup
    rw [List.flatMap_cons]
    refine ((flatMap_filter_perm key items keys hnodup).append_left _).trans ?_
    refine List.Perm.trans ?_ (List.filter_append_perm (fun item => key item == k)
      (items.filter fun item => (k :: keys).contains (key item)))
    rw [List.filter_filter, List.filter_filter]
    apply List.Perm.of_eq
    congr 1
    · apply List.filter_congr
      intro item _
      by_cases h : key item = k <;> simp [h]
    · apply List.filter_congr
      intro item _
      by_cases h : key item = k
      · simp [h, hk]
      · simp [h]

theorem mem_problemKeys {rows : Rows} {key : String} :
    key ∈ problemKeys rows ↔
      key ∈ (rows.problems.map fun problem => resolve rows.aliases problem.problemKey) ++
        rows.attempts.map fun attempt => resolve rows.aliases attempt.problemKey := by
  rw [problemKeys, List.mem_mergeSort, List.mem_dedup]

theorem problemKeys_nodup (rows : Rows) : (problemKeys rows).Nodup :=
  (List.mergeSort_perm _ _).nodup_iff.mpr (List.nodup_dedup _)

open scoped List in
/-- `fold` drops no attempt and repeats none: the attempts filed under its
problems are the attempt rows. -/
theorem fold_keeps_every_attempt (current : Current) (rows : Rows) :
    ((fold current rows).problems.flatMap fun problem => problem.attempts.map (·.attemptKey)).Perm
      (rows.attempts.map (·.attemptKey)) := by
  simp only [fold, foldProblem, List.flatMap_map, List.map_map]
  have hkey : ∀ attempt, (foldAttempt current rows.outcomes attempt).attemptKey =
      attempt.attemptKey := fun _ => rfl
  have hall : ∀ attempt ∈ rows.attempts,
      (problemKeys rows).contains (resolve rows.aliases attempt.problemKey) = true := by
    intro attempt hattempt
    rw [List.contains_iff_mem, mem_problemKeys]
    exact List.mem_append_right _ (List.mem_map.mpr ⟨attempt, hattempt, rfl⟩)
  calc ((problemKeys rows).flatMap fun k => ((rows.attempts.filter fun attempt =>
          resolve rows.aliases attempt.problemKey == k).mergeSort attemptBefore).map
            ((·.attemptKey) ∘ foldAttempt current rows.outcomes))
      = (problemKeys rows).flatMap fun k => ((rows.attempts.filter fun attempt =>
          resolve rows.aliases attempt.problemKey == k).mergeSort attemptBefore).map
            (·.attemptKey) := by
        congr 1
    _ ~ (problemKeys rows).flatMap fun k => (rows.attempts.filter fun attempt =>
          resolve rows.aliases attempt.problemKey == k).map (·.attemptKey) :=
        List.Perm.flatMap_left _ fun _ _ => (List.mergeSort_perm _ _).map _
    _ = ((problemKeys rows).flatMap fun k => rows.attempts.filter fun attempt =>
          resolve rows.aliases attempt.problemKey == k).map (·.attemptKey) := by
        rw [List.map_flatMap]
    _ ~ (rows.attempts.filter fun attempt =>
          (problemKeys rows).contains (resolve rows.aliases attempt.problemKey)).map
            (·.attemptKey) :=
        (flatMap_filter_perm (fun attempt => resolve rows.aliases attempt.problemKey)
          rows.attempts _ (problemKeys_nodup rows)).map _
    _ = rows.attempts.map (·.attemptKey) := by
        rw [List.filter_eq_self.mpr hall]

/-- Every attempt row is filed under the problem its cited key resolves to. -/
theorem fold_files_every_attempt (current : Current) (rows : Rows) (attempt : AttemptRow)
    (h : attempt ∈ rows.attempts) :
    ∃ problem ∈ (fold current rows).problems,
      problem.problemKey = resolve rows.aliases attempt.problemKey ∧
        foldAttempt current rows.outcomes attempt ∈ problem.attempts := by
  refine ⟨foldProblem current rows (resolve rows.aliases attempt.problemKey),
    List.mem_map.mpr ⟨_, ?_, rfl⟩, rfl, ?_⟩
  · rw [mem_problemKeys]
    exact List.mem_append_right _ (List.mem_map.mpr ⟨attempt, h, rfl⟩)
  · simp only [foldProblem]
    exact List.mem_map.mpr ⟨attempt,
      List.mem_mergeSort.mpr (List.mem_filter.mpr ⟨h, beq_self_eq_true _⟩), rfl⟩

/-- A filed attempt is an attempt row whose cited key resolves to its
problem. -/
theorem fold_attempt_is_filed_row (current : Current) (rows : Rows) {problem : FoldedProblem}
    (hproblem : problem ∈ (fold current rows).problems) {folded : FoldedAttempt}
    (hfolded : folded ∈ problem.attempts) :
    ∃ attempt ∈ rows.attempts, folded = foldAttempt current rows.outcomes attempt ∧
      resolve rows.aliases attempt.problemKey = problem.problemKey := by
  obtain ⟨key, -, rfl⟩ := List.mem_map.mp hproblem
  simp only [foldProblem] at hfolded ⊢
  obtain ⟨attempt, hattempt, rfl⟩ := List.mem_map.mp hfolded
  obtain ⟨hrow, hresolve⟩ := List.mem_filter.mp (List.mem_mergeSort.mp hattempt)
  exact ⟨attempt, hrow, rfl, by simpa using hresolve⟩

/-- With distinct attempt keys, as the store keeps them, each attempt is filed
under exactly one problem. -/
theorem fold_files_each_attempt_once (current : Current) (rows : Rows)
    (hunique : (rows.attempts.map (·.attemptKey)).Nodup) (attempt : AttemptRow)
    (h : attempt ∈ rows.attempts) :
    ∃! problem, problem ∈ (fold current rows).problems ∧
      attempt.attemptKey ∈ problem.attempts.map (·.attemptKey) := by
  obtain ⟨problem, hproblem, hkey, hfiled⟩ := fold_files_every_attempt current rows attempt h
  refine ⟨problem, ⟨hproblem, List.mem_map.mpr ⟨_, hfiled, rfl⟩⟩, ?_⟩
  rintro other ⟨hother, hmem⟩
  obtain ⟨folded, hfolded, hfoldedKey⟩ := List.mem_map.mp hmem
  obtain ⟨row, hrow, rfl, hresolve⟩ := fold_attempt_is_filed_row current rows hother hfolded
  have hsame : row = attempt := List.inj_on_of_nodup_map hunique hrow h hfoldedKey
  subst hsame
  obtain ⟨key, -, rfl⟩ := List.mem_map.mp hother
  obtain ⟨key', -, rfl⟩ := List.mem_map.mp hproblem
  simp only [foldProblem] at hresolve hkey ⊢
  rw [← hresolve, ← hkey]

/-- `fold` drops no outcome: each one is listed under its attempt, or as
unattached when its attempt is not among the rows. -/
theorem fold_keeps_every_outcome (current : Current) (rows : Rows) (outcome : OutcomeRow)
    (h : outcome ∈ rows.outcomes) :
    outcome.outcomeKey ∈ (fold current rows).unattached ∨
      ∃ problem ∈ (fold current rows).problems, ∃ folded ∈ problem.attempts,
        observe current outcome ∈ folded.outcomes := by
  by_cases hattempt : rows.attempts.any (·.attemptKey == outcome.attemptKey)
  · right
    obtain ⟨attempt, hrow, hkey⟩ := List.any_eq_true.mp hattempt
    obtain ⟨problem, hproblem, -, hfiled⟩ := fold_files_every_attempt current rows attempt hrow
    refine ⟨problem, hproblem, _, hfiled, ?_⟩
    simp only [foldAttempt, observations]
    have hmatch : (outcome.attemptKey == attempt.attemptKey) = true := by
      rw [beq_iff_eq] at hkey ⊢
      exact hkey.symm
    exact List.mem_map.mpr ⟨outcome,
      List.mem_mergeSort.mpr (List.mem_filter.mpr ⟨h, hmatch⟩), rfl⟩
  · left
    simp only [fold]
    exact List.mem_map.mpr ⟨outcome,
      List.mem_mergeSort.mpr (List.mem_filter.mpr ⟨h, by simpa using hattempt⟩), rfl⟩

/-- An admitted merge preserves every attempt of both problems: after its alias
row, the attempts of `fromKey`'s problem and of `toKey`'s are filed together
under the problem `toKey` resolved to. -/
theorem admitted_merge_files_both_problems_together (current : Current) (rows : Rows)
    (fromKey toKey : String)
    (h : mergeAdmitted (rows.problems.map (·.problemKey)) rows.aliases fromKey toKey = .admitted)
    (attempt : AttemptRow) (hattempt : attempt ∈ rows.attempts)
    (hboth : resolve rows.aliases attempt.problemKey = fromKey ∨
      resolve rows.aliases attempt.problemKey = resolve rows.aliases toKey) :
    ∃ problem ∈ (fold current { rows with aliases := rows.aliases ++ [(fromKey, toKey)] }).problems,
      problem.problemKey = resolve rows.aliases toKey ∧
        foldAttempt current rows.outcomes attempt ∈ problem.attempts := by
  obtain ⟨problem, hproblem, hkey, hfiled⟩ := fold_files_every_attempt current
    { rows with aliases := rows.aliases ++ [(fromKey, toKey)] } attempt hattempt
  exact ⟨problem, hproblem,
    hkey.trans (admitted_merge_joins_both_problems _ _ _ _ _ h hboth), hfiled⟩

/-- The index a turn shows. -/
structure Index where
  lines : List String
  omitted : Nat
  deriving DecidableEq, Repr

/-- The longest root-cause excerpt a line shows; the whole text and the attempt
history are one `memory problem` call away. -/
def nameLimit : Nat := 120

def excerpt (text : String) : String :=
  if text.length ≤ nameLimit then text else text.take nameLimit ++ "..."

def unresolvedCount (problem : FoldedProblem) : Nat :=
  (problem.attempts.map (·.unresolved.length)).sum

def Provenance.text : Provenance → String
  | .same => "same setting"
  | .otherModel => "other model"
  | .unknownModel => "unknown model"
  | .otherAgent => "other agent"
  | .otherModelAndAgent => "other model and agent"
  | .unknownModelAndOtherAgent => "unknown model and other agent"

/-- The relations a problem's attempts hold, in declaration order. -/
def flags (problem : FoldedProblem) : List Provenance :=
  [Provenance.same, .otherModel, .unknownModel, .otherAgent, .otherModelAndAgent,
    .unknownModelAndOtherAgent].filter fun relation =>
    problem.attempts.any (·.provenance == relation)

/-- An observation's score, `unscored` without one, and where it was recorded
unless that is the reader's own setting. -/
def Observation.text (observation : Observation) : String :=
  let score := match observation.score with
    | some score => toString score.passed ++ "/" ++ toString score.total
    | none => "unscored"
  if observation.provenance == .same then score
  else score ++ " (" ++ observation.provenance.text ++ ")"

/-- One index line: key, root-cause excerpt, attempt count, the latest
observation (`Observation.text`, `none` before any), unresolved predictions
and, for a problem with attempts, where they were recorded. -/
def line (problem : FoldedProblem) : String :=
  let latest := (problem.latest.map Observation.text).getD "none"
  let origin := match flags problem with
    | [] => ""
    | relations => "; from " ++ ", ".intercalate (relations.map Provenance.text)
  problem.problemKey ++ " " ++ (problem.name.map excerpt).getD "(no problem row)" ++
    " [attempts " ++ toString problem.attempts.length ++ "; latest " ++ latest ++
    "; unresolved " ++ toString (unresolvedCount problem) ++ origin ++ "]"

/-- A problem's retrieval rank: the first rank naming its key or a key merged
into it; an unranked problem ranks after every ranked one. -/
def rankOf (ranks : List String) (problem : FoldedProblem) : Nat :=
  (ranks.takeWhile fun key => !(key == problem.problemKey || problem.merged.contains key)).length

def shownBefore (ranks : List String) (first second : FoldedProblem) : Bool :=
  let firstRank := rankOf ranks first
  let secondRank := rankOf ranks second
  let firstOpen := decide (0 < unresolvedCount first)
  let secondOpen := decide (0 < unresolvedCount second)
  if firstRank != secondRank then firstRank < secondRank
  else if firstOpen != secondOpen then firstOpen
  else if first.lastActive != second.lastActive then second.lastActive < first.lastActive
  else first.problemKey ≤ second.problemKey

/-- The lines, in order, while each costs its characters and a newline within
what is left of `budget`. -/
def fit : Nat → List String → List String
  | _, [] => []
  | budget, line :: rest =>
    if line.length + 1 ≤ budget then line :: fit (budget - (line.length + 1)) rest else []

/-- The experience index a turn shows. Decision (arXiv 2611.18856, D.2):
problems the request retrieved come first in retrieval order, then problems with
unresolved predictions, then the most recently active. Lines are taken in that
order while they fit `budget`, each costing its characters (Unicode scalar
values, Rust `chars().count()`) and a newline; every problem not shown is
counted in `omitted`, which the reader states. The provenance of each line is
explicit text, never a hidden reweighting. -/
def recall (experience : Experience) (ranks : List String) (budget : Nat) : Index :=
  let shown := fit budget ((experience.problems.mergeSort (shownBefore ranks)).map line)
  { lines := shown, omitted := experience.problems.length - shown.length }

theorem fit_cost : ∀ (budget : Nat) (lines : List String),
    ((fit budget lines).map (·.length + 1)).sum ≤ budget
  | _, [] => by simp [fit]
  | budget, line :: rest => by
    unfold fit
    split_ifs with h
    · have := fit_cost (budget - (line.length + 1)) rest
      simp only [List.map_cons, List.sum_cons]
      omega
    · simp

theorem fit_prefix : ∀ (budget : Nat) (lines : List String), fit budget lines <+: lines
  | _, [] => by simp [fit]
  | budget, line :: rest => by
    unfold fit
    split_ifs
    · exact (List.prefix_cons_inj _).mpr (fit_prefix _ rest)
    · exact List.nil_prefix

/-- The lines `recall` shows fit its budget. -/
theorem recall_within_budget (experience : Experience) (ranks : List String) (budget : Nat) :
    ((recall experience ranks budget).lines.map (·.length + 1)).sum ≤ budget :=
  fit_cost _ _

/-- The lines `recall` shows are the first of the ordered index. -/
theorem recall_shows_a_prefix (experience : Experience) (ranks : List String) (budget : Nat) :
    (recall experience ranks budget).lines <+:
      (experience.problems.mergeSort (shownBefore ranks)).map line :=
  fit_prefix _ _

/-- Every problem is shown or counted as omitted. -/
theorem recall_counts_every_problem (experience : Experience) (ranks : List String)
    (budget : Nat) :
    (recall experience ranks budget).lines.length + (recall experience ranks budget).omitted =
      experience.problems.length := by
  have hle := (recall_shows_a_prefix experience ranks budget).length_le
  rw [List.length_map, (List.mergeSort_perm _ _).length_eq] at hle
  simp only [recall] at hle ⊢
  omega

end Tools.Memory.Experience
