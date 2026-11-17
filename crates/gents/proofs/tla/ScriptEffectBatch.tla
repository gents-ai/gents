---- MODULE ScriptEffectBatch ----
\* lean: none
\* crates: gents-loop, gents-toolcalls
\* models: ToolExecution.Parallel.effectWidth
\* Scope: approval/body/barrier interleavings only; not skip, failure, cancel,
\* terminate or result projection (covered by Rust owner tests).
\* Batch width is supplied by running ScriptEffectBatchCases.lean, not
\* recomputed here. Actors ask, approve, execute and settle nondeterministically.
EXTENDS Naturals, Sequences, FiniteSets, ScriptEffectBatchCases
CONSTANTS SerialApproval, AllApproved, DrainBarrier
VARIABLES lo, hi, reserved, asking, approved, running, settled
vars == <<lo, hi, reserved, asking, approved, running, settled>>
Calls == 1..4
Init == /\ lo = 1 /\ hi = WidthAt(1)
        /\ reserved = 0 /\ asking = {} /\ approved = {}
        /\ running = {} /\ settled = {}
Ask(i) == /\ i = reserved + 1 /\ i <= hi /\ i <= 4
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
Advance == /\ hi < 4 /\ reserved = hi /\ asking = {}
           /\ (DrainBarrier => (lo..hi) \subseteq settled)
           /\ lo' = hi + 1 /\ hi' = hi + WidthAt(hi + 1)
           /\ UNCHANGED <<reserved, asking, approved, running, settled>>
Next == (\E i \in Calls : Ask(i) \/ Approve(i) \/ Start(i) \/ Settle(i)) \/ Advance
Spec == Init /\ [][Next]_vars
NoOverlappingApproval == Cardinality(asking) <= 1
NoUnapprovedExecution == running \subseteq approved
GroupApprovedBeforeExecution == \A i \in running :
  IF i \in 1..2 THEN (1..2) \subseteq approved ELSE TRUE
WriterBarrier == (3 \in running => (1..2) \subseteq settled) /\
                 (4 \in running => (1..3) \subseteq settled)
====
