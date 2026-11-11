import Proofs.Session.Hooks.Returned

namespace Hooks

/-- Core's own transitions at the claim boundary, parameters the plugins
cannot reach. `claim` is the pending to claimed compare-and-set with every
guard core keeps (queue order, the session stop latch, client authorization);
`none` means it affected no row. `retire` and `reject` are the shared pending
terminal paths (superseded, failed) with their dependents. -/
structure ClaimCore (Core : Type) where
  claim : Core → Option Core
  retire : String → Core → Option Core
  reject : String → Core → Option Core

inductive ClaimResult where
  | claimed
  | lost
  | queued
  | retired (reason : String)
  | rejected (reason : String)
  deriving DecidableEq, Repr

variable {Core Q EvG EvA Doc Val Op : Type}

/-- `request.claim.gate`: every plugin answers on the transaction's snapshot.

Design choice: a gate returns a verdict and no effects; a result that carries
any is invalid. The Goal claim fence writes today (it supersedes the request
being claimed), but that write is the transition its own verdict names, so it
becomes `retire`. The alternative, applying gate effects, needs a rule for
whose effects survive when another plugin vetoes; state a plugin wants to keep
about a veto is staged where core performs that transition
(`request.terminal.stage`). -/
def gate (host : Host Core Q Doc Val Op) (event : EvG) (snapshot : State Core Doc Val) :
    List (Plugin Q EvG ClaimVerdict Doc Val Op) → Except Refusal (List ClaimVerdict)
  | [] => .ok []
  | plugin :: rest =>
      match plugin.hook.run host event snapshot with
      | .refuse reason => .error (.refused plugin.manifest.name reason)
      | .proceed verdict effects =>
          if effects.isEmpty then
            match gate host event snapshot rest with
            | .error refusal => .error refusal
            | .ok verdicts => .ok (verdict :: verdicts)
          else .error (.invalid plugin.manifest.name)

variable [DecidableEq Doc]

/-- One claim transaction: gate, core's transition for the combined verdict,
then `request.claim.applied` for a claim that affected the row. -/
def claimStep (host : Host Core Q Doc Val Op)
    (gates : List (Plugin Q EvG ClaimVerdict Doc Val Op))
    (applied : List (Plugin Q EvA Unit Doc Val Op)) (actor : Actor)
    (gateEvent : Core → EvG) (appliedEvent : Core → Core → EvA) (core : ClaimCore Core)
    (state : State Core Doc Val) : Except Refusal (State Core Doc Val × ClaimResult) :=
  match gate host (gateEvent state.core) state gates with
  | .error refusal => .error refusal
  | .ok verdicts =>
      match combine verdicts with
      | .allow =>
          match core.claim state.core with
          | none => .ok (state, .lost)
          | some next =>
              match stage host applied actor (appliedEvent state.core next)
                  { state with core := next } with
              | .error refusal => .error refusal
              | .ok (final, _) => .ok (final, .claimed)
      | .leaveQueued => .ok (state, .queued)
      | .retire reason =>
          match core.retire reason state.core with
          | none => .error .core
          | some next => .ok ({ state with core := next }, .retired reason)
      | .reject reason =>
          match core.reject reason state.core with
          | none => .error .core
          | some next => .ok ({ state with core := next }, .rejected reason)

omit [DecidableEq Doc] in
theorem gate_verdicts_are_snapshot_decisions (host : Host Core Q Doc Val Op) (event : EvG)
    (snapshot : State Core Doc Val) (plugins : List (Plugin Q EvG ClaimVerdict Doc Val Op))
    (verdicts : List ClaimVerdict) (ok : gate host event snapshot plugins = .ok verdicts) :
    plugins.map (fun plugin => plugin.hook.run host event snapshot) =
      verdicts.map fun verdict => .proceed verdict [] := by
  induction plugins generalizing verdicts with
  | nil =>
    simp only [gate, Except.ok.injEq] at ok
    subst ok
    rfl
  | cons plugin rest ih =>
    cases decision : plugin.hook.run host event snapshot with
    | refuse reason => simp [gate, decision] at ok
    | proceed verdict effects =>
      simp only [gate, decision] at ok
      cases effects with
      | cons _ _ => simp at ok
      | nil =>
        simp only [List.isEmpty_nil, if_true] at ok
        cases tail : gate host event snapshot rest with
        | error refusal => simp [tail] at ok
        | ok others =>
          simp only [tail, Except.ok.injEq] at ok
          subst ok
          simp [decision, ih others tail]

/-- A request is claimed only if every gate plugin allowed it and core's own
claim succeeded; what follows the claim is only core operations. No plugin
verdict can produce a claim core's transition refuses. -/
theorem claimed_requires_every_gate_and_core (host : Host Core Q Doc Val Op)
    (gates : List (Plugin Q EvG ClaimVerdict Doc Val Op))
    (applied : List (Plugin Q EvA Unit Doc Val Op)) (actor : Actor)
    (gateEvent : Core → EvG) (appliedEvent : Core → Core → EvA) (core : ClaimCore Core)
    (state final : State Core Doc Val)
    (claimed : claimStep host gates applied actor gateEvent appliedEvent core state =
      .ok (final, .claimed)) :
    ∃ verdicts next, gate host (gateEvent state.core) state gates = .ok verdicts ∧
      (∀ verdict ∈ verdicts, verdict = .allow) ∧ core.claim state.core = some next ∧
      Reach host.perform actor next final.core := by
  unfold claimStep at claimed
  cases gated : gate host (gateEvent state.core) state gates with
  | error refusal => simp [gated] at claimed
  | ok verdicts =>
    simp only [gated] at claimed
    cases combined : combine verdicts with
    | allow =>
      simp only [combined] at claimed
      cases transition : core.claim state.core with
      | none => simp [transition] at claimed
      | some next =>
        simp only [transition] at claimed
        cases staged : stage host applied actor (appliedEvent state.core next)
            { state with core := next } with
        | error refusal => simp [staged] at claimed
        | ok result =>
          obtain ⟨last, values⟩ := result
          simp only [staged, Except.ok.injEq, Prod.mk.injEq, and_true] at claimed
          subst claimed
          exact ⟨verdicts, next, rfl, (combined_allow_iff verdicts).mp combined, rfl,
            staging_moves_core_only_through_perform host applied actor _ _ last values staged⟩
    | leaveQueued => simp [combined] at claimed
    | retire reason =>
      simp only [combined] at claimed
      cases transition : core.retire reason state.core <;> simp [transition] at claimed
    | reject reason =>
      simp only [combined] at claimed
      cases transition : core.reject reason state.core <;> simp [transition] at claimed

/-- Anything other than a claim leaves every plugin's rows as they were: the
gate cannot write, and a retired or rejected request changes only core state,
through core's transition. -/
theorem unclaimed_outcome_writes_no_plugin_rows (host : Host Core Q Doc Val Op)
    (gates : List (Plugin Q EvG ClaimVerdict Doc Val Op))
    (applied : List (Plugin Q EvA Unit Doc Val Op)) (actor : Actor)
    (gateEvent : Core → EvG) (appliedEvent : Core → Core → EvA) (core : ClaimCore Core)
    (state final : State Core Doc Val) (result : ClaimResult)
    (ok : claimStep host gates applied actor gateEvent appliedEvent core state =
      .ok (final, result))
    (unclaimed : result ≠ .claimed) : final.docs = state.docs := by
  unfold claimStep at ok
  cases gated : gate host (gateEvent state.core) state gates with
  | error refusal => simp [gated] at ok
  | ok verdicts =>
    simp only [gated] at ok
    cases combined : combine verdicts with
    | allow =>
      simp only [combined] at ok
      cases transition : core.claim state.core with
      | none =>
        simp only [transition, Except.ok.injEq, Prod.mk.injEq] at ok
        rw [← ok.1]
      | some next =>
        simp only [transition] at ok
        cases staged : stage host applied actor (appliedEvent state.core next)
            { state with core := next } with
        | error refusal => simp [staged] at ok
        | ok staging =>
          obtain ⟨last, values⟩ := staging
          simp only [staged, Except.ok.injEq, Prod.mk.injEq] at ok
          exact (unclaimed ok.2.symm).elim
    | leaveQueued =>
      simp only [combined, Except.ok.injEq, Prod.mk.injEq] at ok
      rw [← ok.1]
    | retire reason =>
      simp only [combined] at ok
      cases transition : core.retire reason state.core with
      | none => simp [transition] at ok
      | some next =>
        simp only [transition, Except.ok.injEq, Prod.mk.injEq] at ok
        rw [← ok.1]
    | reject reason =>
      simp only [combined] at ok
      cases transition : core.reject reason state.core with
      | none => simp [transition] at ok
      | some next =>
        simp only [transition, Except.ok.injEq, Prod.mk.injEq] at ok
        rw [← ok.1]

/-- A retirement is core's `retire`, applied to the state the gate saw. -/
theorem retired_is_cores_transition (host : Host Core Q Doc Val Op)
    (gates : List (Plugin Q EvG ClaimVerdict Doc Val Op))
    (applied : List (Plugin Q EvA Unit Doc Val Op)) (actor : Actor)
    (gateEvent : Core → EvG) (appliedEvent : Core → Core → EvA) (core : ClaimCore Core)
    (state final : State Core Doc Val) (reason : String)
    (ok : claimStep host gates applied actor gateEvent appliedEvent core state =
      .ok (final, .retired reason)) :
    core.retire reason state.core = some final.core := by
  unfold claimStep at ok
  cases gated : gate host (gateEvent state.core) state gates with
  | error refusal => simp [gated] at ok
  | ok verdicts =>
    simp only [gated] at ok
    cases combined : combine verdicts with
    | allow =>
      simp only [combined] at ok
      cases transition : core.claim state.core with
      | none => simp [transition] at ok
      | some next =>
        simp only [transition] at ok
        cases staged : stage host applied actor (appliedEvent state.core next)
            { state with core := next } with
        | error refusal => simp [staged] at ok
        | ok staging =>
          obtain ⟨last, values⟩ := staging
          simp [staged] at ok
    | leaveQueued => simp [combined] at ok
    | retire given =>
      simp only [combined] at ok
      cases transition : core.retire given state.core with
      | none => simp [transition] at ok
      | some next =>
        simp only [transition, Except.ok.injEq, Prod.mk.injEq, ClaimResult.retired.injEq] at ok
        obtain ⟨rfl, rfl⟩ := ok
        exact transition
    | reject given =>
      simp only [combined] at ok
      cases transition : core.reject given state.core <;> simp [transition] at ok

end Hooks
