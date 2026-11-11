namespace StorageFailure

/-- Only the database's stable code establishes full storage. Error text cannot
establish a commit outcome. Freeing space does not clear a latched WAL;
recovery must reopen the same store through its existing owner. -/
def isFull (code : String) : Bool := code == "STORAGE_FULL"

/-- A signed submission remains the same submission during storage recovery. -/
structure Submission where
  requestId : Nat
  signedBytes : List Nat
  outcomeUnknown : Bool
  deriving DecidableEq, Repr

/-- An I/O failure is not a rejection or a receipt. Retain the signed payload
and wait for owner reconciliation rather than authorizing another submission. -/
def observe (code : String) (s : Submission) : Submission :=
  if isFull code then { s with outcomeUnknown := true } else s

/-- Readiness writes fail promptly rather than retrying a failed storage owner. -/
def fatalReadiness (code : String) : Bool := isFull code

/-- Full storage cannot authorize the receipt writer's automatic retry loop. -/
def stopAutomaticRetry (code : String) : Bool := isFull code

theorem preserves_identity (code : String) (s : Submission) :
    (observe code s).requestId = s.requestId := by
  unfold observe; split <;> rfl

theorem preserves_signed_bytes (code : String) (s : Submission) :
    (observe code s).signedBytes = s.signedBytes := by
  unfold observe; split <;> rfl

theorem full_is_unknown (s : Submission) :
    (observe "STORAGE_FULL" s).outcomeUnknown = true := rfl

theorem full_stops_readiness_retry : fatalReadiness "STORAGE_FULL" = true := rfl

theorem full_stops_automatic_replay : stopAutomaticRetry "STORAGE_FULL" = true := rfl

end StorageFailure
