import Mathlib.Data.List.Basic

namespace Hooks

/-- The DefraDB DID a lifecycle transaction runs under. A hook never names an
actor: every effect it returns is checked and applied under the actor of the
transaction that invoked it (maintainer decision, gents-ai/gents#3656: a hook
runs under the caller's actor and can narrow authority, never widen it). -/
abbrev Actor := String

/-- Document identity. `Doc` is a parameter so a model can use a structured
key; the native adapter and a WASM pack use the DefraDB document ID. -/
structure Ref (Doc : Type) where
  collection : String
  doc : Doc
  deriving DecidableEq, Repr

/-- A row as a plugin sees it. `revision` stands for DefraDB's composite
commit of the document: it is the only compare-and-set fence a plugin holds,
since it never holds the transaction that read the row. -/
structure Row (Doc Val : Type) where
  ref : Ref Doc
  revision : Nat
  value : Val
  deriving DecidableEq, Repr

abbrev Store (Doc Val : Type) := List (Row Doc Val)

inductive Access where
  | create
  | update
  | delete
  deriving DecidableEq, Repr

/-- The writes a plugin may ask for, as data. An update or delete names the
revision the plugin was shown; a create names a document that must be absent.
There is no blind write and no upsert: a plugin that was not shown a row cannot
overwrite it. -/
inductive Write (Doc Val : Type) where
  | create (ref : Ref Doc) (value : Val)
  | update (ref : Ref Doc) (expected : Nat) (value : Val)
  | delete (ref : Ref Doc) (expected : Nat)
  deriving DecidableEq, Repr

variable {Doc Val Op Core : Type}

def Write.ref : Write Doc Val → Ref Doc
  | .create ref _ => ref
  | .update ref _ _ => ref
  | .delete ref _ => ref

def Write.access : Write Doc Val → Access
  | .create _ _ => .create
  | .update _ _ _ => .update
  | .delete _ _ => .delete

/-- The effect vocabulary. `core op` asks core to perform one of its own named
operations; it is not a write. The hook catalog (gents-ai/gents#3642) found
features performing core transitions themselves: the Goal claim fence
supersedes pending AgentRequest rows (`goal/request_head.rs`), Task Goal
application flips a TriggerFire flag and publishes FireOutcome
(`goal/task_fire.rs`), trigger supersession revokes executions. Under this
contract those become requests that core performs under its own rules and the
caller's actor, so a plugin never writes a core-owned row.

Design choice: a requested operation carries its arguments as data and core's
`perform` reads only core state. The alternative, letting core read the
plugin's collections while performing the operation, would keep core naming
the feature's schema. -/
inductive Effect (Doc Val Op : Type) where
  | write (w : Write Doc Val)
  | core (op : Op)
  deriving DecidableEq, Repr

variable [DecidableEq Doc]

def held (ref : Ref Doc) (expected : Nat) (row : Row Doc Val) : Bool :=
  row.ref == ref && row.revision == expected

/-- One write against the transaction's current rows. `none` is a compare-and-
set miss or a create of an existing document; the caller aborts the whole
lifecycle transaction, it does not skip the write. -/
def applyWrite (store : Store Doc Val) : Write Doc Val → Option (Store Doc Val)
  | .create ref value =>
      if store.any (fun row => row.ref == ref) then none
      else some (⟨ref, 0, value⟩ :: store)
  | .update ref expected value =>
      if store.any (held ref expected) then
        some (store.map fun row =>
          if held ref expected row then
            { row with revision := expected + 1, value := value }
          else row)
      else none
  | .delete ref expected =>
      if store.any (held ref expected) then
        some (store.filter fun row => !(row.ref == ref))
      else none

/-- What a bundled plugin declares at build time. `owns` is the unit of write
authority: whole collections. Core-owned collections are in no manifest, so a
core-owned field is unreachable by construction; a feature field that lives on
a core row today (TriggerFire.goal_assignment_applied, the Goal columns of
AgentRequest) must either move to a collection the plugin owns or be changed
through a requested core operation. `bound` caps one hook result. -/
structure Manifest where
  name : String
  owns : List String
  bound : Nat
  deriving DecidableEq, Repr

/-- Document ACP as core observes it for the transaction's actor. Permission
categories are groupings of these grants (gents-ai/gents#3656), so there is no
second predicate for agents. -/
abbrev Acp (Doc : Type) := Actor → Access → Ref Doc → Bool

def Effect.permitted (manifest : Manifest) (allowed : Acp Doc) (actor : Actor) :
    Effect Doc Val Op → Bool
  | .write w => manifest.owns.contains w.ref.collection && allowed actor w.access w.ref
  | .core _ => true

/-- Core's validation of a hook result, decided before anything is applied:
bounded, every write in a collection the plugin owns, every write permitted to
the caller's actor by document ACP. A requested core operation is admitted
here and authorized by core's own `perform` under the same actor. -/
def valid (manifest : Manifest) (allowed : Acp Doc) (actor : Actor)
    (effects : List (Effect Doc Val Op)) : Bool :=
  decide (effects.length ≤ manifest.bound) &&
    effects.all (Effect.permitted manifest allowed actor)

/-- The lifecycle transaction's view: core-owned state and document rows of
plugin-owned collections. -/
structure State (Core Doc Val : Type) where
  core : Core
  docs : Store Doc Val

def applyEffect (perform : Actor → Op → Core → Option Core) (actor : Actor)
    (state : State Core Doc Val) : Effect Doc Val Op → Option (State Core Doc Val)
  | .write w => (applyWrite state.docs w).map fun docs => { state with docs := docs }
  | .core op => (perform actor op state.core).map fun core => { state with core := core }

/-- Effects apply in the order returned. Any miss discards all of them. -/
def applyEffects (perform : Actor → Op → Core → Option Core) (actor : Actor) :
    List (Effect Doc Val Op) → State Core Doc Val → Option (State Core Doc Val)
  | [], state => some state
  | effect :: rest, state =>
      match applyEffect perform actor state effect with
      | none => none
      | some next => applyEffects perform actor rest next

def commit (manifest : Manifest) (allowed : Acp Doc)
    (perform : Actor → Op → Core → Option Core) (actor : Actor)
    (effects : List (Effect Doc Val Op)) (state : State Core Doc Val) :
    Option (State Core Doc Val) :=
  if valid manifest allowed actor effects then applyEffects perform actor effects state
  else none

theorem applyEffects_append (perform : Actor → Op → Core → Option Core) (actor : Actor)
    (first second : List (Effect Doc Val Op)) (state : State Core Doc Val) :
    applyEffects perform actor (first ++ second) state =
      (applyEffects perform actor first state).bind (applyEffects perform actor second) := by
  induction first generalizing state with
  | nil => rfl
  | cons effect rest ih =>
    simp only [List.cons_append, applyEffects]
    cases applyEffect perform actor state effect with
    | none => rfl
    | some next => exact ih next

theorem invalid_effects_commit_nothing (manifest : Manifest) (allowed : Acp Doc)
    (perform : Actor → Op → Core → Option Core) (actor : Actor)
    (effects : List (Effect Doc Val Op)) (state : State Core Doc Val)
    (invalid : valid manifest allowed actor effects = false) :
    commit manifest allowed perform actor effects state = none := by
  simp [commit, invalid]

theorem commit_is_bounded (manifest : Manifest) (allowed : Acp Doc)
    (perform : Actor → Op → Core → Option Core) (actor : Actor)
    (effects : List (Effect Doc Val Op)) (state next : State Core Doc Val)
    (committed : commit manifest allowed perform actor effects state = some next) :
    effects.length ≤ manifest.bound := by
  unfold commit at committed
  split at committed
  · rename_i accepted
    simp only [valid, Bool.and_eq_true, decide_eq_true_eq] at accepted
    exact accepted.1
  · cases committed

/-- Effects cannot widen authority: every committed write is one document ACP
grants the caller's actor, in a collection the plugin owns. The hook result
has no field that could name another actor. -/
theorem committed_writes_are_owned_and_authorized (manifest : Manifest) (allowed : Acp Doc)
    (perform : Actor → Op → Core → Option Core) (actor : Actor)
    (effects : List (Effect Doc Val Op)) (state next : State Core Doc Val)
    (committed : commit manifest allowed perform actor effects state = some next)
    (w : Write Doc Val) (member : Effect.write w ∈ effects) :
    w.ref.collection ∈ manifest.owns ∧ allowed actor w.access w.ref = true := by
  unfold commit at committed
  split at committed
  · rename_i accepted
    simp only [valid, Bool.and_eq_true, List.all_eq_true] at accepted
    have permitted := accepted.2 _ member
    simpa [Effect.permitted] using permitted
  · cases committed

theorem unauthorized_write_commits_nothing (manifest : Manifest) (allowed : Acp Doc)
    (perform : Actor → Op → Core → Option Core) (actor : Actor)
    (effects : List (Effect Doc Val Op)) (state : State Core Doc Val)
    (w : Write Doc Val) (member : Effect.write w ∈ effects)
    (denied : allowed actor w.access w.ref = false) :
    commit manifest allowed perform actor effects state = none := by
  cases result : commit manifest allowed perform actor effects state with
  | none => rfl
  | some next =>
    have authorized :=
      (committed_writes_are_owned_and_authorized manifest allowed perform actor effects
        state next result w member).2
    simp [denied] at authorized

theorem foreign_write_commits_nothing (manifest : Manifest) (allowed : Acp Doc)
    (perform : Actor → Op → Core → Option Core) (actor : Actor)
    (effects : List (Effect Doc Val Op)) (state : State Core Doc Val)
    (w : Write Doc Val) (member : Effect.write w ∈ effects)
    (foreign : w.ref.collection ∉ manifest.owns) :
    commit manifest allowed perform actor effects state = none := by
  cases result : commit manifest allowed perform actor effects state with
  | none => rfl
  | some next =>
    exact (foreign (committed_writes_are_owned_and_authorized manifest allowed perform actor
      effects state next result w member).1).elim

def inCollection (collection : String) (row : Row Doc Val) : Bool :=
  row.ref.collection == collection

omit [DecidableEq Doc] in
private theorem filter_map_unaffected (store : Store Doc Val) (keep : Row Doc Val → Bool)
    (change : Row Doc Val → Row Doc Val)
    (outside : ∀ row, change row = row ∨ (keep (change row) = false ∧ keep row = false)) :
    (store.map change).filter keep = store.filter keep := by
  induction store with
  | nil => rfl
  | cons row rest ih =>
    rcases outside row with same | ⟨after, before⟩
    · simp [List.filter_cons, same, ih]
    · simp [List.filter_cons, after, before, ih]

omit [DecidableEq Doc] in
private theorem filter_filter_unaffected (store : Store Doc Val)
    (keep drop : Row Doc Val → Bool)
    (outside : ∀ row, drop row = false → keep row = false) :
    (store.filter drop).filter keep = store.filter keep := by
  induction store with
  | nil => rfl
  | cons row rest ih =>
    cases dropped : drop row with
    | true => simp [List.filter_cons, dropped, ih]
    | false => simp [List.filter_cons, dropped, outside row dropped, ih]

theorem write_leaves_other_collections (store next : Store Doc Val) (w : Write Doc Val)
    (collection : String) (other : w.ref.collection ≠ collection)
    (applied : applyWrite store w = some next) :
    next.filter (inCollection collection) = store.filter (inCollection collection) := by
  cases w with
  | create ref value =>
    simp only [applyWrite] at applied
    split at applied
    · cases applied
    · cases applied
      have : inCollection collection (⟨ref, 0, value⟩ : Row Doc Val) = false := by
        simpa [inCollection, Write.ref] using other
      simp [List.filter_cons, this]
  | update ref expected value =>
    simp only [applyWrite] at applied
    split at applied
    · cases applied
      apply filter_map_unaffected
      intro row
      by_cases matching : held ref expected row = true
      · right
        have same : row.ref = ref := by
          simp only [held, Bool.and_eq_true, beq_iff_eq] at matching
          exact matching.1
        have differs : ref.collection ≠ collection := by simpa [Write.ref] using other
        simp [matching, inCollection, same, differs]
      · left
        simp [matching]
    · cases applied
  | delete ref expected =>
    simp only [applyWrite] at applied
    split at applied
    · cases applied
      apply filter_filter_unaffected
      intro row dropped
      have same : row.ref = ref := by simpa using dropped
      have differs : ref.collection ≠ collection := by simpa [Write.ref] using other
      simp [inCollection, same, differs]
    · cases applied

/-- Rows outside the plugin's collections are byte-for-byte what they were:
another plugin's state and any projection core keeps as documents cannot be
changed by a hook result that passed validation. -/
theorem commit_leaves_unowned_collections (manifest : Manifest) (allowed : Acp Doc)
    (perform : Actor → Op → Core → Option Core) (actor : Actor)
    (effects : List (Effect Doc Val Op)) (state next : State Core Doc Val)
    (committed : commit manifest allowed perform actor effects state = some next)
    (collection : String) (unowned : collection ∉ manifest.owns) :
    next.docs.filter (inCollection collection) = state.docs.filter (inCollection collection) := by
  have owned : ∀ w, Effect.write w ∈ effects → w.ref.collection ≠ collection := by
    intro w member same
    exact unowned (same ▸ (committed_writes_are_owned_and_authorized manifest allowed perform
      actor effects state next committed w member).1)
  have applied : applyEffects perform actor effects state = some next := by
    unfold commit at committed
    split at committed
    · exact committed
    · cases committed
  clear committed
  induction effects generalizing state with
  | nil =>
    simp only [applyEffects, Option.some.injEq] at applied
    rw [applied]
  | cons effect rest ih =>
    simp only [applyEffects] at applied
    cases step : applyEffect perform actor state effect with
    | none => simp [step] at applied
    | some mid =>
      simp only [step] at applied
      have tail := ih mid (fun w member => owned w (List.mem_cons_of_mem _ member)) applied
      rw [tail]
      cases effect with
      | write w =>
        simp only [applyEffect, Option.map_eq_some'] at step
        obtain ⟨docs, written, rfl⟩ := step
        exact write_leaves_other_collections state.docs docs w collection
          (owned w (List.mem_cons_self _ _)) written
      | core op =>
        simp only [applyEffect, Option.map_eq_some'] at step
        obtain ⟨core, _, rfl⟩ := step
        rfl

/-- Core state moves only by core's own operations, performed for the caller's
actor. -/
inductive Reach (perform : Actor → Op → Core → Option Core) (actor : Actor) :
    Core → Core → Prop where
  | refl (core : Core) : Reach perform actor core core
  | step {first mid last : Core} (op : Op) (performed : perform actor op first = some mid)
      (rest : Reach perform actor mid last) : Reach perform actor first last

theorem Reach.trans {perform : Actor → Op → Core → Option Core} {actor : Actor}
    {first mid last : Core} (left : Reach perform actor first mid)
    (right : Reach perform actor mid last) : Reach perform actor first last := by
  induction left with
  | refl => exact right
  | step op performed _ ih => exact .step op performed (ih right)

theorem effects_move_core_only_through_perform (perform : Actor → Op → Core → Option Core)
    (actor : Actor) (effects : List (Effect Doc Val Op)) (state next : State Core Doc Val)
    (applied : applyEffects perform actor effects state = some next) :
    Reach perform actor state.core next.core := by
  induction effects generalizing state with
  | nil =>
    simp only [applyEffects, Option.some.injEq] at applied
    rw [applied]
    exact .refl _
  | cons effect rest ih =>
    simp only [applyEffects] at applied
    cases step : applyEffect perform actor state effect with
    | none => simp [step] at applied
    | some mid =>
      simp only [step] at applied
      have tail := ih mid applied
      cases effect with
      | write w =>
        simp only [applyEffect, Option.map_eq_some'] at step
        obtain ⟨docs, _, rfl⟩ := step
        exact tail
      | core op =>
        simp only [applyEffect, Option.map_eq_some'] at step
        obtain ⟨core, performed, rfl⟩ := step
        exact .step op performed tail

def Effect.isWrite : Effect Doc Val Op → Bool
  | .write _ => true
  | .core _ => false

theorem writes_leave_core (perform : Actor → Op → Core → Option Core)
    (actor : Actor) (effects : List (Effect Doc Val Op)) (state next : State Core Doc Val)
    (onlyWrites : effects.all Effect.isWrite = true)
    (applied : applyEffects perform actor effects state = some next) :
    next.core = state.core := by
  induction effects generalizing state with
  | nil =>
    simp only [applyEffects, Option.some.injEq] at applied
    rw [applied]
  | cons effect rest ih =>
    simp only [List.all_cons, Bool.and_eq_true] at onlyWrites
    simp only [applyEffects] at applied
    cases step : applyEffect perform actor state effect with
    | none => simp [step] at applied
    | some mid =>
      simp only [step] at applied
      rw [ih mid onlyWrites.2 applied]
      cases effect with
      | write w =>
        simp only [applyEffect, Option.map_eq_some'] at step
        obtain ⟨docs, _, rfl⟩ := step
        rfl
      | core op => simp [Effect.isWrite] at onlyWrites

/-- Deleting the rows a plugin was shown removes exactly those documents. -/
theorem deletes_remove_shown_rows (perform : Actor → Op → Core → Option Core)
    (actor : Actor) (rows : List (Row Doc Val)) (state : State Core Doc Val)
    (distinct : (rows.map (·.ref)).Nodup) (shown : ∀ row ∈ rows, row ∈ state.docs) :
    applyEffects perform actor
        (rows.map fun row => (Effect.write (.delete row.ref row.revision) : Effect Doc Val Op))
        state =
      some { state with
        docs := state.docs.filter fun row => !(rows.map (·.ref)).contains row.ref } := by
  induction rows generalizing state with
  | nil => simp [applyEffects]
  | cons row rest ih =>
    simp only [List.map_cons, List.nodup_cons] at distinct
    have present : state.docs.any (held row.ref row.revision) = true := by
      simp only [List.any_eq_true]
      exact ⟨row, shown row (List.mem_cons_self _ _), by simp [held]⟩
    simp only [List.map_cons, applyEffects, applyEffect, applyWrite, present, if_true,
      Option.map_some']
    have remaining : ∀ other ∈ rest,
        other ∈ (state.docs.filter fun candidate => !(candidate.ref == row.ref)) := by
      intro other member
      simp only [List.mem_filter, Bool.not_eq_true', beq_eq_false_iff_ne, ne_eq]
      refine ⟨shown other (List.mem_cons_of_mem _ member), ?_⟩
      intro same
      exact distinct.1 (same ▸ List.mem_map_of_mem (·.ref) member)
    rw [ih { state with docs := state.docs.filter fun candidate => !(candidate.ref == row.ref) }
      distinct.2 remaining]
    simp only [List.filter_filter, Option.some.injEq]
    congr 1
    apply List.filter_congr
    intro candidate _
    by_cases same : candidate.ref = row.ref
    · simp [same]
    · have differs : (candidate.ref == row.ref) = false := by simpa using same
      simp [List.contains_cons, differs, same]

end Hooks
