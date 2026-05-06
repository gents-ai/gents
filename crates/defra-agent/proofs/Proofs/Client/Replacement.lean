import Proofs.Client.Terminal

/-!
# Client Turn Replacement

Determinism and retry/replacement facts for turn attempt chains.
-/

/-! ## T1: Merge Assumption / Determinism

    Under the merged-snapshot assumption (DefraDB delivers CRDT-merged
    latest per document), equivalent document sets are expected to be
    normalized to the same observation list before `deriveTurn` runs.

    The proof below is `rfl` — it is purely declarative, restating that
    a pure function is deterministic on that normalized list. The real
    convergence guarantee is carried by DefraDB's CRDT merge semantics,
    which live outside this Lean model. This theorem's purpose is to
    document the dependency so a reader knows merge convergence is
    *assumed* here, not *proven* here.
-/

/-- T1 support: deriveTurn is deterministic on a normalized observation list. -/
theorem deriveTurn_deterministic
    (attempts : List AttemptView) :
    deriveTurn attempts = deriveTurn attempts := rfl

/-! ## Theorem T5: Turn Replacement

    Adding a retry attempt to the end of a normalized chain changes the tip.
    deriveTurn of the extended chain equals deriveAttempt of the new tip.

    The rank relationship depends on the scenario:
    - Supersession (isSuperseded set on old tip): rank stays at 2
    - Retry restart (old tip was failed, new attempt is pending):
      rank decreases from 2 to 0. This is the one allowed decrease.
-/

/-- T5a: extending the chain with a new attempt always derives from
    the new attempt. -/
theorem turn_replacement_derives_new_tip
    (attempts : List AttemptView)
    (newTip : AttemptView) :
    deriveTurn (attempts ++ [newTip]) = some (deriveAttempt newTip) :=
  deriveTurn_append_singleton attempts newTip

/-- T5b: supersession always produces rank 2. -/
theorem supersession_rank
    (view : AttemptView)
    (h_super : view.request.isSuperseded = true) :
    (deriveAttempt view).rank = 2 := by
  simp [deriveAttempt, h_super, ClientTurnState.rank]

/-- T5c: retry restart is the one case where a new tip can have lower
    rank than the old tip. The new tip is waitingForClaim (rank 0). -/
theorem retry_restart_state
    (newTip : AttemptView)
    (h_pending : newTip.request.lifecycleState = .pending)
    (h_not_super : newTip.request.isSuperseded = false)
    (h_no_resp : newTip.response = none) :
    deriveAttempt newTip = .waitingForClaim := by
  simp [deriveAttempt, h_not_super, h_pending, h_no_resp]
