import Proofs.Session.DurableLineage

/-! The lineage partition a session reader lists for one session: the sessions
it started, the sessions it sent requests to, and the sessions it received
requests from. A verified starter is an input here (the signed creation receipt
that `AgentSession.observedStarter` checks). A parent pointer alone is never a
starter: its session is messaging, not creation. -/

namespace SessionPartition
open DurableLineage

structure Session where
  id : String
  starter : Option String

def insertSorted (x : String) : List String → List String
  | [] => [x]
  | y :: ys => if x ≤ y then x :: y :: ys else y :: insertSorted x ys

def sortStrings : List String → List String
  | [] => []
  | x :: xs => insertSorted x (sortStrings xs)

def distinctSorted (xs : List String) : List String :=
  sortStrings xs.eraseDups

def ownRows (rows : List RequestRow) (s : String) : List RequestRow :=
  rows.filter (·.session == s)

def sessionOf (rows : List RequestRow) (doc : String) : Option String :=
  (rows.find? (·.docId == doc)).map (·.session)

def starterOf (sessions : List Session) (s : String) : Option String :=
  (sessions.find? (·.id == s)).bind (·.starter)

def startedList (sessions : List Session) (s : String) : List String :=
  sortStrings ((sessions.filter (·.starter == some s)).map (·.id))

/-- The sessions a session received requests from: the sessions of the requests
its own requests were caused by, except itself and its verified starter. -/
def receivedList (rows : List RequestRow) (sessions : List Session) (s : String) : List String :=
  let starter := starterOf sessions s
  let causes := ((ownRows rows s).filterMap (·.causeRequestDoc)).filterMap (sessionOf rows)
  distinctSorted (causes.filter fun x => x != s && some x != starter)

/-- The sessions a session sent requests to: the sessions of the requests caused
by its own requests, except itself and the sessions it started. -/
def sentList (rows : List RequestRow) (sessions : List Session) (s : String) : List String :=
  let docs := (ownRows rows s).map (·.docId)
  let targets := rows.filterMap fun r =>
    match r.causeRequestDoc with
    | some c => if docs.contains c then some r.session else none
    | none => none
  let started := startedList sessions s
  distinctSorted (targets.filter fun x => x != s && !started.contains x)

section Unverified

private def row (t : Nat) (doc session : String) (cause : Option String) : RequestRow :=
  { createdAt := t, docId := doc, session := session, isPublic := true
  , causeRequestDoc := cause, causeToolCallDoc := none }

private def unverifiedRows : List RequestRow :=
  [ row 1 "p1" "parent" none, row 0 "s1" "existing" none, row 2 "s2" "existing" (some "p1")
  , row 2 "c1" "started" (some "p1"), row 1 "o1" "other" none, row 2 "a0" "started" (some "o1") ]

private def unverifiedSessions : List Session :=
  [ ⟨"parent", none⟩, ⟨"existing", none⟩, ⟨"started", none⟩, ⟨"other", none⟩ ]

/-- A session whose parent pointer has no verified starter is not started by
the session it points at; it is only messaging: the parent sends to it and the
child receives from the parent. -/
theorem a_parent_pointer_without_a_verified_starter_is_only_messaging :
    startedList unverifiedSessions "parent" = [] ∧
    starterOf unverifiedSessions "started" = none ∧
    sentList unverifiedRows unverifiedSessions "parent" = ["existing", "started"] ∧
    receivedList unverifiedRows unverifiedSessions "parent" = [] ∧
    receivedList unverifiedRows unverifiedSessions "started" = ["other", "parent"] ∧
    receivedList unverifiedRows unverifiedSessions "existing" = ["parent"] := by
  decide

end Unverified

/-- A call that two requests name projects to no single request. -/
theorem caused_requests_projection_omits_a_call_with_two_requests (a b : RequestRow) :
    singleRequestProjection (jobRows "call-two" [a, b]) "call-two" = none :=
  two_job_requests_have_no_single_projection "call-two" a b

end SessionPartition
