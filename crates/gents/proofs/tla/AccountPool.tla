---- MODULE AccountPool ----
\* lean: AccountRotation.tierOfStatus, AccountRotation.Tier.rank, AccountRotation.cursorAction, AccountRotation.sessionMapAction
\* crates: gents-claude, gents-inference
\* models: AccountRotation.select, AccountRotation.observe, AccountRotation.reject
EXTENDS Naturals, Sequences, FiniteSets, LeanDecision_AccountRotation, LeanDecision_AccountRotation_Tier

\* Concurrent `choose`, `observe` and `reject` calls by two workers on one
\* `AccountPool` with two accounts and two sessions (`SessionCap = 1`).
\* Every pure decision (`tierOfStatus`, `rank`, `cursorAction`,
\* `sessionMapAction`) comes from the generated Lean `AccountRotation` tables.
\*
\* Interleavings modelled:
\* - Two workers may choose the same clear account before the server exhausts
\*   it; the first worker to observe exhaustion marks `poolLimits[a]` rejected
\*   or blocked while the second worker is still in flight (one in-flight race).
\* - An earlier response from before exhaustion may execute `Observe` after a
\*   newer rejection already landed (last-write-wins), temporarily restoring a
\*   stale `"clear"` entry until the next response supersedes it.
\* - Inserting an unrecorded session at `SessionCap` clears `sessions` without
\*   touching `poolLimits`, losing only stickiness.
\*
\* Safeguards, each removed by one constant:
\*   RespectEligibility          `choose` filters accounts via Lean `tierOfStatus`
\*                               and `rank` instead of blindly keeping a sticky key;
\*   RejectBlocks                `reject` sets `blocked = TRUE` on the account;
\*   ClearSessionsOnly           cap clearing clears only `sessions`, never `poolLimits`;
\*   EnforceSessionCap           `choose` applies Lean `sessionMapAction` at cap
\*                               instead of inserting unboundedly;
\*   UseLeanCursorAction         `choose` advances `cursor` only when Lean
\*                               `cursorAction` returns `"advance_past_chosen"`;
\*   ResponseCarriesCurrentState every response carries the server's current state
\*                               so `observe` supersedes any stale LWW write.

CONSTANTS
  RespectEligibility,
  RejectBlocks,
  ClearSessionsOnly,
  EnforceSessionCap,
  UseLeanCursorAction,
  ResponseCarriesCurrentState

Accounts == {"a1", "a2"}
Sessions == {"s1", "s2"}
Workers == {"w1", "w2"}
SessionCap == 1

NextAccount(a) == IF a = "a1" THEN "a2" ELSE "a1"

TierOf(entry) ==
  tierOfStatus(TRUE, entry.blocked, entry.status, "absent", FALSE)

Eligible(entry) == TierOf(entry) # "none"

\* Round-robin cyclic order over `Accounts` starting from `start \in Accounts`.
CyclicOrder(start) ==
  IF start = "a1" THEN <<"a1", "a2">> ELSE <<"a2", "a1">>

BestRank(lim) ==
  LET ranks == { rank(TierOf(lim[a])) : a \in { x \in Accounts : Eligible(lim[x]) } }
  IN CHOOSE r \in ranks : \A q \in ranks : r <= q

SelectAccount(lim, current, cur) ==
  IF \A a \in Accounts : ~Eligible(lim[a]) THEN "none"
  ELSE LET best == BestRank(lim)
           hit(a) == Eligible(lim[a]) /\ rank(TierOf(lim[a])) = best
           order == IF current # "none"
                    THEN CyclicOrder(NextAccount(current))
                    ELSE CyclicOrder(cur)
       IN IF current # "none" /\ hit(current) THEN current
          ELSE IF hit(order[1]) THEN order[1]
          ELSE order[2]

SessionCount(sess) == Cardinality({ s \in Sessions : sess[s] # "none" })

CapStatusOf(len) ==
  IF SessionCap = 0 THEN "zero"
  ELSE IF len < SessionCap THEN "below_cap"
  ELSE "at_or_above_cap"

ApplySessionAction(sess, s, chosen, action) ==
  IF action = "keep" THEN sess
  ELSE IF action = "insert" THEN [sess EXCEPT ![s] = chosen]
  ELSE IF action = "clear_and_insert" THEN
    [x \in Sessions |-> IF x = s THEN chosen ELSE "none"]
  ELSE [x \in Sessions |-> "none"]

FreshLimits ==
  [a \in Accounts |-> [status |-> "clear", blocked |-> FALSE, gen |-> 0]]

VARIABLES
  serverStatus,
  serverGen,
  poolLimits,
  observedExhausted,
  sessions,
  cursor,
  pc,
  reqSession,
  reqAcct,
  respStatus,
  respGen,
  choseExhausted,
  cursorMovedOnSticky

vars == <<serverStatus, serverGen, poolLimits, observedExhausted, sessions, cursor,
          pc, reqSession, reqAcct, respStatus, respGen, choseExhausted, cursorMovedOnSticky>>

Init ==
  /\ serverStatus = [a \in Accounts |-> "clear"]
  /\ serverGen = [a \in Accounts |-> 0]
  /\ poolLimits = FreshLimits
  /\ observedExhausted = [a \in Accounts |-> FALSE]
  /\ sessions = [s \in Sessions |-> "none"]
  /\ cursor = "a1"
  /\ pc = [w \in Workers |-> "idle"]
  /\ reqSession = [w \in Workers |-> "s1"]
  /\ reqAcct = [w \in Workers |-> "none"]
  /\ respStatus = [w \in Workers |-> "clear"]
  /\ respGen = [w \in Workers |-> 0]
  /\ choseExhausted = FALSE
  /\ cursorMovedOnSticky = FALSE

\* Server exhausts account "a1" once (generation 0 -> 1).
ServerExhaust ==
  /\ serverGen["a1"] = 0
  /\ serverStatus' = [serverStatus EXCEPT !["a1"] = "rejected"]
  /\ serverGen' = [serverGen EXCEPT !["a1"] = 1]
  /\ UNCHANGED <<poolLimits, observedExhausted, sessions, cursor,
                 pc, reqSession, reqAcct, respStatus, respGen,
                 choseExhausted, cursorMovedOnSticky>>

\* Worker `w` calls `AccountPool.choose` for session `s`.
Choose(w, s) ==
  /\ pc[w] = "idle"
  /\ LET recorded == sessions[s]
         current == recorded
         chosen == IF ~RespectEligibility /\ current # "none"
                   THEN current
                   ELSE SelectAccount(poolLimits, current, cursor)
     IN /\ chosen # "none"
        /\ LET cAct == cursorAction(current # "none")
               nextCur == IF cAct = "advance_past_chosen"
                          THEN NextAccount(chosen)
                          ELSE IF ~UseLeanCursorAction
                               THEN NextAccount(cursor)
                               ELSE cursor
               switched == current # chosen
               capSt == CapStatusOf(SessionCount(sessions))
               sAct == IF EnforceSessionCap
                       THEN sessionMapAction(switched, recorded # "none", capSt)
                       ELSE IF switched THEN "insert" ELSE "keep"
               nextSess == ApplySessionAction(sessions, s, chosen, sAct)
               clearedLimits == ~ClearSessionsOnly /\ sAct = "clear_and_insert"
               nextLim == IF clearedLimits THEN FreshLimits ELSE poolLimits
           IN /\ poolLimits' = nextLim
              /\ sessions' = nextSess
              /\ cursor' = nextCur
              /\ pc' = [pc EXCEPT ![w] = "in_flight"]
              /\ reqSession' = [reqSession EXCEPT ![w] = s]
              /\ reqAcct' = [reqAcct EXCEPT ![w] = chosen]
              /\ choseExhausted' = (choseExhausted \/ ~Eligible(poolLimits[chosen]) \/ observedExhausted[chosen])
              /\ cursorMovedOnSticky' = (cursorMovedOnSticky \/ (current # "none" /\ ~switched /\ nextCur # cursor))
              /\ UNCHANGED <<serverStatus, serverGen, observedExhausted, respStatus, respGen>>

\* Server answers in-flight worker `w` with the server's current state.
Respond(w) ==
  /\ pc[w] = "in_flight"
  /\ LET a == reqAcct[w]
         st == IF ResponseCarriesCurrentState THEN serverStatus[a] ELSE "clear"
         g == IF ResponseCarriesCurrentState THEN serverGen[a] ELSE 0
     IN /\ pc' = [pc EXCEPT ![w] = "got_response"]
        /\ respStatus' = [respStatus EXCEPT ![w] = st]
        /\ respGen' = [respGen EXCEPT ![w] = g]
        /\ UNCHANGED <<serverStatus, serverGen, poolLimits, observedExhausted, sessions, cursor,
                       reqSession, reqAcct, choseExhausted, cursorMovedOnSticky>>

\* Worker `w` folds its response into `poolLimits[a]` last-write-wins (`AccountPool.observe`).
\* A stale older-generation write overwrites `poolLimits[a]` and clears `observedExhausted[a]`
\* until the next response from `a` supersedes it.
Observe(w) ==
  /\ pc[w] = "got_response"
  /\ LET a == reqAcct[w]
         st == respStatus[w]
         g == respGen[w]
         staleWrite == g < poolLimits[a].gen
         nextBlocked == IF staleWrite THEN FALSE ELSE poolLimits[a].blocked
         nextEntry == [status |-> st, blocked |-> nextBlocked, gen |-> g]
     IN /\ poolLimits' = [poolLimits EXCEPT ![a] = nextEntry]
        /\ observedExhausted' = [observedExhausted EXCEPT ![a] = ~Eligible(nextEntry)]
        /\ pc' = [pc EXCEPT ![w] = "idle"]
        /\ reqAcct' = [reqAcct EXCEPT ![w] = "none"]
        /\ UNCHANGED <<serverStatus, serverGen, sessions, cursor,
                       reqSession, respStatus, respGen, choseExhausted, cursorMovedOnSticky>>

\* Worker `w` records a provider rejection (`AccountPool.reject`).
Reject(w) ==
  /\ pc[w] = "got_response"
  /\ respStatus[w] = "rejected"
  /\ LET a == reqAcct[w]
         nextEntry == [poolLimits[a] EXCEPT !.blocked = IF RejectBlocks THEN TRUE ELSE poolLimits[a].blocked,
                                            !.gen = respGen[w]]
     IN /\ poolLimits' = [poolLimits EXCEPT ![a] = nextEntry]
        /\ observedExhausted' = [observedExhausted EXCEPT ![a] = TRUE]
        /\ pc' = [pc EXCEPT ![w] = "idle"]
        /\ reqAcct' = [reqAcct EXCEPT ![w] = "none"]
        /\ UNCHANGED <<serverStatus, serverGen, sessions, cursor,
                       reqSession, respStatus, respGen, choseExhausted, cursorMovedOnSticky>>

Next ==
  \/ ServerExhaust
  \/ \E w \in Workers, s \in Sessions : Choose(w, s)
  \/ \E w \in Workers : Respond(w) \/ Observe(w) \/ Reject(w)

Fairness ==
  /\ \A w \in Workers :
       /\ WF_vars(Respond(w))
       /\ WF_vars(Observe(w))
       /\ \A s \in Sessions : SF_vars(Choose(w, s))

Spec == Init /\ [][Next]_vars /\ Fairness

\* Safety: no `choose` picks an account whose latest observation shows it exhausted.
NoChooseOfExhausted == ~choseExhausted

\* Safety: while `poolLimits[a]` shows `a` exhausted, at most one concurrent
\* worker (the in-flight race chosen before the observation) is in flight to `a`.
AtMostOneInFlightOnExhausted ==
  \A a \in Accounts :
    ~Eligible(poolLimits[a]) =>
      Cardinality({ w \in Workers : pc[w] = "in_flight" /\ reqAcct[w] = a }) <= 1

\* Safety: session map never exceeds `SessionCap`.
SessionCapBounded == SessionCount(sessions) <= SessionCap

\* Safety: a sticky hit never advances the round-robin cursor.
StickyKeepsCursor == ~cursorMovedOnSticky

\* Liveness: a stale last-write-wins overwrite that leaves an exhausted server
\* account looking eligible in `poolLimits` is superseded by the next response.
StaleEntry(a) ==
  serverStatus[a] = "rejected" /\ Eligible(poolLimits[a]) /\ poolLimits[a].gen < serverGen[a]

StaleUpdateSuperseded ==
  \A a \in Accounts : StaleEntry(a) ~> ~StaleEntry(a)
====
