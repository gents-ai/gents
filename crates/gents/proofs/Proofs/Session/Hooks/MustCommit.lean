import Proofs.Session.Hooks.Staging
import Proofs.Session.Hooks.Delivery

namespace Hooks

variable {Core Q Ev V Doc Val Op : Type} [DecidableEq Doc]

/-- One plugin's staging at a point where core's transition must commit.
`none` is every way the staging can fail: core could not read the plugin's
declared rows, the plugin refused, its result failed validation, or an effect
missed. None of these reaches the caller. -/
def attempt (host : Host Core Q Doc Val Op) (actor : Actor) (event : Ev)
    (snapshot : State Core Doc Val) (readFailed : String → Bool)
    (plugin : Plugin Q Ev V Doc Val Op) (staged : State Core Doc Val) :
    Option (State Core Doc Val) :=
  if readFailed plugin.manifest.name then none
  else
    match plugin.hook.run host event snapshot with
    | .refuse _ => none
    | .proceed _ effects =>
        if valid plugin.manifest host.allowed actor effects then
          applyEffects host.perform actor effects staged
        else none

/-- Staging where a plugin cannot stop the transition (maintainer decision,
gents-ai/gents#3656: a plugin can refuse work from starting or continuing and
can ask for work to stop, but can never prevent work from stopping). A plugin
whose staging fails is skipped, whole, and flagged; the others still stage.
The flags align with the plugin list.

Premise of the native realisation (decision 2026-11-11, maintainer-approved):
"skipped whole" leaves the staged state exactly as `applyEffects` does on a
miss. The native host reads, checks every revision and validates before its
first write, so most failures skip a plugin with nothing applied. A failure
after part of a plugin's effects were written (a core operation refusing
midway) cannot be undone inside the transaction, since DefraDB has no
savepoint. The transition is then aborted and run again once, from a fresh
snapshot, with that plugin dropped and owed: every lifecycle transition runs
in a transaction closure the store already re-runs on a conflict, so it is
re-runnable, and by `Hooks.retry_is_a_fresh_evaluation` what commits is this
definition evaluated on the snapshot of the final attempt. -/
def stageDropping (host : Host Core Q Doc Val Op) (actor : Actor) (event : Ev)
    (snapshot : State Core Doc Val) (readFailed : String → Bool) :
    List (Plugin Q Ev V Doc Val Op) → State Core Doc Val → State Core Doc Val × List Bool
  | [], staged => (staged, [])
  | plugin :: rest, staged =>
      match attempt host actor event snapshot readFailed plugin staged with
      | none =>
          ((stageDropping host actor event snapshot readFailed rest staged).1,
            true :: (stageDropping host actor event snapshot readFailed rest staged).2)
      | some next =>
          ((stageDropping host actor event snapshot readFailed rest next).1,
            false :: (stageDropping host actor event snapshot readFailed rest next).2)

/-- A transition that must commit: interrupt, stop, and a request or tool call
becoming terminal. Core's transition decides alone whether the transaction
commits; plugins are told and stage what they can.

This changes behaviour the hook catalog found (gents-ai/gents#3642): a failed
read of Goal state blocks an interrupt (`request/interrupt.rs`) and a failed
TriggerFire read rolls back every request terminal
(`request/lifecycle/execution_lease.rs`). -/
def mustCommit (host : Host Core Q Doc Val Op) (plugins : List (Plugin Q Ev V Doc Val Op))
    (actor : Actor) (core : Core → Option Core) (event : Core → Core → Ev)
    (readFailed : String → Bool) (state : State Core Doc Val) :
    Option (State Core Doc Val × List Bool) :=
  (core state.core).map fun next =>
    stageDropping host actor (event state.core next) { state with core := next } readFailed
      plugins { state with core := next }

/-- The deliveries a must-commit transition owes: one per dropped plugin,
published with the transition in `Hooks.Delivery.State.owed` and retried after
commit. Until that delivery commits the plugin's rows lag core's. -/
def owed (plugins : List (Plugin Q Ev V Doc Val Op)) (dropped : List Bool) : List String :=
  ((plugins.zip dropped).filter fun entry => entry.2).map fun entry => entry.1.manifest.name

/-- Core's transition commits whatever the plugins answer: whether the
transaction commits is a function of core's transition alone. -/
theorem must_commit_is_decided_by_core (host : Host Core Q Doc Val Op)
    (plugins : List (Plugin Q Ev V Doc Val Op)) (actor : Actor) (core : Core → Option Core)
    (event : Core → Core → Ev) (readFailed : String → Bool) (state : State Core Doc Val) :
    (mustCommit host plugins actor core event readFailed state).isSome =
      (core state.core).isSome := by
  simp [mustCommit]

theorem plugins_cannot_block_a_must_commit_transition (host : Host Core Q Doc Val Op)
    (plugins : List (Plugin Q Ev V Doc Val Op)) (actor : Actor) (core : Core → Option Core)
    (event : Core → Core → Ev) (readFailed : String → Bool) (state : State Core Doc Val)
    (next : Core) (transition : core state.core = some next) :
    ∃ final dropped,
      mustCommit host plugins actor core event readFailed state = some (final, dropped) := by
  simp only [mustCommit, transition, Option.map_some', Option.some.injEq]
  exact ⟨_, _, rfl⟩

private theorem attempt_reach (host : Host Core Q Doc Val Op) (actor : Actor) (event : Ev)
    (snapshot : State Core Doc Val) (readFailed : String → Bool)
    (plugin : Plugin Q Ev V Doc Val Op) (staged next : State Core Doc Val)
    (kept : attempt host actor event snapshot readFailed plugin staged = some next) :
    Reach host.perform actor staged.core next.core := by
  unfold attempt at kept
  split at kept
  · cases kept
  · split at kept
    · cases kept
    · split at kept
      · exact effects_move_core_only_through_perform host.perform actor _ staged next kept
      · cases kept

private theorem stageDropping_reach (host : Host Core Q Doc Val Op) (actor : Actor) (event : Ev)
    (snapshot : State Core Doc Val) (readFailed : String → Bool)
    (plugins : List (Plugin Q Ev V Doc Val Op)) (staged : State Core Doc Val) :
    Reach host.perform actor staged.core
      (stageDropping host actor event snapshot readFailed plugins staged).1.core := by
  induction plugins generalizing staged with
  | nil => exact .refl _
  | cons plugin rest ih =>
    simp only [stageDropping]
    cases kept : attempt host actor event snapshot readFailed plugin staged with
    | none => exact ih staged
    | some next =>
      exact (attempt_reach host actor event snapshot readFailed plugin staged next kept).trans
        (ih next)

/-- At a must-commit point too, core state is core's transition followed only
by core operations. -/
theorem must_commit_keeps_core_transition (host : Host Core Q Doc Val Op)
    (plugins : List (Plugin Q Ev V Doc Val Op)) (actor : Actor) (core : Core → Option Core)
    (event : Core → Core → Ev) (readFailed : String → Bool)
    (state final : State Core Doc Val) (dropped : List Bool)
    (committed : mustCommit host plugins actor core event readFailed state =
      some (final, dropped)) :
    ∃ next, core state.core = some next ∧ Reach host.perform actor next final.core := by
  unfold mustCommit at committed
  cases transition : core state.core with
  | none => simp [transition] at committed
  | some next =>
    simp only [transition, Option.map_some', Option.some.injEq] at committed
    refine ⟨next, rfl, ?_⟩
    have reached := stageDropping_reach host actor (event state.core next)
      { state with core := next } readFailed plugins { state with core := next }
    rw [committed] at reached
    exact reached

theorem dropped_flags_cover_plugins (host : Host Core Q Doc Val Op) (actor : Actor) (event : Ev)
    (snapshot : State Core Doc Val) (readFailed : String → Bool)
    (plugins : List (Plugin Q Ev V Doc Val Op)) (staged : State Core Doc Val) :
    (stageDropping host actor event snapshot readFailed plugins staged).2.length =
      plugins.length := by
  induction plugins generalizing staged with
  | nil => rfl
  | cons plugin rest ih =>
    simp only [stageDropping]
    cases attempt host actor event snapshot readFailed plugin staged with
    | none => simp [ih staged]
    | some next => simp [ih next]

/-- A plugin that refuses, returns an invalid result, or could not be shown
its rows is owed a delivery; it is not an error. -/
theorem failed_staging_is_owed (host : Host Core Q Doc Val Op) (actor : Actor) (event : Ev)
    (snapshot : State Core Doc Val) (readFailed : String → Bool)
    (plugins : List (Plugin Q Ev V Doc Val Op)) (staged : State Core Doc Val)
    (plugin : Plugin Q Ev V Doc Val Op) (member : plugin ∈ plugins)
    (fails : readFailed plugin.manifest.name = true ∨
      (plugin.hook.run host event snapshot).value? = none ∨
      valid plugin.manifest host.allowed actor
        (plugin.hook.run host event snapshot).effects = false) :
    plugin.manifest.name ∈
      owed plugins (stageDropping host actor event snapshot readFailed plugins staged).2 := by
  induction plugins generalizing staged with
  | nil => cases member
  | cons head rest ih =>
    simp only [stageDropping]
    rcases List.mem_cons.mp member with rfl | later
    · have none : attempt host actor event snapshot readFailed plugin staged = none := by
        unfold attempt
        rcases fails with unreadable | refused | invalid
        · simp [unreadable]
        · split
          · rfl
          · cases decision : plugin.hook.run host event snapshot with
            | refuse reason => rfl
            | proceed value effects => simp [decision, Outcome.value?] at refused
        · split
          · rfl
          · cases decision : plugin.hook.run host event snapshot with
            | refuse reason => rfl
            | proceed value effects =>
              have rejected : valid plugin.manifest host.allowed actor effects = false := by
                simpa [decision, Outcome.effects] using invalid
              simp [rejected]
      simp [none, owed]
    · cases attempt host actor event snapshot readFailed head staged with
      | none =>
        have tail := ih staged later
        simp only [owed, List.zip_cons_cons, List.filter_cons, List.map_cons] at tail ⊢
        simp only [if_true, List.map_cons, List.mem_cons]
        exact Or.inr tail
      | some next =>
        have tail := ih next later
        simp only [owed, List.zip_cons_cons, List.filter_cons] at tail ⊢
        simpa using tail

omit [DecidableEq Doc] in
/-- An owed staging is published with the terminal and is pending until its
delivery commits, so the recovery sweep finds it. -/
theorem owed_staging_is_pending {Outcome : Type} (plugins : List (Plugin Q Ev V Doc Val Op))
    (dropped : List Bool) (record : Delivery.State Outcome String) (outcome : Outcome)
    (name : String) (fresh : record.terminal = none) (named : name ∈ owed plugins dropped)
    (undelivered : name ∉ record.delivered) :
    name ∈ Delivery.pending (Delivery.publish record outcome (owed plugins dropped) true) :=
  Delivery.published_delivery_is_pending record outcome _ name fresh named undelivered

private theorem stageDropping_all_kept (host : Host Core Q Doc Val Op) (actor : Actor)
    (event : Ev) (snapshot : State Core Doc Val) (readFailed : String → Bool)
    (plugins : List (Plugin Q Ev V Doc Val Op)) (staged : State Core Doc Val)
    (kept : (stageDropping host actor event snapshot readFailed plugins staged).2.all
      (fun flag => !flag) = true) :
    applyEffects host.perform actor
        (plugins.flatMap fun plugin => (plugin.hook.run host event snapshot).effects) staged =
      some (stageDropping host actor event snapshot readFailed plugins staged).1 := by
  induction plugins generalizing staged with
  | nil => rfl
  | cons plugin rest ih =>
    simp only [stageDropping] at kept ⊢
    cases tried : attempt host actor event snapshot readFailed plugin staged with
    | none => simp [tried] at kept
    | some next =>
      simp only [tried, List.all_cons, Bool.not_false, Bool.true_and] at kept ⊢
      have applied : applyEffects host.perform actor
          (plugin.hook.run host event snapshot).effects staged = some next := by
        unfold attempt at tried
        split at tried
        · cases tried
        · cases decision : plugin.hook.run host event snapshot with
          | refuse reason => simp [decision] at tried
          | proceed value effects =>
            simp only [decision] at tried
            split at tried
            · exact tried
            · cases tried
      simp only [List.flatMap_cons, applyEffects_append, applied, Option.some_bind]
      exact ih next kept

/-- Agreement, restated per kind of point. Where refusal is allowed
(`Hooks.staged_state_applies_every_plugin`) a committed transaction always
holds every plugin's effects. Where the transition must commit, that holds
exactly when no plugin was dropped; a dropped plugin's rows lag until its owed
delivery commits. -/
theorem undropped_staging_applies_every_plugin (host : Host Core Q Doc Val Op)
    (plugins : List (Plugin Q Ev V Doc Val Op)) (actor : Actor) (core : Core → Option Core)
    (event : Core → Core → Ev) (readFailed : String → Bool)
    (state final : State Core Doc Val) (dropped : List Bool) (next : Core)
    (transition : core state.core = some next)
    (committed : mustCommit host plugins actor core event readFailed state =
      some (final, dropped))
    (none : dropped.all (fun flag => !flag) = true) :
    applyEffects host.perform actor
        (plugins.flatMap fun plugin =>
          (plugin.hook.run host (event state.core next) { state with core := next }).effects)
        { state with core := next } = some final := by
  simp only [mustCommit, transition, Option.map_some', Option.some.injEq] at committed
  have kept := stageDropping_all_kept host actor (event state.core next)
    { state with core := next } readFailed plugins { state with core := next }
    (by rw [committed]; exact none)
  rw [committed] at kept
  exact kept

/-- The cost, stated: a refusing plugin's rows are exactly as they were while
core's transition has committed. -/
theorem refused_staging_lags_core (host : Host Core Q Doc Val Op)
    (plugin : Plugin Q Ev V Doc Val Op) (actor : Actor) (core : Core → Option Core)
    (event : Core → Core → Ev) (readFailed : String → Bool) (state : State Core Doc Val)
    (next : Core) (reason : String) (transition : core state.core = some next)
    (refused : plugin.hook.run host (event state.core next) { state with core := next } =
      .refuse reason) :
    mustCommit host [plugin] actor core event readFailed state =
      some ({ core := next, docs := state.docs }, [true]) := by
  have none : attempt host actor (event state.core next) { state with core := next } readFailed
      plugin { state with core := next } = none := by
    unfold attempt
    split
    · rfl
    · simp [refused]
  simp [mustCommit, transition, stageDropping, none]

/-- How an error travels from a must-commit point to the store's transaction
owner: an application error's added context, an owner variant with its own
message and the error as its source, an owner variant that forwards display
and `source()` past what it wraps (`#[error(transparent)]`), or a rendering of
the error into new text. -/
inductive Carrier where
  | context
  | sourced
  | transparent
  | stringified
  deriving DecidableEq, Repr

/-- Whether the transaction owner still finds a must-commit point's request to
run the transition again (`stageDropping`'s native premise) after the error
travelled `path`, outermost last.

Decision 2026-11-11, maintainer-approved: the request rides one level below
the store's error (`RerunRequest` as the source of
`ConfigClientError::RerunRequested`), so a transparent owner variant, which
hides only the error it wraps, cannot hide it; owner enums answer the store's
classifier (`TransactionFailure::rerun_requested`) from their own variants.
Only rendering the error into text loses it, and no path from
`request.terminal.stage` or `control.session.halt` to the transaction owner
renders it. -/
def rerunSurvives (path : List Carrier) : Bool :=
  path.all (· != .stringified)

theorem rerun_survives_every_owner_wrapping (path : List Carrier)
    (kept : Carrier.stringified ∉ path) : rerunSurvives path = true := by
  simp only [rerunSurvives, List.all_eq_true, bne_iff_ne, ne_eq]
  intro carrier member same
  exact kept (same ▸ member)

theorem stringified_rerun_is_lost (path : List Carrier)
    (rendered : Carrier.stringified ∈ path) : rerunSurvives path = false := by
  simp only [rerunSurvives, List.all_eq_false]
  exact ⟨_, rendered, by simp⟩

end Hooks
