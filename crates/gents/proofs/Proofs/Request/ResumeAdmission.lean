import Proofs.Basic

namespace RequestResumeAdmission

/-- A pending request has no execution deadline. A parked request may retain
one only when its exact, same-document claim version proves that the owner
installed the same binding. The ordinary admission owner still checks the
signature and current requester authority. -/
structure ClaimFacts where
  waiting : Bool
  versionPresent : Bool
  claimed : Bool
  sameIdentity : Bool
  sameDeadline : Bool
  sameBackend : Bool
  deriving Repr, DecidableEq

def claimValid (facts : ClaimFacts) : Bool :=
  facts.waiting && facts.versionPresent && facts.claimed && facts.sameIdentity &&
    facts.sameDeadline && facts.sameBackend

def deadlineAdmitted (deadlineAbsent : Bool) (facts : ClaimFacts) : Bool :=
  deadlineAbsent || claimValid facts

theorem fresh_deadline_still_denied (facts : ClaimFacts)
    (h : facts.waiting = false) : deadlineAdmitted false facts = false := by
  simp [deadlineAdmitted, claimValid, h]

theorem resumed_deadline_requires_claim (facts : ClaimFacts)
    (h : deadlineAdmitted false facts = true) : claimValid facts = true := by
  simpa [deadlineAdmitted] using h

end RequestResumeAdmission
