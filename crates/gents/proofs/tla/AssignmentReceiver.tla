---- MODULE AssignmentReceiver ----
\* lean: AssignmentReceiver.permits
\* crates: gents-store
EXTENDS Naturals, FiniteSets, LeanDecision_AssignmentReceiver

CONSTANTS IgnoreFloor, IgnoreStop, IgnoreBody
VARIABLES durable, highest, active, body, pending, floor, stopped
vars == <<durable, highest, active, body, pending, floor, stopped>>

Assigned(revision) == IF revision = 0 THEN {"B"} ELSE {"C"}

Init ==
  /\ durable = 0 /\ highest = 0
  /\ active = {"B"} /\ body = {"B"}
  /\ pending = 2 /\ floor = 0 /\ stopped = FALSE

Capture(revision) ==
  /\ pending = 2 /\ revision \in {0, 1}
  /\ (IgnoreFloor \/ revision >= floor)
  /\ floor' = revision
  /\ pending' = revision
  /\ UNCHANGED <<durable, highest, active, body, stopped>>

StopWithdrawn ==
  /\ (pending # 2 \/ floor > durable)
  /\ active' = active \cap Assigned(IF pending = 2 THEN floor ELSE pending)
  /\ active' # active
  /\ stopped' = (stopped \/ ("B" \in active /\ "B" \notin active'))
  /\ UNCHANGED <<durable, highest, body, pending, floor>>

Commit ==
  /\ pending # 2
  /\ (IgnoreFloor \/ pending >= durable)
  /\ (IgnoreStop \/ active \subseteq Assigned(pending))
  /\ durable' = pending
  /\ highest' = IF pending > highest THEN pending ELSE highest
  /\ pending' = 2
  /\ UNCHANGED <<active, body, floor, stopped>>

Start(id) ==
  /\ pending = 2 /\ id \in {"B", "C"} /\ id \notin active
  /\ (IgnoreFloor \/ durable >= floor)
  /\ permits(id \in Assigned(durable), IgnoreBody \/ id \in body)
  /\ active' = active \cup {id}
  /\ UNCHANGED <<durable, highest, body, pending, floor, stopped>>

BodyArrives ==
  /\ "C" \notin body
  /\ body' = body \cup {"C"}
  /\ UNCHANGED <<durable, highest, active, pending, floor, stopped>>

Crash(recoveredFloor) ==
  /\ (active # {} \/ pending # 2)
  /\ active' = {} /\ pending' = 2
  /\ floor' = recoveredFloor
  /\ UNCHANGED <<durable, highest, body, stopped>>

Next(recoveredFloor) == (\E revision \in {0, 1}: Capture(revision)) \/ StopWithdrawn \/ Commit \/
  (\E id \in {"B", "C"}: Start(id)) \/ BodyArrives \/ Crash(recoveredFloor)

Spec == Init /\ [][Next(floor)]_vars
NoRevival == highest = 1 => "B" \notin active
StoppedStaysStopped == stopped => "B" \notin active
VerifiedBody == active \subseteq body
====
