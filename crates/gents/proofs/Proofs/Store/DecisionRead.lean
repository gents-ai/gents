/-! A read a decision is taken on: the absence of a row it does not return is
acted upon (a receipt to withdraw, an owed removal, a client access request,
the enrollment projection). -/

namespace DecisionRead

/-- Why a strict read returned no rows. External premise (defradb.rs#3101,
`RequestContext::with_strict_access_reads`): a strict read returns every row
its reader may see, or fails with no rows when a document's access check had
no answer: GraphQL code `ACCESS_CHECK_UNAVAILABLE` when the provider was
unreachable, late, busy or stale once its own waits had run (the same read may
answer later), `ACCESS_CHECK_FAILED` when the check broke otherwise (reading
again does not help). A denial is an answer: it leaves the row out, as a read
that is not strict does. -/
inductive AccessCheck where
  | unavailable
  | failed
  deriving DecidableEq, Repr

/-- The access check a failed strict read reports, from its GraphQL error
code; any other code is not an access check. -/
def accessCheck (code : String) : Option AccessCheck :=
  if code == "ACCESS_CHECK_UNAVAILABLE" then some .unavailable
  else if code == "ACCESS_CHECK_FAILED" then some .failed
  else none

/-- What the caller of a decision read does with what it got back. -/
inductive Step where
  /-- Every row the reader may see came back: decide on them. -/
  | decide
  /-- The runtime cannot answer now: take no decision, and say the caller may
  ask again (the client access endpoint answers 503; a settlement pass is run
  again by its next trigger). -/
  | wait
  /-- A check broke: take no decision and report the failure; asking again
  does not help until its cause is fixed. -/
  | fail
  deriving DecidableEq, Repr

/-- What a strict read came back with: every row its reader may see, or the
access check it failed on. -/
inductive Outcome where
  | complete
  | unavailable
  | failed
  deriving DecidableEq, Repr

/-- The outcome of a strict read that failed on `check`. -/
def Outcome.ofCheck : AccessCheck → Outcome
  | .unavailable => .unavailable
  | .failed => .failed

/-- Decision (gents#3831): a decision read asks DefraDB for a strict read, so
a check without an answer fails the read instead of hiding a row. -/
def step (outcome : Outcome) : Step :=
  match outcome with
  | .complete => .decide
  | .unavailable => .wait
  | .failed => .fail

/-- A decision is taken only on a read whose every check was answered. -/
theorem decides_only_on_a_complete_read (outcome : Outcome)
    (h : step outcome = .decide) : outcome = .complete := by
  cases outcome <;> simp_all [step]

/-- Only a provider that did not answer in time lets the caller ask again. -/
theorem waits_only_when_the_provider_was_late (outcome : Outcome)
    (h : step outcome = .wait) : outcome = .unavailable := by
  cases outcome <;> simp_all [step]

/-- A denial is not an access check failure: its code decides nothing here. -/
theorem a_denial_is_not_an_access_check : accessCheck "PERMISSION_DENIED" = none := rfl

end DecisionRead
