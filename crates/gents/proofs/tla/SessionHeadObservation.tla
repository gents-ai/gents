---- MODULE SessionHeadObservation ----
\* A session's observed request head and the client that projects it on
\* reopen (#3604).
\*
\* Writers. The runtime claims a session's normal requests one at a time.
\* Each claim (`materialize::claim`, `advance_session_request_observation_in_txn`)
\* and each admission refusal (`advance_refused_request_observation_in_txn`)
\* reads the session's request facts and its `AgentSession` document in one
\* transaction and names the incoming request as
\* `observation.latest_request` only when it is the canonical latest
\* (`incoming_is_canonical_latest`, Lean `AgentSession.advance`). A completion
\* refreshes the head only when the head already names it
\* (`refresh_session_request_observation_in_txn`, Lean `AgentSession.refresh`).
\* DefraDB is optimistic: a commit aborts when a document it read changed after
\* its snapshot, and the transaction re-reads. Retried transactions therefore
\* commit in some serial order, so each writer is one atomic step here.
\*
\* `created_at` is signed by the requester at whole-second precision and
\* `request_id` is random, so A and B admitted in the same second have no
\* admission order of their own. `Order` selects the canonical tie rule:
\*   "lean"          created second, live over terminal, later terminalization,
\*                   request_id (Lean `AgentSession.newer`, the fix);
\*   "requestId"     created second, request_id (the code before the fix);
\*   "noLive"        created second, terminalization (live rows count as 0),
\*                   request_id;
\*   "noTerminal"    created second, live over terminal, request_id.
\*
\* Reader. The reopened client reads the runtime as its User over HTTP
\* (`load_node_snapshot_on`): one query returns the nonterminal requests and
\* each session's observed head, a later query reads each head row by document
\* ID (`session_head_queries`); each query is its own snapshot, so a writer may
\* commit between them. Terminal requests other than the head are not read.
\* Every runtime commit while the client is open is a subscription event that
\* queues a load; a load that overlaps an event is discarded by the store
\* fence and re-run (Lean `ClientDataPublication.uncached_request_event_rejects_old_read`).
\* `HeadSeed` FALSE drops the head read, leaving only nonterminal rows.
\*
\* The reopen test waits for the client projection to show B's terminal state.
\* `ReplyObserved` is that wait; `HeadFollowsBoundary` is the writer invariant
\* behind it: once B is claimed, or refused while no other request is live,
\* the observation names B. A refusal admitted in the second of a still-live
\* turn deliberately leaves the live turn as the head (the order errs toward a
\* busy session), so neither property claims it (`refusedWhileLive`).
EXTENDS Naturals

CONSTANTS
  Order,    \* "lean" | "requestId" | "noLive" | "noTerminal"
  HeadSeed, \* the reader reads the observed head row
  Refusals  \* B may be refused at admission instead of queued

ASSUME /\ Order \in {"lean", "requestId", "noLive", "noTerminal"}
       /\ HeadSeed \in BOOLEAN
       /\ Refusals \in BOOLEAN

Requests == {"A", "B"}
Live == {"pending", "claimed"}
Terminal == {"completed", "refused"}

VARIABLES
  state,     \* request -> "none" | "pending" | "claimed" | "completed" | "refused"
  second,    \* request -> created second
  term,      \* request -> runtime terminalization time (0 while live)
  clock,     \* runtime clock for terminalization times
  aHigher,   \* A's random request_id sorts above B's
  head,      \* request named by observation.latest_request ("none" before any)
  online,    \* the client is open
  dirty,     \* a subscription event is waiting for a load
  refusedWhileLive, \* B was refused while A was live
  phase,     \* reader: "idle" | "mid"
  q1State,   \* request -> state seen by the first query ("absent" if terminal)
  q1Head,    \* head seen by the first query
  proj       \* request -> state in the client projection ("absent" if not shown)

vars == <<state, second, term, clock, aHigher, head, refusedWhileLive, online,
          dirty, phase, q1State, q1Head, proj>>
writerVars == <<state, second, term, clock, aHigher, head, refusedWhileLive>>
readerVars == <<online, dirty, phase, q1State, q1Head, proj>>

Init ==
  /\ state = [r \in Requests |-> IF r = "A" THEN "pending" ELSE "none"]
  /\ second = [r \in Requests |-> 0]
  /\ term = [r \in Requests |-> 0]
  /\ clock = 0
  /\ aHigher \in BOOLEAN
  /\ head = "none"
  /\ refusedWhileLive = FALSE
  /\ online = FALSE
  /\ dirty = FALSE
  /\ phase = "idle"
  /\ q1State = [r \in Requests |-> "absent"]
  /\ q1Head = "none"
  /\ proj = [r \in Requests |-> "absent"]

IdHigher(x) == IF x = "A" THEN aHigher ELSE ~aHigher
LiveRank(x, s) == IF s[x] \in Live THEN 1 ELSE 0

\* x is newer than y under `Order`, reading the transaction's own view of
\* request states `s`, terminalization times `t` and created seconds `c`.
Newer(x, y, s, t, c) ==
  LET tie == c[x] = c[y]
      byId == IdHigher(x)
      live == LiveRank(x, s) > LiveRank(y, s)
      sameLive == LiveRank(x, s) = LiveRank(y, s)
      later == t[x] > t[y]
      sameTerm == t[x] = t[y]
  IN \/ c[x] > c[y]
     \/ tie /\ CASE Order = "lean" -> live \/ (sameLive /\ (later \/ (sameTerm /\ byId)))
                 [] Order = "requestId" -> byId
                 [] Order = "noLive" -> later \/ (sameTerm /\ byId)
                 [] Order = "noTerminal" -> live \/ (sameLive /\ byId)

\* Lean `AgentSession.advance`: name r when no admitted request is newer.
Advance(r, s, t, c) ==
  IF \A y \in Requests \ {r} : s[y] = "none" \/ ~Newer(y, r, s, t, c)
    THEN r ELSE head

Event == dirty' = (dirty \/ online)

ClaimA ==
  /\ state["A"] = "pending"
  /\ LET s == [state EXCEPT !["A"] = "claimed"]
     IN /\ state' = s
        /\ head' = Advance("A", s, term, second)
  /\ Event
  /\ UNCHANGED <<second, term, clock, aHigher, refusedWhileLive, online, phase,
                 q1State, q1Head, proj>>

Complete(r) ==
  /\ state[r] = "claimed"
  /\ state' = [state EXCEPT ![r] = "completed"]
  /\ clock' = clock + 1
  /\ term' = [term EXCEPT ![r] = clock + 1]
  /\ Event
  /\ UNCHANGED <<second, aHigher, head, refusedWhileLive, online, phase, q1State,
                 q1Head, proj>>

\* B is admitted once A has been claimed, in A's second or a later one; it is
\* either queued or refused at admission.
AdmitB ==
  /\ state["B"] = "none"
  /\ state["A"] # "pending"
  /\ \E sec \in {0, 1}, refused \in (IF Refusals THEN BOOLEAN ELSE {FALSE}) :
       /\ second' = [second EXCEPT !["B"] = sec]
       /\ IF refused
            THEN LET s == [state EXCEPT !["B"] = "refused"]
                     t == [term EXCEPT !["B"] = clock + 1]
                     c == [second EXCEPT !["B"] = sec]
                 IN /\ state' = s
                    /\ term' = t
                    /\ clock' = clock + 1
                    /\ head' = Advance("B", s, t, c)
                    /\ refusedWhileLive' = (state["A"] \in Live)
            ELSE /\ state' = [state EXCEPT !["B"] = "pending"]
                 /\ UNCHANGED <<term, clock, head, refusedWhileLive>>
  /\ Event
  /\ UNCHANGED <<aHigher, online, phase, q1State, q1Head, proj>>

ClaimB ==
  /\ state["B"] = "pending"
  /\ state["A"] \in Terminal
  /\ LET s == [state EXCEPT !["B"] = "claimed"]
     IN /\ state' = s
        /\ head' = Advance("B", s, term, second)
  /\ Event
  /\ UNCHANGED <<second, term, clock, aHigher, refusedWhileLive, online, phase,
                 q1State, q1Head, proj>>

Reopen ==
  /\ ~online
  /\ online' = TRUE
  /\ dirty' = TRUE
  /\ UNCHANGED <<writerVars, phase, q1State, q1Head, proj>>

\* First query: nonterminal rows and the observed head.
LoadBegin ==
  /\ online /\ dirty /\ phase = "idle"
  /\ q1State' = [r \in Requests |-> IF state[r] \in Live THEN state[r] ELSE "absent"]
  /\ q1Head' = head
  /\ dirty' = FALSE
  /\ phase' = "mid"
  /\ UNCHANGED <<writerVars, online, proj>>

\* Second query reads the head row; the result is published unless an event
\* arrived during the load, which leaves `dirty` set for the re-run.
LoadEnd ==
  /\ phase = "mid"
  /\ LET result == [r \in Requests |->
           IF q1State[r] # "absent" THEN q1State[r]
           ELSE IF HeadSeed /\ r = q1Head /\ state[r] # "none" THEN state[r]
           ELSE "absent"]
     IN proj' = IF dirty THEN proj ELSE result
  /\ phase' = "idle"
  /\ UNCHANGED <<writerVars, online, dirty, q1State, q1Head>>

Next ==
  \/ ClaimA
  \/ Complete("A")
  \/ AdmitB
  \/ ClaimB
  \/ Complete("B")
  \/ Reopen
  \/ LoadBegin
  \/ LoadEnd

Spec ==
  /\ Init
  /\ [][Next]_vars
  /\ WF_vars(ClaimA)
  /\ WF_vars(Complete("A"))
  /\ WF_vars(AdmitB)
  /\ WF_vars(ClaimB)
  /\ WF_vars(Complete("B"))
  /\ WF_vars(Reopen)
  /\ WF_vars(LoadBegin)
  /\ WF_vars(LoadEnd)

TypeOK ==
  /\ state \in [Requests -> {"none", "pending", "claimed", "completed", "refused"}]
  /\ second \in [Requests -> {0, 1}]
  /\ term \in [Requests -> 0..2]
  /\ head \in Requests \cup {"none"}
  /\ phase \in {"idle", "mid"}
  /\ proj \in [Requests -> {"absent", "pending", "claimed", "completed", "refused"}]

Claimed(r) == state[r] \in {"claimed", "completed"}
BoundaryNamesB == Claimed("B") \/ (state["B"] = "refused" /\ ~refusedWhileLive)

\* Once B is claimed, or refused while no other request is live, the
\* observation names B.
HeadFollowsBoundary == BoundaryNamesB => head = "B"

\* The reopened client eventually shows B's terminal state.
ReplyObserved ==
  \A out \in Terminal : (state["B"] = out /\ BoundaryNamesB) ~> (proj["B"] = out)

====
