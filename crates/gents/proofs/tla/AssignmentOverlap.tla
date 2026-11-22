---- MODULE AssignmentOverlap ----
\* lean: AssignmentReceiver.permits
\* crates: gents-store
EXTENDS Naturals, FiniteSets, LeanDecision_AssignmentReceiver

CONSTANT IgnoreLock
Readers == {1, 2}
View(reader) == IF reader = 1 THEN 0 ELSE 1
Assigned(revision) == IF revision = 0 THEN {"B"} ELSE {"C"}

VARIABLES owner, phase, durable, highest, active
vars == <<owner, phase, durable, highest, active>>

Init ==
  /\ owner = 0 /\ phase = [reader \in Readers |-> "idle"]
  /\ durable = 0 /\ highest = 0 /\ active = {}

Acquire(reader) ==
  /\ phase[reader] = "idle" /\ (IgnoreLock \/ owner = 0)
  /\ owner' = IF IgnoreLock THEN owner ELSE reader
  /\ phase' = [phase EXCEPT ![reader] = "reading"]
  /\ UNCHANGED <<durable, highest, active>>

StopWithdrawn(reader) ==
  /\ phase[reader] = "reading"
  /\ active' = active \cap Assigned(View(reader)) /\ active' # active
  /\ UNCHANGED <<owner, phase, durable, highest>>

Commit(reader) ==
  /\ phase[reader] = "reading" /\ View(reader) >= durable
  /\ active \subseteq Assigned(View(reader))
  /\ durable' = View(reader)
  /\ highest' = IF View(reader) > highest THEN View(reader) ELSE highest
  /\ phase' = [phase EXCEPT ![reader] = "committed"]
  /\ UNCHANGED <<owner, active>>

Reject(reader) ==
  /\ phase[reader] = "reading" /\ View(reader) < durable
  /\ phase' = [phase EXCEPT ![reader] = "done"]
  /\ owner' = IF IgnoreLock THEN owner ELSE 0
  /\ UNCHANGED <<durable, highest, active>>

Start(reader) ==
  /\ phase[reader] = "committed"
  /\ LET id == IF View(reader) = 0 THEN "B" ELSE "C"
     IN /\ permits(id \in Assigned(View(reader)), TRUE)
        /\ active' = active \cup {id}
  /\ phase' = [phase EXCEPT ![reader] = "done"]
  /\ owner' = IF IgnoreLock THEN owner ELSE 0
  /\ UNCHANGED <<durable, highest>>

Next == \E reader \in Readers:
  Acquire(reader) \/ StopWithdrawn(reader) \/ Commit(reader) \/ Reject(reader) \/ Start(reader)

Spec == Init /\ [][Next]_vars
NoRevival == highest = 1 => "B" \notin active
====
