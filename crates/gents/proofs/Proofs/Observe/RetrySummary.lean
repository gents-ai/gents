import Proofs.Session.Request.BackendWake

namespace RunTimelineRetry

/-- One provider attempt of a request as its `InferenceCall` row records it:
the request-scoped call sequence, whether the attempt failed, and its failure
reason. -/
structure Attempt where
  seq : Nat
  failed : Bool
  reason : Option String
  deriving Repr, DecidableEq

structure Summary where
  retries : Nat
  lastTransient : Option String
  recovered : Bool
  deriving Repr, DecidableEq

/-- A failed attempt is a retry when the request made another attempt after
it. "After" is the request's call sequence, which a resumed execution
continues (`RequestBackendWake.resumedCallSeq`), so it orders the attempts of
parked and resumed executions without reading a clock, and the summary does
not depend on the order the rows were read in. -/
def retried (attempts : List Attempt) (attempt : Attempt) : Bool :=
  attempt.failed && attempts.any fun later => attempt.seq < later.seq

def retries (attempts : List Attempt) : List Attempt :=
  attempts.filter (retried attempts)

/-- The latest attempt by call sequence; the first listed wins a tie. -/
def latest (attempts : List Attempt) : Option Attempt :=
  attempts.foldl (fun best attempt => match best with
    | some kept => if kept.seq < attempt.seq then some attempt else some kept
    | none => some attempt) none

/-- The run timeline's retry summary: every retried attempt across the
request's executions, the reason of the latest one, and whether the request
completed after retrying. -/
def summary (completed : Bool) (attempts : List Attempt) : Summary :=
  let counted := retries attempts
  { retries := counted.length
  , lastTransient := (latest counted).bind (·.reason)
  , recovered := completed && decide (0 < counted.length) }

/-- A failure in a parked execution is counted once the request resumes and
attempts again, because the resumed calls are numbered after it. -/
theorem parked_failure_is_retried_after_resume
    (parked resumed : List Attempt) (attempt next : Attempt)
    (hparked : attempt ∈ parked) (hfailed : attempt.failed = true)
    (hnext : next ∈ resumed)
    (hnumbered : RequestBackendWake.resumedCallSeq (parked.map (·.seq)) < next.seq) :
    attempt ∈ retries (parked ++ resumed) := by
  have bound := RequestBackendWake.persisted_call_precedes_resumed
    (parked.map (·.seq)) attempt.seq (List.mem_map_of_mem _ hparked)
  simp only [retries, List.mem_filter, retried, hfailed, Bool.true_and,
    List.any_eq_true, decide_eq_true_eq]
  exact ⟨List.mem_append_left _ hparked, next, List.mem_append_right _ hnext, by omega⟩

/-- The defect this model fixes: a resumed execution that restarted its call
sequence numbered its first call like the parked one's, hiding the retry. -/
theorem restarted_sequence_hides_a_retry :
    (summary true [⟨1, true, some "503"⟩, ⟨1, false, none⟩]).retries = 0 := by decide

theorem continued_sequence_counts_the_retry :
    summary true [⟨1, true, some "503"⟩, ⟨2, false, none⟩] =
      ⟨1, some "503", true⟩ := by decide

theorem final_attempt_is_not_a_retry (attempts : List Attempt) (attempt : Attempt)
    (hmax : ∀ other ∈ attempts, other.seq ≤ attempt.seq) :
    retried attempts attempt = false := by
  simp only [retried, Bool.and_eq_false_iff, List.any_eq_false, decide_eq_true_eq]
  exact Or.inr fun other hother => by have := hmax other hother; omega

end RunTimelineRetry
