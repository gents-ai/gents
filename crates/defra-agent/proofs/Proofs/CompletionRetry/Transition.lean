import Proofs.CompletionRetry.State

namespace CompletionRetry

/-- Legal transitions of the per-completion retry machine.

Failure transitions consume the observed `FailureClass`: transport
failures may only take the transport constructors, parse-400s the
resample/repair constructors, permanent classifications only
`failPermanent`. Design invariants are structural here:
- every entry into `backingOff`/`repairing` requires `s.turn.effects = 0`
  or increments the turn index (`continueAfterClose`), so re-issues never
  face open effects (proved as an invariant in `Properties.lean`);
- `retract` requires `s.turn.effects = 0`, zeroes `rendered`, and keeps
  the turn index — it is the only same-turn rendered decrease;
- every entry into `backingOff wake` carries `fitsDeadline wake s.deadline`;
- `repair` requires `¬ s.repairUsed`; `repairIssue` sets it. -/
inductive Transition : State → State → Prop
  /-- Issue the completion for the current turn. -/
  | issue (s : State) (h : s.phase = Phase.issuing) :
      Transition s { s with phase := Phase.streaming }

  /-- A tool executed during the current streaming turn. -/
  | toolEffect (s : State) (h : s.phase = Phase.streaming) :
      Transition s { s with turn := { s.turn with effects := s.turn.effects + 1 } }

  /-- The completion streamed to its end; its turn is retained once. -/
  | streamOk (s : State) (h : s.phase = Phase.streaming) :
      Transition s { s with phase := Phase.turnDone,
                            turn := { s.turn with rendered := 1 } }

  /-- Pre-stream / no-yield transport-class failure with ladder + deadline
  room. -/
  | transportBackoff (s : State) (c : FailureClass) (wake : Time)
      (hc : c = FailureClass.transport)
      (hp : s.phase = Phase.streaming)
      (heff : s.turn.effects = 0)
      (hbudget : s.transportUsed < s.budget.transportRetries)
      (hfit : fitsDeadline wake s.deadline)
      (hfwd : s.now ≤ wake) :
      Transition s { s with phase := Phase.backingOff wake,
                            transportUsed := s.transportUsed + 1 }

  /-- Transport-class failure with no ladder room, or whose SELECTED wake
  time does not fit the deadline → terminal. The chosen wake models Rust's
  fail-fast: the jittered ladder delay is picked first, and if `now + delay`
  passes a still-future deadline the attempt fails immediately rather than
  sleeping into certain death. Also covers the retract case (same phase,
  budget/deadline exhausted). -/
  | transportExhaust (s : State) (c : FailureClass) (wake : Time)
      (hc : c = FailureClass.transport)
      (hp : s.phase = Phase.streaming)
      (hfwd : s.now ≤ wake)
      (h : s.transportUsed ≥ s.budget.transportRetries ∨
           ¬ fitsDeadline wake s.deadline) :
      Transition s { s with phase := Phase.exhausted }

  /-- Fresh parse-400 (differs from the last seen) with resample room. -/
  | resampleBackoff (s : State) (c : FailureClass) (err : String) (wake : Time)
      (hc : c = FailureClass.parseBadRequest)
      (hp : s.phase = Phase.streaming)
      (heff : s.turn.effects = 0)
      (hfresh : s.lastParseError ≠ some err)
      (hbudget : s.resampleUsed < s.budget.resampleRetries)
      (hfit : fitsDeadline wake s.deadline)
      (hfwd : s.now ≤ wake) :
      Transition s { s with phase := Phase.backingOff wake,
                            resampleUsed := s.resampleUsed + 1,
                            lastParseError := some err }

  /-- Deterministic parse-400 (identical to last) or resample budget spent:
  go straight to repair — at most once per request. -/
  | repair (s : State) (c : FailureClass) (err : String)
      (hc : c = FailureClass.parseBadRequest)
      (hp : s.phase = Phase.streaming)
      (heff : s.turn.effects = 0)
      (hdet : s.lastParseError = some err ∨ s.resampleUsed ≥ s.budget.resampleRetries)
      (hallow : s.budget.allowRepair)
      (hunused : ¬ s.repairUsed) :
      Transition s { s with phase := Phase.repairing,
                            lastParseError := some err }

  /-- Parse-400 that can make no progress → terminal: neither a fresh resample
  (with ladder + deadline room, no open effects) nor a one-shot repair is
  enabled for this error. The selected resample wake is carried so that a
  deadline overshoot (`¬ fitsDeadline`) participates in `hno_resample`, giving
  the same fail-fast decision as the transport ladder. -/
  | parseExhaust (s : State) (c : FailureClass) (err : String) (wake : Time)
      (hc : c = FailureClass.parseBadRequest)
      (hp : s.phase = Phase.streaming)
      (hfwd : s.now ≤ wake)
      (hno_resample :
        ¬ (s.turn.effects = 0 ∧ s.lastParseError ≠ some err ∧
           s.resampleUsed < s.budget.resampleRetries ∧
           fitsDeadline wake s.deadline ∧ s.now ≤ wake))
      (hno_repair :
        ¬ (s.turn.effects = 0 ∧
           (s.lastParseError = some err ∨ s.resampleUsed ≥ s.budget.resampleRetries) ∧
           s.budget.allowRepair ∧ ¬ s.repairUsed)) :
      Transition s { s with phase := Phase.exhausted }

  /-- Permanent classification → terminal, immediately. -/
  | failPermanent (s : State) (c : FailureClass)
      (hc : c = FailureClass.permanent)
      (hp : s.phase = Phase.streaming) :
      Transition s { s with phase := Phase.failedPermanent }

  /-- Mid-stream failure with NO effects this turn: retract the partial
  render, then back off toward a resample of the same turn. -/
  | retract (s : State) (wake : Time)
      (hp : s.phase = Phase.streaming)
      (heff : s.turn.effects = 0)
      (hbudget : s.transportUsed < s.budget.transportRetries)
      (hfit : fitsDeadline wake s.deadline)
      (hfwd : s.now ≤ wake) :
      Transition s { s with phase := Phase.backingOff wake,
                            transportUsed := s.transportUsed + 1,
                            turn := { s.turn with rendered := 0 } }

  /-- Mid-stream failure WITH effects: close the turn durably (partial
  assistant turn + executed tool results threaded); rendered content is
  retained and frozen. -/
  | closeTurn (s : State)
      (hp : s.phase = Phase.streaming)
      (heff : 0 < s.turn.effects) :
      Transition s { s with phase := Phase.turnClosed,
                            turn := { s.turn with rendered := 1 } }

  /-- Continue after a closed turn: next completion begins a NEW turn —
  the turn index advances and effect/render counters reset (this is NOT a
  retraction of the closed turn, whose rendered content is frozen under
  its own index). Budget consumed like a transport retry. -/
  | continueAfterClose (s : State) (wake : Time)
      (hp : s.phase = Phase.turnClosed)
      (hbudget : s.transportUsed < s.budget.transportRetries)
      (hfit : fitsDeadline wake s.deadline)
      (hfwd : s.now ≤ wake) :
      Transition s { s with phase := Phase.backingOff wake,
                            transportUsed := s.transportUsed + 1,
                            turn := { turnIndex := s.turn.turnIndex + 1,
                                      effects := 0, rendered := 0 } }

  /-- A closed turn with no ladder room, or whose selected wake does not
  fit the deadline → terminal. -/
  | closeExhaust (s : State) (wake : Time)
      (hp : s.phase = Phase.turnClosed)
      (hfwd : s.now ≤ wake)
      (h : s.transportUsed ≥ s.budget.transportRetries ∨
           ¬ fitsDeadline wake s.deadline) :
      Transition s { s with phase := Phase.exhausted }

  /-- Wake from backoff and re-issue. Clock moves to the wake time. -/
  | wake (s : State) (w : Time) (hp : s.phase = Phase.backingOff w) :
      Transition s { s with phase := Phase.issuing, now := w }

  /-- Repair mutates the assembled input (sanitizer pass), then re-issues.
  Marks repair used. -/
  | repairIssue (s : State)
      (hp : s.phase = Phase.repairing)
      (hunused : ¬ s.repairUsed) :
      Transition s { s with phase := Phase.issuing, repairUsed := true }

end CompletionRetry
