import Proofs.Observe.RetrySummary
import Proofs.Conformance.Family

namespace Conformance.RunTimelineRetry
open Lean _root_.RunTimelineRetry

deriving instance ToJson for Attempt, Summary

structure Input where
  name : String
  completed : Bool
  attempts : List Attempt
  deriving ToJson

private def failed (seq : Nat) (reason : String) : Attempt := ⟨seq, true, some reason⟩
private def answered (seq : Nat) : Attempt := ⟨seq, false, none⟩

@[conformance_family]
def summary : Family Input Summary where
  inputs :=
    [ ⟨"no_attempts", true, []⟩
    , ⟨"first_attempt_answers", true, [answered 1]⟩
    , ⟨"only_attempt_fails", false, [failed 1 "terminal"]⟩
    , ⟨"two_retries_then_answer", true, [failed 1 "first", failed 2 "second", answered 3]⟩
    , ⟨"rows_read_out_of_order", true, [answered 3, failed 1 "first", failed 2 "second"]⟩
    , ⟨"terminal_failure_is_not_a_retry", false, [failed 1 "retried", failed 2 "terminal"]⟩
    , ⟨"parked_then_resumed_continues_the_sequence", true,
        [failed 1 "503 before park", failed 2 "503 parked", failed 3 "503 after wake", answered 4]⟩
    , ⟨"restarted_sequence_hides_the_parked_retry", true, [failed 1 "503 parked", answered 1]⟩
    , ⟨"retry_without_a_reason", true, [⟨1, true, none⟩, answered 2]⟩
    , ⟨"latest_retry_reason_wins", true, [failed 2 "later", failed 1 "earlier", answered 5]⟩
    , ⟨"retry_still_in_flight", false, [failed 1 "transient", answered 2]⟩ ]
  model := fun i => _root_.RunTimelineRetry.summary i.completed i.attempts

end Conformance.RunTimelineRetry
