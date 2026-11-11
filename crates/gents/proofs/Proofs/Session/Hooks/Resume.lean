import Proofs.Hooks.Staging

namespace Hooks.Resume

/-- A request core created, with the DID that signed it. -/
structure Request where
  id : Nat
  agent : Actor
  signer : Actor
  deriving DecidableEq, Repr

/-- The part of core state a session resume touches: the session stop latch
and the session's requests. -/
structure Session where
  stopped : Bool
  requests : List Request
  deriving DecidableEq, Repr

/-- The arguments of the core operation `request.create`, as data: the
unsigned request a plugin asks core to create. It names the agent the request
runs as and names no signer, so a plugin cannot choose the authority the
request is created under (decision 2026-11-11, maintainer-approved).

Natively the arguments are `{"request": AgentRequestCreate}` without its
signature; core fills the session and Agent document IDs and the session hop,
binds client targets, signs with its own identity and creates the row. -/
structure CreateArgs where
  id : Nat
  agent : Actor
  deriving DecidableEq, Repr

/-- Core performs a requested creation under its own rule and the
transaction's actor, stated like `Hooks.Safety.perform`: `mayCreate` is core's
decision whether that actor could itself create the request (natively: the
actor is the request's agent and core's signer, and document ACP admits the
write), and the request is signed by that actor. The operation adds a request
and touches nothing else, in particular not the stop latch, so a plugin that
asks for a request in a stopped session still gets no claim of it
(`Hooks.Safety.claimNext`).

Decision 2026-11-11, maintainer-approved: `request.create` joins the
vocabulary a plugin may request as an effect, under the same caller-authority
rule as the stop operations. Hooks narrow authority and never widen it. -/
def perform (mayCreate : Actor → CreateArgs → Session → Bool) (actor : Actor)
    (args : CreateArgs) (session : Session) : Option Session :=
  if mayCreate actor args session = true ∧ !(session.requests.any (·.id == args.id)) = true then
    some { session with requests := ⟨args.id, args.agent, actor⟩ :: session.requests }
  else none

theorem create_needs_the_actors_own_authority (mayCreate : Actor → CreateArgs → Session → Bool)
    (actor : Actor) (args : CreateArgs) (session : Session)
    (denied : mayCreate actor args session = false) :
    perform mayCreate actor args session = none := by
  simp [perform, denied]

/-- A created request is signed by the transaction's actor, and the rest of the
session is unchanged. -/
theorem created_request_is_signed_by_the_actor (mayCreate : Actor → CreateArgs → Session → Bool)
    (actor : Actor) (args : CreateArgs) (session next : Session)
    (performed : perform mayCreate actor args session = some next) :
    mayCreate actor args session = true ∧
      next = { session with requests := ⟨args.id, args.agent, actor⟩ :: session.requests } := by
  unfold perform at performed
  split at performed
  · rename_i accepted
    simp only [Option.some.injEq] at performed
    exact ⟨accepted.1, performed.symm⟩
  · cases performed

private theorem reach_creates_only_as_actor (mayCreate : Actor → CreateArgs → Session → Bool)
    (actor : Actor) (first last : Session) (reached : Reach (perform mayCreate) actor first last) :
    last.stopped = first.stopped ∧
      ∀ request ∈ last.requests, request ∈ first.requests ∨ request.signer = actor := by
  induction reached with
  | refl => exact ⟨rfl, fun request member => Or.inl member⟩
  | step op performed _ ih =>
    obtain ⟨_, rfl⟩ := created_request_is_signed_by_the_actor mayCreate actor op _ _ performed
    obtain ⟨stopped, created⟩ := ih
    refine ⟨stopped, fun request member => ?_⟩
    rcases created request member with old | signed
    · rcases List.mem_cons.mp old with rfl | earlier
      · exact Or.inr rfl
      · exact Or.inl earlier
    · exact Or.inr signed

private theorem failing_effect_applies_nothing {Doc Val Op Core : Type} [DecidableEq Doc]
    (perform : Actor → Op → Core → Option Core) (actor : Actor) (effect : Effect Doc Val Op)
    (fails : ∀ state, applyEffect perform actor state effect = none) :
    ∀ (effects : List (Effect Doc Val Op)) (state : State Core Doc Val),
      effect ∈ effects → applyEffects perform actor effects state = none := by
  intro effects
  induction effects with
  | nil => intro _ member; cases member
  | cons head rest ih =>
    intro state member
    simp only [applyEffects]
    rcases List.mem_cons.mp member with rfl | later
    · simp [fails state]
    · cases applyEffect perform actor state head with
      | none => rfl
      | some next => exact ih next later

/-- `control.session.resume`: core's own transition clears the stop latch once
its quiescence rule (`settled`) holds, then the plugins at the point stage on
the cleared snapshot, refusably (`Hooks.Catalog.controlSessionResume`), in the
same transaction. -/
def resume {Q Ev V Doc Val : Type} [DecidableEq Doc] (host : Host Session Q Doc Val CreateArgs)
    (plugins : List (Plugin Q Ev V Doc Val CreateArgs)) (settled : Session → Bool)
    (actor : Actor) (event : Ev) (state : State Session Doc Val) :
    Except Refusal (State Session Doc Val × List V) :=
  if settled state.core then
    stage host plugins actor event { state with core := { state.core with stopped := false } }
  else .error .core

variable {Q Ev V Doc Val : Type} [DecidableEq Doc]

/-- A plugin's refusal at resume commits nothing: the latch stays set and no
request is created. -/
theorem resume_refusal_commits_nothing (host : Host Session Q Doc Val CreateArgs)
    (plugins : List (Plugin Q Ev V Doc Val CreateArgs)) (settled : Session → Bool)
    (actor : Actor) (event : Ev) (state : State Session Doc Val)
    (plugin : Plugin Q Ev V Doc Val CreateArgs) (member : plugin ∈ plugins) (reason : String)
    (refused : plugin.hook.run host event
      { state with core := { state.core with stopped := false } } = .refuse reason) :
    settle state (resume host plugins settled actor event state) = state := by
  unfold resume
  split
  · cases staged : stage host plugins actor event
        { state with core := { state.core with stopped := false } } with
    | error refusal => rfl
    | ok result =>
      obtain ⟨final, values⟩ := result
      have decided := staged_values_are_snapshot_decisions host plugins actor event _ final values
        staged
      have present := List.mem_map_of_mem
        (fun plugin => (plugin.hook.run host event
          { state with core := { state.core with stopped := false } }).value?) member
      rw [decided] at present
      simp [refused, Outcome.value?] at present
  · rfl

/-- A creation the actor could not perform itself commits nothing: the whole
resume, latch included, is not applied. -/
theorem resume_failed_create_commits_nothing (mayCreate : Actor → CreateArgs → Session → Bool)
    (select : Session → Store Doc Val → Q → List (Row Doc Val)) (allowed : Acp Doc)
    (plugins : List (Plugin Q Ev V Doc Val CreateArgs)) (settled : Session → Bool)
    (actor : Actor) (event : Ev) (state : State Session Doc Val)
    (plugin : Plugin Q Ev V Doc Val CreateArgs) (member : plugin ∈ plugins) (args : CreateArgs)
    (requested : Effect.core args ∈ (plugin.hook.run ⟨select, perform mayCreate, allowed⟩ event
      { state with core := { state.core with stopped := false } }).effects)
    (denied : ∀ session, mayCreate actor args session = false) :
    settle state (resume ⟨select, perform mayCreate, allowed⟩ plugins settled actor event state) =
      state := by
  unfold resume
  split
  · cases staged : stage ⟨select, perform mayCreate, allowed⟩ plugins actor event
        { state with core := { state.core with stopped := false } } with
    | error refusal => rfl
    | ok result =>
      obtain ⟨final, values⟩ := result
      have applied := staged_state_applies_every_plugin _ plugins actor event _ final values staged
      have fails : ∀ staged : State Session Doc Val,
          applyEffect (perform mayCreate) actor staged (Effect.core args : Effect Doc Val CreateArgs) =
            none := by
        intro staged
        simp [applyEffect, create_needs_the_actors_own_authority mayCreate actor args _
          (denied staged.core)]
      rw [failing_effect_applies_nothing (perform mayCreate) actor _ fails _ _
        (List.mem_flatMap.mpr ⟨plugin, member, requested⟩)] at applied
      cases applied
  · rfl

/-- Creation never widens authority: a committed resume leaves the latch
cleared, and every request it holds that the session did not hold before was
created and signed under the transaction's actor. -/
theorem resume_creates_only_as_the_actor (mayCreate : Actor → CreateArgs → Session → Bool)
    (select : Session → Store Doc Val → Q → List (Row Doc Val)) (allowed : Acp Doc)
    (plugins : List (Plugin Q Ev V Doc Val CreateArgs)) (settled : Session → Bool)
    (actor : Actor) (event : Ev) (state final : State Session Doc Val) (values : List V)
    (ok : resume ⟨select, perform mayCreate, allowed⟩ plugins settled actor event state =
      .ok (final, values)) :
    final.core.stopped = false ∧
      ∀ request ∈ final.core.requests,
        request ∈ state.core.requests ∨ request.signer = actor := by
  unfold resume at ok
  split at ok
  · have reached := staging_moves_core_only_through_perform _ plugins actor event _ final values ok
    exact reach_creates_only_as_actor mayCreate actor _ _ reached
  · cases ok

/-- Core's own rule runs first: unsettled work refuses the resume before any
plugin is asked. -/
theorem unsettled_resume_commits_nothing (host : Host Session Q Doc Val CreateArgs)
    (plugins : List (Plugin Q Ev V Doc Val CreateArgs)) (settled : Session → Bool)
    (actor : Actor) (event : Ev) (state : State Session Doc Val)
    (unsettled : settled state.core = false) :
    resume host plugins settled actor event state = .error .core := by
  simp [resume, unsettled]

end Hooks.Resume
