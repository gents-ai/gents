import Proofs.CompletionRetry.State

namespace CompletionRetry

inductive Transition : State → State → Prop
  | issue (s : State) (h : s.phase = Phase.issuing) :
      Transition s { s with phase := Phase.streaming }

  | toolEffect (s : State) (h : s.phase = Phase.streaming) :
      Transition s { s with turn := { s.turn with effects := s.turn.effects + 1 } }

  | streamOk (s : State) (h : s.phase = Phase.streaming) :
      Transition s { s with phase := Phase.turnDone,
                            turn := { s.turn with rendered := 1 } }

  | transportBackoff (s : State) (c : FailureClass) (wake : Time)
      (hc : c = FailureClass.transport)
      (hp : s.phase = Phase.streaming)
      (heff : s.turn.effects = 0)
      (hbudget : s.transportUsed < s.budget.transportRetries)
      (hfit : fitsDeadline wake s.deadline)
      (hfwd : s.now ≤ wake) :
      Transition s { s with phase := Phase.backingOff wake,
                            transportUsed := s.transportUsed + 1 }

  | transportExhaust (s : State) (c : FailureClass) (wake : Time)
      (hc : c = FailureClass.transport)
      (hp : s.phase = Phase.streaming)
      (hfwd : s.now ≤ wake)
      (h : s.transportUsed ≥ s.budget.transportRetries ∨
           ¬ fitsDeadline wake s.deadline) :
      Transition s { s with phase := Phase.exhausted }

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

  | repair (s : State) (c : FailureClass) (err : String)
      (hc : c = FailureClass.parseBadRequest)
      (hp : s.phase = Phase.streaming)
      (heff : s.turn.effects = 0)
      (hdet : s.lastParseError = some err ∨ s.resampleUsed ≥ s.budget.resampleRetries)
      (hallow : s.budget.allowRepair)
      (hunused : ¬ s.repairUsed) :
      Transition s { s with phase := Phase.repairing,
                            lastParseError := some err }

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

  | failPermanent (s : State) (c : FailureClass)
      (hc : c = FailureClass.permanent)
      (hp : s.phase = Phase.streaming) :
      Transition s { s with phase := Phase.failedPermanent }

  | retract (s : State) (wake : Time)
      (hp : s.phase = Phase.streaming)
      (heff : s.turn.effects = 0)
      (hbudget : s.transportUsed < s.budget.transportRetries)
      (hfit : fitsDeadline wake s.deadline)
      (hfwd : s.now ≤ wake) :
      Transition s { s with phase := Phase.backingOff wake,
                            transportUsed := s.transportUsed + 1,
                            turn := { s.turn with rendered := 0 } }

  | closeTurn (s : State)
      (hp : s.phase = Phase.streaming)
      (heff : 0 < s.turn.effects) :
      Transition s { s with phase := Phase.turnClosed,
                            turn := { s.turn with rendered := 1 } }

  | continueAfterClose (s : State) (wake : Time)
      (hp : s.phase = Phase.turnClosed)
      (hbudget : s.transportUsed < s.budget.transportRetries)
      (hfit : fitsDeadline wake s.deadline)
      (hfwd : s.now ≤ wake) :
      Transition s { s with phase := Phase.backingOff wake,
                            transportUsed := s.transportUsed + 1,
                            turn := { turnIndex := s.turn.turnIndex + 1,
                                      effects := 0, rendered := 0 } }

  | closeExhaust (s : State) (wake : Time)
      (hp : s.phase = Phase.turnClosed)
      (hfwd : s.now ≤ wake)
      (h : s.transportUsed ≥ s.budget.transportRetries ∨
           ¬ fitsDeadline wake s.deadline) :
      Transition s { s with phase := Phase.exhausted }

  | wake (s : State) (w : Time) (hp : s.phase = Phase.backingOff w) :
      Transition s { s with phase := Phase.issuing, now := w }

  | repairIssue (s : State)
      (hp : s.phase = Phase.repairing)
      (hunused : ¬ s.repairUsed) :
      Transition s { s with phase := Phase.issuing, repairUsed := true }

end CompletionRetry
