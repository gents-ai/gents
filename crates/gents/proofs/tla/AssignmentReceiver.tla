---- MODULE AssignmentReceiver ----
\* lean: AssignmentReceiver.permits
\* crates: gents-store
EXTENDS Naturals, FiniteSets, LeanDecision_AssignmentReceiver

CONSTANTS IgnoreFloor, IgnoreStop, IgnoreBody
VARIABLES durable, highest, active, body, pending
vars == <<durable, highest, active, body, pending>>

Assigned(revision) == IF revision = 0 THEN {"B"} ELSE {"C"}

Init ==
  /\ durable = 0 /\ highest = 0
  /\ active = {"B"} /\ body = {"B"}
  /\ pending = 2

Capture(revision) ==
  /\ pending = 2 /\ revision \in {0, 1}
  /\ pending' = revision
  /\ UNCHANGED <<durable, highest, active, body>>

StopWithdrawn ==
  /\ pending # 2
  /\ active' = active \cap Assigned(pending)
  /\ active' # active
  /\ UNCHANGED <<durable, highest, body, pending>>

Commit ==
  /\ pending # 2
  /\ (IgnoreFloor \/ pending >= durable)
  /\ (IgnoreStop \/ active \subseteq Assigned(pending))
  /\ durable' = pending
  /\ highest' = IF pending > highest THEN pending ELSE highest
  /\ pending' = 2
  /\ UNCHANGED <<active, body>>

Start(id) ==
  /\ pending = 2 /\ id \in {"B", "C"} /\ id \notin active
  /\ permits(id \in Assigned(durable), IgnoreBody \/ id \in body)
  /\ active' = active \cup {id}
  /\ UNCHANGED <<durable, highest, body, pending>>

BodyArrives ==
  /\ "C" \notin body
  /\ body' = body \cup {"C"}
  /\ UNCHANGED <<durable, highest, active, pending>>

Crash ==
  /\ (active # {} \/ pending # 2)
  /\ active' = {} /\ pending' = 2
  /\ UNCHANGED <<durable, highest, body>>

Next == (\E revision \in {0, 1}: Capture(revision)) \/ StopWithdrawn \/ Commit \/
  (\E id \in {"B", "C"}: Start(id)) \/ BodyArrives \/ Crash

Spec == Init /\ [][Next]_vars
NoRevival == highest = 1 => "B" \notin active
VerifiedBody == active \subseteq body
====
