import Proofs.Hooks.Effect

namespace Hooks

/-- What a hook returns: a refusal, or a value for core and the effects it
wants. There is no third case and no handle: a plugin cannot commit, read
again, or observe whether its effects were applied. -/
inductive Outcome (V Doc Val Op : Type) where
  | refuse (reason : String)
  | proceed (value : V) (effects : List (Effect Doc Val Op))

variable {Core Q Ev V Doc Val Op : Type}

def Outcome.effects : Outcome V Doc Val Op → List (Effect Doc Val Op)
  | .refuse _ => []
  | .proceed _ effects => effects

def Outcome.value? : Outcome V Doc Val Op → Option V
  | .refuse _ => none
  | .proceed value _ => some value

/-- A data hook. `reads` is the plugin's declaration, from the event alone, of
the rows it needs; core answers each query inside the lifecycle transaction.
`decide` is a function of the event and those rows and nothing else, which is
what lets the same contract be served by a WASM guest (gents-ai/gents#3656).

Design choice: one round of reads. A plugin cannot issue a second query that
depends on a row it was shown. The Goal claim fence needs the request its Goal
row names as assignment root; it declares the session's requests instead of
following the reference. The alternative, a read-decide loop, would make the
guest a participant in the transaction and reintroduce the handle. -/
structure Hook (Q Ev V Doc Val Op : Type) where
  reads : Ev → List Q
  decide : Ev → List (List (Row Doc Val)) → Outcome V Doc Val Op

/-- A bundled plugin at one hook point. The list of plugins at a point is
fixed at build time in core's composition root; a missing listener is a build
error, not a runtime state (gents-ai/gents#3656). -/
structure Plugin (Q Ev V Doc Val Op : Type) where
  manifest : Manifest
  hook : Hook Q Ev V Doc Val Op

/-- What core supplies. `select` answers a declared query from the
transaction: rows of the plugin's collections, and core rows or observations
(a verified signature, a document ACP decision for a named subject) rendered
as read-only rows. `perform` is the named core operations. -/
structure Host (Core Q Doc Val Op : Type) where
  select : Core → Store Doc Val → Q → List (Row Doc Val)
  perform : Actor → Op → Core → Option Core
  allowed : Acp Doc

/-- Why a lifecycle transaction did not commit. A plugin's refusal reaches the
caller as the plugin's name and reason, as data. The request owner's error
enum names a feature today (`ClaimAdmissionError::MailboxReplyRejected`,
`session/mailbox/reply.rs`); for this contract to suffice core must carry
`refused` generically and drop that variant. -/
inductive Refusal where
  | core
  | refused (plugin reason : String)
  | invalid (plugin : String)
  | conflict (plugin : String)
  deriving DecidableEq, Repr

def Hook.run (host : Host Core Q Doc Val Op) (hook : Hook Q Ev V Doc Val Op) (event : Ev)
    (snapshot : State Core Doc Val) : Outcome V Doc Val Op :=
  hook.decide event ((hook.reads event).map (host.select snapshot.core snapshot.docs))

variable [DecidableEq Doc]

/-- Staging at a point where a plugin may refuse: work is about to start or
continue (request creation, claim, resume, fork). Any refusal, invalid result
or miss aborts the transaction. Interrupt, stop and terminal transitions use
`Hooks.stageDropping` instead. Every plugin at a point decides against the
same `snapshot`; results are validated and applied in list order onto
`staged`.

Design choice: a later plugin sees none of an earlier plugin's staged effects
at the same point. The alternative is to show it all of them. None was chosen
because decisions then do not depend on plugin order, a guest can be evaluated
without replaying other plugins' writes, and one plugin cannot steer another
by staging rows the other reads. Plugins own disjoint collections, so the
only order-sensitive part left is the order of requested core operations,
which is the fixed list order. A later hook point in the same transaction
does see everything staged by earlier points. -/
def stageFrom (host : Host Core Q Doc Val Op) (actor : Actor) (event : Ev)
    (snapshot : State Core Doc Val) :
    List (Plugin Q Ev V Doc Val Op) → State Core Doc Val →
      Except Refusal (State Core Doc Val × List V)
  | [], staged => .ok (staged, [])
  | plugin :: rest, staged =>
      match plugin.hook.run host event snapshot with
      | .refuse reason => .error (.refused plugin.manifest.name reason)
      | .proceed value effects =>
          if valid plugin.manifest host.allowed actor effects then
            match applyEffects host.perform actor effects staged with
            | none => .error (.conflict plugin.manifest.name)
            | some next =>
                match stageFrom host actor event snapshot rest next with
                | .error refusal => .error refusal
                | .ok (final, values) => .ok (final, value :: values)
          else .error (.invalid plugin.manifest.name)

def stage (host : Host Core Q Doc Val Op) (plugins : List (Plugin Q Ev V Doc Val Op))
    (actor : Actor) (event : Ev) (snapshot : State Core Doc Val) :
    Except Refusal (State Core Doc Val × List V) :=
  stageFrom host actor event snapshot plugins snapshot

/-- A core transition with hooks on both sides, in one transaction. Hooks
before the transition return values core consumes (an amended request input, a
claim verdict); hooks after it see the transition's result in their event.
The core transition is a parameter the plugins have no access to. -/
def around {EvB VB EvA VA : Type} (host : Host Core Q Doc Val Op)
    (before : List (Plugin Q EvB VB Doc Val Op)) (after : List (Plugin Q EvA VA Doc Val Op))
    (actor : Actor) (beforeEvent : Core → EvB) (core : List VB → Core → Option Core)
    (afterEvent : Core → Core → EvA) (state : State Core Doc Val) :
    Except Refusal (State Core Doc Val × List VB × List VA) :=
  match stage host before actor (beforeEvent state.core) state with
  | .error refusal => .error refusal
  | .ok (prepared, returned) =>
      match core returned prepared.core with
      | none => .error .core
      | some next =>
          match stage host after actor (afterEvent prepared.core next)
              { prepared with core := next } with
          | .error refusal => .error refusal
          | .ok (final, observed) => .ok (final, returned, observed)

/-- The transaction boundary: an error commits nothing. -/
def settle {α : Type} (state : State Core Doc Val) :
    Except Refusal (State Core Doc Val × α) → State Core Doc Val
  | .ok (next, _) => next
  | .error _ => state

private theorem stageFrom_ok (host : Host Core Q Doc Val Op) (actor : Actor) (event : Ev)
    (snapshot : State Core Doc Val) (plugins : List (Plugin Q Ev V Doc Val Op))
    (staged final : State Core Doc Val) (values : List V)
    (ok : stageFrom host actor event snapshot plugins staged = .ok (final, values)) :
    applyEffects host.perform actor
        (plugins.flatMap fun plugin => (plugin.hook.run host event snapshot).effects) staged =
        some final ∧
      plugins.map (fun plugin => (plugin.hook.run host event snapshot).value?) =
        values.map some ∧
      ∀ plugin ∈ plugins, valid plugin.manifest host.allowed actor
        (plugin.hook.run host event snapshot).effects = true := by
  induction plugins generalizing staged values with
  | nil =>
    simp only [stageFrom, Except.ok.injEq, Prod.mk.injEq] at ok
    obtain ⟨rfl, rfl⟩ := ok
    simp [applyEffects]
  | cons plugin rest ih =>
    cases decision : plugin.hook.run host event snapshot with
    | refuse reason => simp [stageFrom, decision] at ok
    | proceed value effects =>
      simp only [stageFrom, decision] at ok
      by_cases accepted : valid plugin.manifest host.allowed actor effects = true
      · simp only [accepted, if_true] at ok
        cases applied : applyEffects host.perform actor effects staged with
        | none => simp [applied] at ok
        | some next =>
          simp only [applied] at ok
          cases tail : stageFrom host actor event snapshot rest next with
          | error refusal => simp [tail] at ok
          | ok result =>
            obtain ⟨last, others⟩ := result
            simp only [tail, Except.ok.injEq, Prod.mk.injEq] at ok
            obtain ⟨rfl, rfl⟩ := ok
            obtain ⟨effectsApplied, decided, allValid⟩ := ih next others tail
            refine ⟨?_, ?_, ?_⟩
            · simp only [List.flatMap_cons, decision, Outcome.effects, applyEffects_append,
                applied, Option.bind_some]
              exact effectsApplied
            · rw [List.map_cons, List.map_cons, decision, decided]
              rfl
            · intro candidate member
              rcases List.mem_cons.mp member with rfl | member
              · simpa [decision, Outcome.effects] using accepted
              · exact allValid candidate member
      · simp [accepted] at ok

/-- No partial commit, at a point where refusal is allowed: a staged point is
exactly the snapshot plus every plugin's effects, in plugin order. So a
plugin's rows and core's rows change together or not at all, and a plugin's
state cannot disagree with core's. At a point whose transition must commit
this holds only while no plugin is dropped
(`Hooks.undropped_staging_applies_every_plugin`). -/
theorem staged_state_applies_every_plugin (host : Host Core Q Doc Val Op)
    (plugins : List (Plugin Q Ev V Doc Val Op)) (actor : Actor) (event : Ev)
    (snapshot final : State Core Doc Val) (values : List V)
    (ok : stage host plugins actor event snapshot = .ok (final, values)) :
    applyEffects host.perform actor
      (plugins.flatMap fun plugin => (plugin.hook.run host event snapshot).effects) snapshot =
      some final :=
  (stageFrom_ok host actor event snapshot plugins snapshot final values ok).1

/-- The view rule: each returned value is that plugin's decision on the shared
snapshot, wherever the plugin sits in the list and whatever the plugins before
it staged. -/
theorem staged_values_are_snapshot_decisions (host : Host Core Q Doc Val Op)
    (plugins : List (Plugin Q Ev V Doc Val Op)) (actor : Actor) (event : Ev)
    (snapshot final : State Core Doc Val) (values : List V)
    (ok : stage host plugins actor event snapshot = .ok (final, values)) :
    plugins.map (fun plugin => (plugin.hook.run host event snapshot).value?) =
      values.map some :=
  (stageFrom_ok host actor event snapshot plugins snapshot final values ok).2.1

theorem staged_effects_passed_validation (host : Host Core Q Doc Val Op)
    (plugins : List (Plugin Q Ev V Doc Val Op)) (actor : Actor) (event : Ev)
    (snapshot final : State Core Doc Val) (values : List V)
    (ok : stage host plugins actor event snapshot = .ok (final, values))
    (plugin : Plugin Q Ev V Doc Val Op) (member : plugin ∈ plugins) :
    valid plugin.manifest host.allowed actor
      (plugin.hook.run host event snapshot).effects = true :=
  (stageFrom_ok host actor event snapshot plugins snapshot final values ok).2.2 plugin member

/-- Staging cannot widen authority: every write any plugin staged is in that
plugin's collections and permitted to the transaction's actor. -/
theorem staged_writes_are_owned_and_authorized (host : Host Core Q Doc Val Op)
    (plugins : List (Plugin Q Ev V Doc Val Op)) (actor : Actor) (event : Ev)
    (snapshot final : State Core Doc Val) (values : List V)
    (ok : stage host plugins actor event snapshot = .ok (final, values))
    (plugin : Plugin Q Ev V Doc Val Op) (member : plugin ∈ plugins) (w : Write Doc Val)
    (written : Effect.write w ∈ (plugin.hook.run host event snapshot).effects) :
    w.ref.collection ∈ plugin.manifest.owns ∧ host.allowed actor w.access w.ref = true := by
  have accepted := staged_effects_passed_validation host plugins actor event snapshot final
    values ok plugin member
  simp only [valid, Bool.and_eq_true, List.all_eq_true] at accepted
  simpa [Effect.permitted] using accepted.2 _ written

theorem staging_moves_core_only_through_perform (host : Host Core Q Doc Val Op)
    (plugins : List (Plugin Q Ev V Doc Val Op)) (actor : Actor) (event : Ev)
    (snapshot final : State Core Doc Val) (values : List V)
    (ok : stage host plugins actor event snapshot = .ok (final, values)) :
    Reach host.perform actor snapshot.core final.core :=
  effects_move_core_only_through_perform host.perform actor _ snapshot final
    (staged_state_applies_every_plugin host plugins actor event snapshot final values ok)

private theorem stageFrom_refusal (host : Host Core Q Doc Val Op) (actor : Actor) (event : Ev)
    (snapshot : State Core Doc Val) (plugins : List (Plugin Q Ev V Doc Val Op))
    (staged : State Core Doc Val) (plugin : Plugin Q Ev V Doc Val Op)
    (member : plugin ∈ plugins)
    (rejected : (plugin.hook.run host event snapshot).value? = none ∨
      valid plugin.manifest host.allowed actor
        (plugin.hook.run host event snapshot).effects = false) :
    ∃ refusal, stageFrom host actor event snapshot plugins staged = .error refusal := by
  cases outcome : stageFrom host actor event snapshot plugins staged with
  | error refusal => exact ⟨refusal, rfl⟩
  | ok result =>
    obtain ⟨final, values⟩ := result
    obtain ⟨_, decided, allValid⟩ :=
      stageFrom_ok host actor event snapshot plugins staged final values outcome
    rcases rejected with refused | invalid
    · have present : (plugin.hook.run host event snapshot).value? ∈
          plugins.map (fun plugin => (plugin.hook.run host event snapshot).value?) :=
        List.mem_map_of_mem _ member
      rw [decided, refused] at present
      simp at present
    · have accepted := allValid plugin member
      rw [invalid] at accepted
      cases accepted

/-- Atomicity, refusal: one plugin refusing leaves the state as it was,
including core's own transition and every other plugin's effects. -/
theorem refusal_leaves_state_unchanged (host : Host Core Q Doc Val Op)
    (plugins : List (Plugin Q Ev V Doc Val Op)) (actor : Actor) (event : Ev)
    (snapshot : State Core Doc Val) (plugin : Plugin Q Ev V Doc Val Op)
    (member : plugin ∈ plugins) (reason : String)
    (refused : plugin.hook.run host event snapshot = .refuse reason) :
    settle snapshot (stage host plugins actor event snapshot) = snapshot := by
  obtain ⟨refusal, failed⟩ := stageFrom_refusal host actor event snapshot plugins snapshot plugin
    member (Or.inl (by simp [refused, Outcome.value?]))
  simp [stage, failed, settle]

/-- Atomicity, validation: one effect outside the plugin's collections,
outside the actor's ACP, or over the bound leaves the state as it was. -/
theorem invalid_effect_leaves_state_unchanged (host : Host Core Q Doc Val Op)
    (plugins : List (Plugin Q Ev V Doc Val Op)) (actor : Actor) (event : Ev)
    (snapshot : State Core Doc Val) (plugin : Plugin Q Ev V Doc Val Op)
    (member : plugin ∈ plugins)
    (invalid : valid plugin.manifest host.allowed actor
      (plugin.hook.run host event snapshot).effects = false) :
    settle snapshot (stage host plugins actor event snapshot) = snapshot := by
  obtain ⟨refusal, failed⟩ := stageFrom_refusal host actor event snapshot plugins snapshot plugin
    member (Or.inr invalid)
  simp [stage, failed, settle]

/-- Hooks cannot replace or skip the core transition: a committed transaction
contains exactly one application of core's own transition, to a core state
reached only through core operations, and everything after it is again only
core operations. If core refuses, nothing commits. -/
theorem hooks_cannot_replace_core_transition {EvB VB EvA VA : Type}
    (host : Host Core Q Doc Val Op)
    (before : List (Plugin Q EvB VB Doc Val Op)) (after : List (Plugin Q EvA VA Doc Val Op))
    (actor : Actor) (beforeEvent : Core → EvB) (core : List VB → Core → Option Core)
    (afterEvent : Core → Core → EvA) (state final : State Core Doc Val)
    (returned : List VB) (observed : List VA)
    (ok : around host before after actor beforeEvent core afterEvent state =
      .ok (final, returned, observed)) :
    ∃ prepared next, Reach host.perform actor state.core prepared ∧
      core returned prepared = some next ∧ Reach host.perform actor next final.core := by
  unfold around at ok
  cases first : stage host before actor (beforeEvent state.core) state with
  | error refusal => simp [first] at ok
  | ok result =>
    obtain ⟨prepared, values⟩ := result
    simp only [first] at ok
    cases transition : core values prepared.core with
    | none => simp [transition] at ok
    | some next =>
      simp only [transition] at ok
      cases second : stage host after actor (afterEvent prepared.core next)
          { prepared with core := next } with
      | error refusal => simp [second] at ok
      | ok result =>
        obtain ⟨last, seen⟩ := result
        simp only [second, Except.ok.injEq, Prod.mk.injEq] at ok
        obtain ⟨rfl, rfl, rfl⟩ := ok
        exact ⟨prepared.core, next,
          staging_moves_core_only_through_perform host before actor _ state prepared values first,
          transition,
          staging_moves_core_only_through_perform host after actor _ _ last seen second⟩

theorem core_refusal_commits_nothing {EvB VB EvA VA : Type}
    (host : Host Core Q Doc Val Op)
    (before : List (Plugin Q EvB VB Doc Val Op)) (after : List (Plugin Q EvA VA Doc Val Op))
    (actor : Actor) (beforeEvent : Core → EvB) (core : List VB → Core → Option Core)
    (afterEvent : Core → Core → EvA) (state : State Core Doc Val)
    (refuses : ∀ returned prepared, core returned prepared = none) :
    settle state (around host before after actor beforeEvent core afterEvent state) = state := by
  cases outcome : around host before after actor beforeEvent core afterEvent state with
  | error refusal => rfl
  | ok result =>
    obtain ⟨final, returned, observed⟩ := result
    obtain ⟨prepared, next, _, transition, _⟩ := hooks_cannot_replace_core_transition host before
      after actor beforeEvent core afterEvent state final returned observed outcome
    rw [refuses] at transition
    cases transition

/-- A hook after the transition that refuses rolls the transition back. -/
theorem after_refusal_rolls_back_core {EvB VB EvA VA : Type}
    (host : Host Core Q Doc Val Op)
    (before : List (Plugin Q EvB VB Doc Val Op)) (after : List (Plugin Q EvA VA Doc Val Op))
    (actor : Actor) (beforeEvent : Core → EvB) (core : List VB → Core → Option Core)
    (afterEvent : Core → Core → EvA) (state : State Core Doc Val)
    (refuses : ∀ event snapshot, ∃ refusal,
      stage host after actor event snapshot = .error refusal) :
    settle state (around host before after actor beforeEvent core afterEvent state) = state := by
  unfold around
  cases stage host before actor (beforeEvent state.core) state with
  | error refusal => rfl
  | ok result =>
    obtain ⟨prepared, values⟩ := result
    simp only
    cases core values prepared.core with
    | none => rfl
    | some next =>
      obtain ⟨refusal, failed⟩ := refuses (afterEvent prepared.core next)
        { prepared with core := next }
      simp [failed, settle]

omit [DecidableEq Doc] in
/-- Retry safety, same inputs: a decision is a function of the event and the
rows core read for the plugin's declared queries. Two snapshots that agree on
those rows get the same decision, so re-running a transaction closure on
unchanged inputs cannot change what is staged. -/
theorem decision_depends_only_on_declared_rows (host : Host Core Q Doc Val Op)
    (hook : Hook Q Ev V Doc Val Op) (event : Ev) (first second : State Core Doc Val)
    (same : ∀ query ∈ hook.reads event,
      host.select first.core first.docs query = host.select second.core second.docs query) :
    hook.run host event first = hook.run host event second := by
  unfold Hook.run
  congr 1
  exact List.map_congr_left same

/-- A lifecycle transaction that lost a commit race runs again on a fresh
snapshot. -/
def retried {α : Type} (run : State Core Doc Val → α) :
    List (State Core Doc Val) → State Core Doc Val → α
  | [], last => run last
  | _ :: rest, last => retried run rest last

omit [DecidableEq Doc] in
/-- Retry safety, fresh inputs: what commits after any number of conflicted
attempts is the evaluation on the last snapshot alone. `Hook.decide` takes no
state from an earlier attempt and returns none, so the rule is carried by the
type: effects that are not database writes (a model call, a host command)
cannot be expressed here and belong to the prepared decision. -/
theorem retry_is_a_fresh_evaluation {α : Type} (run : State Core Doc Val → α)
    (conflicted : List (State Core Doc Val)) (last : State Core Doc Val) :
    retried run conflicted last = run last := by
  induction conflicted with
  | nil => rfl
  | cons _ rest ih => exact ih

/-- Composition order is deterministic and visible in the result: adding a
plugin at the front applies its effects first and returns its value first. -/
theorem stage_cons_order (host : Host Core Q Doc Val Op) (plugin : Plugin Q Ev V Doc Val Op)
    (rest : List (Plugin Q Ev V Doc Val Op)) (actor : Actor) (event : Ev)
    (snapshot final : State Core Doc Val) (values : List V)
    (ok : stage host (plugin :: rest) actor event snapshot = .ok (final, values)) :
    ∃ value others mid,
      plugin.hook.run host event snapshot = .proceed value
        (plugin.hook.run host event snapshot).effects ∧
      values = value :: others ∧
      applyEffects host.perform actor (plugin.hook.run host event snapshot).effects snapshot =
        some mid ∧
      stageFrom host actor event snapshot rest mid = .ok (final, others) := by
  unfold stage at ok
  cases decision : plugin.hook.run host event snapshot with
  | refuse reason => simp [stageFrom, decision] at ok
  | proceed value effects =>
    simp only [stageFrom, decision] at ok
    by_cases accepted : valid plugin.manifest host.allowed actor effects = true
    · simp only [accepted, if_true] at ok
      cases applied : applyEffects host.perform actor effects snapshot with
      | none => simp [applied] at ok
      | some next =>
        simp only [applied] at ok
        cases tail : stageFrom host actor event snapshot rest next with
        | error refusal => simp [tail] at ok
        | ok result =>
          obtain ⟨last, others⟩ := result
          simp only [tail, Except.ok.injEq, Prod.mk.injEq] at ok
          obtain ⟨rfl, rfl⟩ := ok
          exact ⟨value, others, next, rfl, rfl, by simpa [Outcome.effects] using applied, tail⟩
    · simp [accepted] at ok

end Hooks
