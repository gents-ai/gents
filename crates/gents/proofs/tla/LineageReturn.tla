---- MODULE LineageReturn ----
\* lean: LineageReturn.quiet
\* crates: gents-toolcalls, gents-session
\* models: LineageReturn.decide
EXTENDS LeanDecision_LineageReturn

\* A caller's background agent_new call caused one request in a delegate's
\* session. That turn may start a helper in the background with its own call
\* and end before the helper answers (the caller finishing before the nested
\* result arrives). The helper's result then commits a wake in the delegate's
\* session, and the wake's turn holds the answer the caller asked for. The
\* settler of the caller's call reads the lineage one document at a time while
\* everything else runs on: the delegate's turn ends, the helper finishes, the
\* helper's call settles, its completion commits the wake, a later transaction
\* marks the call delivered, and the wake runs. A settler that finds the
\* lineage not quiet tries again on the next sweep.
\*
\* Safeguards, each removed by one constant:
\*   CausedOnly          the settler reads only the caused request;
\*   CallsFirst          the settler reads a request's calls before its terminal;
\*   SettledIsDelivered  a call stops owing its result when it settles, before
\*                       the transaction that commits its wake.

CONSTANTS CausedOnly, CallsFirst, SettledIsDelivered

VARIABLES caused, delegated, helper, call, wake,
          pc, seenTerminal, seenCall, seenOwed, seenWake, answer
vars == <<caused, delegated, helper, call, wake,
          pc, seenTerminal, seenCall, seenOwed, seenWake, answer>>

Init ==
  /\ caused = "running"
  /\ delegated = FALSE
  /\ helper = "none"
  /\ call = "none"
  /\ wake = "none"
  /\ pc = "idle"
  /\ seenTerminal = FALSE
  /\ seenCall = FALSE
  /\ seenOwed = FALSE
  /\ seenWake = "none"
  /\ answer = "none"

World == <<caused, delegated, helper, call, wake>>
Settler == <<pc, seenTerminal, seenCall, seenOwed, seenWake, answer>>

Delegate ==
  /\ caused = "running" /\ ~delegated
  /\ delegated' = TRUE /\ helper' = "running" /\ call' = "running"
  /\ UNCHANGED <<caused, wake>> /\ UNCHANGED Settler

EndTurn ==
  /\ caused = "running"
  /\ caused' = "terminal"
  /\ UNCHANGED <<delegated, helper, call, wake>> /\ UNCHANGED Settler

HelperAnswers ==
  /\ helper = "running"
  /\ helper' = "done"
  /\ UNCHANGED <<caused, delegated, call, wake>> /\ UNCHANGED Settler

SettleCall ==
  /\ call = "running" /\ helper = "done"
  /\ call' = "settled"
  /\ UNCHANGED <<caused, delegated, helper, wake>> /\ UNCHANGED Settler

CommitWake ==
  /\ call = "settled" /\ wake = "none"
  /\ wake' = "pending"
  /\ UNCHANGED <<caused, delegated, helper, call>> /\ UNCHANGED Settler

MarkDelivered ==
  /\ call = "settled" /\ wake # "none"
  /\ call' = "delivered"
  /\ UNCHANGED <<caused, delegated, helper, wake>> /\ UNCHANGED Settler

RunWake ==
  /\ wake = "pending" /\ caused = "terminal"
  /\ wake' = "terminal"
  /\ UNCHANGED <<caused, delegated, helper, call>> /\ UNCHANGED Settler

Owed == IF SettledIsDelivered THEN call = "running"
        ELSE call \in {"running", "settled"}

Start ==
  /\ pc = "idle"
  /\ pc' = IF CallsFirst THEN "calls" ELSE "terminal"
  /\ UNCHANGED <<seenTerminal, seenCall, seenOwed, seenWake, answer>> /\ UNCHANGED World

ReadTerminal ==
  /\ pc = "terminal"
  /\ seenTerminal' = (caused = "terminal")
  /\ pc' = IF CausedOnly THEN "decide" ELSE IF CallsFirst THEN "wake" ELSE "calls"
  /\ UNCHANGED <<seenCall, seenOwed, seenWake, answer>> /\ UNCHANGED World

ReadCalls ==
  /\ pc = "calls"
  /\ seenCall' = delegated
  /\ seenOwed' = (delegated /\ Owed)
  /\ pc' = IF CallsFirst THEN "terminal" ELSE "wake"
  /\ UNCHANGED <<seenTerminal, seenWake, answer>> /\ UNCHANGED World

ReadWake ==
  /\ pc = "wake"
  /\ seenWake' = IF seenCall /\ ~seenOwed THEN wake ELSE "none"
  /\ pc' = "decide"
  /\ UNCHANGED <<seenTerminal, seenCall, seenOwed, answer>> /\ UNCHANGED World

CausedQuiet == quiet(seenTerminal, seenOwed /\ ~CausedOnly)
WakeQuiet == seenWake = "none" \/ quiet(seenWake = "terminal", FALSE)

Decide ==
  /\ pc = "decide"
  /\ IF CausedQuiet /\ WakeQuiet
       THEN /\ answer' = IF seenWake = "none" THEN "caused" ELSE "wake"
            /\ pc' = "done"
            /\ UNCHANGED <<seenTerminal, seenCall, seenOwed, seenWake>>
       ELSE /\ pc' = "idle"
            /\ seenTerminal' = FALSE /\ seenCall' = FALSE
            /\ seenOwed' = FALSE /\ seenWake' = "none"
            /\ UNCHANGED answer
  /\ UNCHANGED World

Next == Delegate \/ EndTurn \/ HelperAnswers \/ SettleCall \/ CommitWake
          \/ MarkDelivered \/ RunWake
          \/ Start \/ ReadTerminal \/ ReadCalls \/ ReadWake \/ Decide

Spec == Init /\ [][Next]_vars
          /\ WF_vars(EndTurn) /\ WF_vars(HelperAnswers) /\ WF_vars(SettleCall)
          /\ WF_vars(CommitWake) /\ WF_vars(MarkDelivered) /\ WF_vars(RunWake)
          /\ WF_vars(Start) /\ WF_vars(ReadTerminal) /\ WF_vars(ReadCalls)
          /\ WF_vars(ReadWake) /\ WF_vars(Decide)

\* The caller is answered by the delegate's caused turn only when that turn
\* started no helper; otherwise by the wake that carried the helper's result,
\* once it has run.
NestedResultReturned ==
  /\ (answer = "caused" => ~delegated)
  /\ (answer = "wake" => wake = "terminal")
Answered == <> (answer # "none")
====
