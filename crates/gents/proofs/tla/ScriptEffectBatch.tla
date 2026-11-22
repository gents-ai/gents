---- MODULE ScriptEffectBatch ----
\* lean: ToolExecution.Parallel.joinsGroup
\* crates: gents-loop, gents-toolcalls
\* models: ToolExecution.Parallel.effectWidth
EXTENDS Naturals, Sequences, FiniteSets, LeanDecision_ToolExecution_Parallel

\* One composing plugin's batch of tool calls through the loop's effect
\* dispatcher. Calls are grouped from the head of the batch: a call joins the
\* group while Lean `joinsGroup` says so, and a call that cannot join starts a
\* group of its own (`effectWidth`). Within a group each call reserves its
\* ordinal and asks its approval in request order; bodies start once the
\* group is approved; the next group starts once this one settled.
\*
\* Scope: approval, body and barrier interleavings only. Skipped, failed,
\* cancelled and terminated members, and the result projection, belong to the
\* dispatcher's own tests.
\*
\* Safeguards, each removed by one constant:
\*   SerialApproval a call asks only once the previous approval returned;
\*   AllApproved    no body starts before its whole group is approved;
\*   DrainBarrier   the next group waits until this one settled.

CONSTANTS SerialApproval, AllApproved, DrainBarrier

Footprints == <<"read", "read", "barrier", "read">>
Limit == 4
N == Len(Footprints)
Calls == 1..N

RECURSIVE Extend(_, _)
Extend(i, n) ==
  IF i + n <= N /\ joinsGroup(Footprints[i + n], FALSE, n >= Limit)
  THEN Extend(i, n + 1) ELSE n

WidthAt(i) ==
  IF i > N THEN 0
  ELSE IF joinsGroup(Footprints[i], FALSE, Limit = 0) THEN Extend(i, 1) ELSE 1

RECURSIVE GroupStart(_, _)
GroupStart(s, i) == IF i < s + WidthAt(s) THEN s ELSE GroupStart(s + WidthAt(s), i)

GroupOf(i) == GroupStart(1, i)..(GroupStart(1, i) + WidthAt(GroupStart(1, i)) - 1)

VARIABLES lo, hi, reserved, asking, approved, running, settled
vars == <<lo, hi, reserved, asking, approved, running, settled>>

Init == /\ lo = 1 /\ hi = WidthAt(1)
        /\ reserved = 0 /\ asking = {} /\ approved = {}
        /\ running = {} /\ settled = {}

Ask(i) == /\ i = reserved + 1 /\ i <= hi
          /\ (SerialApproval => asking = {})
          /\ reserved' = i /\ asking' = asking \cup {i}
          /\ UNCHANGED <<lo, hi, approved, running, settled>>

Approve(i) == /\ i \in asking
              /\ asking' = asking \ {i} /\ approved' = approved \cup {i}
              /\ UNCHANGED <<lo, hi, reserved, running, settled>>

Start(i) == /\ i \in (lo..hi) \cap approved
            /\ i \notin running \cup settled
            /\ (AllApproved => (lo..hi) \subseteq approved)
            /\ running' = running \cup {i}
            /\ UNCHANGED <<lo, hi, reserved, asking, approved, settled>>

Settle(i) == /\ i \in running
             /\ running' = running \ {i} /\ settled' = settled \cup {i}
             /\ UNCHANGED <<lo, hi, reserved, asking, approved>>

Advance == /\ hi < N /\ reserved = hi /\ asking = {}
           /\ (DrainBarrier => (lo..hi) \subseteq settled)
           /\ lo' = hi + 1 /\ hi' = hi + WidthAt(hi + 1)
           /\ UNCHANGED <<reserved, asking, approved, running, settled>>

Next == (\E i \in Calls : Ask(i) \/ Approve(i) \/ Start(i) \/ Settle(i)) \/ Advance
Spec == Init /\ [][Next]_vars

NoOverlappingApproval == Cardinality(asking) <= 1
NoUnapprovedExecution == running \subseteq approved
GroupApprovedBeforeExecution == \A i \in running : GroupOf(i) \subseteq approved
WriterBarrier == \A i \in running : \A j \in 1..(GroupStart(1, i) - 1) : j \in settled
====
