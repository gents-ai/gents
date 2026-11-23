---- MODULE MC_AssignmentReceiver_Red_CrashFloor ----
EXTENDS AssignmentReceiver
LoseAcceptedRevision == Init /\ [][Next(durable)]_vars
====
