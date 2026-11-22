---- MODULE AssignmentShutdown ----
\* lean: AssignmentReceiver.stopForShutdown
\* crates: gents-store
EXTENDS Naturals, LeanDecision_AssignmentReceiver

CONSTANTS CancelCommit, RestartFalse
VARIABLES phase, shutdown, closed, lockHeld, commitInFlight, calls
vars == <<phase, shutdown, closed, lockHeld, commitInFlight, calls>>

Init == /\ phase = "callback" /\ shutdown = FALSE /\ closed = FALSE
        /\ lockHeld = TRUE /\ commitInFlight = FALSE /\ calls = 1

FalseUpdate ==
  /\ phase = "callback" /\ ~shutdown /\ ~closed
  /\ calls' = IF RestartFalse THEN calls + 1 ELSE calls
  /\ UNCHANGED <<phase, shutdown, closed, lockHeld, commitInFlight>>

RequestShutdown == /\ ~shutdown /\ shutdown' = TRUE
  /\ UNCHANGED <<phase, closed, lockHeld, commitInFlight, calls>>

Close == /\ ~closed /\ closed' = TRUE
  /\ UNCHANGED <<phase, shutdown, lockHeld, commitInFlight, calls>>

CancelCallback ==
  /\ phase = "callback" /\ stopForShutdown(shutdown, closed)
  /\ phase' = "done" /\ lockHeld' = FALSE
  /\ UNCHANGED <<shutdown, closed, commitInFlight, calls>>

BeginCommit == /\ phase = "callback" /\ ~stopForShutdown(shutdown, closed)
  /\ phase' = "commit" /\ commitInFlight' = TRUE
  /\ UNCHANGED <<shutdown, closed, lockHeld, calls>>

CancelDuringCommit == /\ CancelCommit /\ phase = "commit"
  /\ stopForShutdown(shutdown, closed)
  /\ phase' = "done" /\ lockHeld' = FALSE
  /\ UNCHANGED <<shutdown, closed, commitInFlight, calls>>

FinishCommit == /\ phase = "commit" /\ phase' = "callback"
  /\ commitInFlight' = FALSE
  /\ UNCHANGED <<shutdown, closed, lockHeld, calls>>

Next == FalseUpdate \/ RequestShutdown \/ Close \/ CancelCallback \/ BeginCommit
        \/ CancelDuringCommit \/ FinishCommit
Spec == Init /\ [][Next]_vars
CommitHoldsLock == commitInFlight => lockHeld
FalseUpdateKeepsCall == calls = 1
====
