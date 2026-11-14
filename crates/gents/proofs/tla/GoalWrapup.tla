---- MODULE GoalWrapup ----
(***************************************************************************)
(* The one wrap-up turn of a Goal whose token budget is exhausted (#3810). *)
(*                                                                          *)
(* A request of the Goal's session completed over budget. The Goal source  *)
(* (`GoalSource::build_intent`, triggers/engine/goal_source.rs) reconciles *)
(* the Goal from what it reads: the canonical Goal row, the session's       *)
(* latest goal request (`latest_goal_request`, which skips a parent that   *)
(* already has a goal child) and whether that request has a goal child.    *)
(* It decides with Lean `Goals.decide` (Rust `decide_goal_continuation`)   *)
(* and then writes, each write its own DefraDB transaction:                *)
(*   Begin   `begin_wrapup`: status active -> budget_limited and the       *)
(*           `wrapup_requested` latch, CAS on status and sequence           *)
(*           (intent_phases.rs `begin_wrapup`, controller.rs:8).            *)
(*   Claim   `claim_continuation_from_access`: last_continued_from := the  *)
(*           parent and sequence + 1, CAS on status and sequence            *)
(*           (controller.rs:91).                                            *)
(*   Publish `publish_claimed_continuation`: in one transaction, recheck   *)
(*           the claim and create the child keyed by                        *)
(*           `goal_continuation_identity(goal, parent, sequence)`, or       *)
(*           recover the child that key already names                       *)
(*           (claimed_publication.rs:167-285).                              *)
(*   Latch   a completed wrap-up child sets `wrapup_completed`, CAS on     *)
(*           status budget_limited and sequence (goal_source.rs:461-481).   *)
(* A source sees the Goal as already claimed (last_continued_from is the   *)
(* latest request, no child) and goes straight to Publish                   *)
(* (goal_source.rs:398-449). The source runs only while `next_fire` is     *)
(* polled; a caller that stops polling drops the in-flight step between    *)
(* transactions (`Drop`), and the next poll starts over from the rows.     *)
(*                                                                          *)
(* The test (`token_budget_materializes_one_wrapup_and_never_repeats_it`)  *)
(* completes the wrap-up child and then reads the Goal. On main it polled  *)
(* `next_fire` for a 200 ms window first, so the read races the source's   *)
(* step (`AwaitReconcile = FALSE`); the fix awaits one whole step of the   *)
(* source before reading.                                                   *)
(*                                                                          *)
(* Premise, not checked here: each write is one atomic transaction, so a   *)
(* concurrent Publish of the same key either recovers the committed child  *)
(* or aborts. The operating convention runs one Goal source per node; the  *)
(* model allows several to show the CAS and the keyed identity suffice.    *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
  Sources,           \* Goal source instances reconciling the same Goal
  MaxReq,            \* request ids 0..MaxReq; 0 is the over-budget parent
  WrapupGuard,       \* `decide` consults whether the latest request is the wrap-up
  KeyedChild,        \* Publish keys the child by (goal, parent, sequence)
  AwaitReconcile     \* the test awaits a whole source step before reading

Ids == 0..MaxReq
NoReq == MaxReq + 1

VARIABLES
  goal,      \* [status, requested, completed, lastFrom, seq]
  req,       \* [Ids -> [exists, done, wrapup, parent]]
  pc,        \* source: idle | decided | begun | claimed
  snap,      \* source: what its step read
  test,      \* test: running | waiting | read
  latchSeen, \* ghost: the latch the test read
  stepped    \* ghost: a source step that saw the completed wrap-up finished

vars == <<goal, req, pc, snap, test, latchSeen, stepped>>

NoSnap == [l |-> NoReq, seq |-> 0, status |-> "none", wrapup |-> FALSE,
           afterChild |-> FALSE]

Init ==
  /\ goal = [status |-> "active", requested |-> FALSE, completed |-> FALSE,
             lastFrom |-> NoReq, seq |-> 0]
  /\ req = [i \in Ids |->
             IF i = 0
               THEN [exists |-> TRUE, done |-> TRUE, wrapup |-> FALSE, parent |-> NoReq]
               ELSE [exists |-> FALSE, done |-> FALSE, wrapup |-> FALSE, parent |-> NoReq]]
  /\ pc = [s \in Sources |-> "idle"]
  /\ snap = [s \in Sources |-> NoSnap]
  /\ test = "running"
  /\ latchSeen = FALSE
  /\ stepped = FALSE

Existing == {i \in Ids : req[i].exists}
HasChild(i) == \E j \in Existing : req[j].parent = i
\* `latest_goal_request`: the newest request that no goal child continues.
Heads == {i \in Existing : ~HasChild(i)}
Latest == CHOOSE i \in Heads : \A j \in Heads : j <= i
SessionIdle == \A i \in Existing : req[i].done
WrapupChildOf(i) == {j \in Existing : req[j].parent = i /\ req[j].wrapup}
CompletedWrapup == \E i \in Existing : req[i].wrapup /\ req[i].done

(* Lean `Goals.decide` for a completed, over-budget latest request with     *)
(* activity, an idle session and no child; `WrapupGuard = FALSE` drops its *)
(* `requestIsWrapup` input.                                                  *)
Decide(status, requested, completed, isWrapup) ==
  CASE status = "active" -> "wrapup"
    [] status = "budget_limited" ->
         IF ~requested \/ completed THEN "none"
         ELSE IF ~(WrapupGuard /\ isWrapup) THEN "wrapup"
         ELSE "none"
    [] OTHER -> "none"

FreshId == CHOOSE i \in Ids : ~req[i].exists /\ \A j \in Ids : ~req[j].exists => i <= j

(* The source. *)

\* goal_source.rs:294-375: read the Goal, the latest request and its child.
Observe(s) ==
  /\ pc[s] = "idle" /\ SessionIdle
  /\ LET l == Latest
         isW == req[l].wrapup
         s1 == [l |-> l, seq |-> goal.seq, status |-> goal.status, wrapup |-> isW,
                afterChild |-> isW /\ req[l].done]
     IN \/ /\ goal.lastFrom = l
           \* goal_source.rs:398-449: claimed, no child yet -> Publish.
           /\ pc' = [pc EXCEPT ![s] = "claimed"]
           /\ snap' = [snap EXCEPT ![s] = s1]
           /\ UNCHANGED <<goal, stepped>>
        \/ /\ goal.lastFrom # l
           /\ IF goal.status = "budget_limited" /\ ~goal.completed /\ isW /\ WrapupGuard
                \* goal_source.rs:461-481: the completed wrap-up sets the latch.
                THEN /\ goal' = IF goal.status = "budget_limited" /\ goal.seq = s1.seq
                                  THEN [goal EXCEPT !.completed = TRUE] ELSE goal
                     /\ pc' = pc /\ snap' = snap
                     /\ stepped' = (stepped \/ s1.afterChild)
                ELSE IF Decide(goal.status, goal.requested, goal.completed, isW) = "wrapup"
                  THEN /\ pc' = [pc EXCEPT ![s] = "decided"]
                       /\ snap' = [snap EXCEPT ![s] = s1]
                       /\ UNCHANGED <<goal, stepped>>
                  ELSE /\ stepped' = (stepped \/ s1.afterChild)
                       /\ UNCHANGED <<goal, pc, snap>>
  /\ UNCHANGED <<req, test, latchSeen>>

\* intent_phases.rs `begin_wrapup`.
Begin(s) ==
  /\ pc[s] = "decided"
  /\ IF snap[s].status = "active"
       THEN IF goal.status = "active" /\ goal.seq = snap[s].seq
              THEN /\ goal' = [goal EXCEPT !.status = "budget_limited", !.requested = TRUE]
                   /\ pc' = [pc EXCEPT ![s] = "begun"]
                   /\ snap' = [snap EXCEPT ![s].status = "budget_limited"]
              ELSE /\ pc' = [pc EXCEPT ![s] = "idle"] /\ UNCHANGED <<goal, snap>>
       ELSE /\ pc' = [pc EXCEPT ![s] = "begun"] /\ UNCHANGED <<goal, snap>>
  /\ UNCHANGED <<req, test, latchSeen, stepped>>

\* controller.rs:91 `claim_continuation_from_access`.
Claim(s) ==
  /\ pc[s] = "begun"
  /\ IF goal.status = snap[s].status /\ goal.seq = snap[s].seq
       THEN /\ goal' = [goal EXCEPT !.lastFrom = snap[s].l, !.seq = @ + 1]
            /\ pc' = [pc EXCEPT ![s] = "claimed"]
       ELSE /\ pc' = [pc EXCEPT ![s] = "idle"] /\ UNCHANGED goal
  /\ UNCHANGED <<req, snap, test, latchSeen, stepped>>

\* claimed_publication.rs:167-285: recheck the claim; create or recover the
\* child the key names. `KeyedChild = FALSE` creates a fresh request each time.
Publish(s) ==
  /\ pc[s] = "claimed"
  /\ pc' = [pc EXCEPT ![s] = "idle"]
  /\ LET l == snap[s].l IN
     IF goal.lastFrom = l
        /\ (~KeyedChild \/ ~HasChild(l))
        /\ \E i \in Ids : ~req[i].exists
       THEN req' = [req EXCEPT ![FreshId] =
                     [exists |-> TRUE, done |-> FALSE,
                      wrapup |-> goal.status = "budget_limited", parent |-> l]]
       ELSE UNCHANGED req
  /\ UNCHANGED <<goal, snap, test, latchSeen, stepped>>

\* The caller stops polling `next_fire`: the step is dropped between writes.
Drop(s) ==
  /\ pc[s] # "idle"
  /\ pc' = [pc EXCEPT ![s] = "idle"]
  /\ UNCHANGED <<goal, req, snap, test, latchSeen, stepped>>

(* The wrap-up turn runs and completes. *)
Complete(i) ==
  /\ req[i].exists /\ ~req[i].done
  /\ req' = [req EXCEPT ![i].done = TRUE]
  /\ UNCHANGED <<goal, pc, snap, test, latchSeen, stepped>>

(* The test: once the wrap-up completed it reads the Goal. On main it reads *)
(* when its window ends, whatever the source has done; the fix reads after   *)
(* a source step that saw the completed wrap-up.                             *)
TestRead ==
  /\ test = "running" /\ CompletedWrapup
  /\ AwaitReconcile => stepped
  /\ test' = "read" /\ latchSeen' = goal.completed
  /\ UNCHANGED <<goal, req, pc, snap, stepped>>

Next ==
  \/ TestRead
  \/ \E i \in Ids : Complete(i)
  \/ \E s \in Sources : Observe(s) \/ Begin(s) \/ Claim(s) \/ Publish(s) \/ Drop(s)

Fairness ==
  /\ \A s \in Sources : WF_vars(Observe(s)) /\ WF_vars(Begin(s))
                        /\ WF_vars(Claim(s)) /\ WF_vars(Publish(s))
  /\ \A i \in Ids : WF_vars(Complete(i))

Spec == Init /\ [][Next]_vars /\ Fairness

(* Properties. *)

TypeOK ==
  /\ goal.status \in {"active", "budget_limited"}
  /\ pc \in [Sources -> {"idle", "decided", "begun", "claimed"}]
  /\ test \in {"running", "read"}

\* One exhaustion materialises one wrap-up: no request has two wrap-up children.
OneWrapupPerParent == \A i \in Ids : Cardinality(WrapupChildOf(i)) <= 1

\* A completed wrap-up is never followed by another wrap-up.
NeverAfterCompletedWrapup ==
  \A i \in Existing : (req[i].wrapup /\ req[i].done) => ~HasChild(i)

\* At most one wrap-up request exists in the whole run (the parent's).
OneWrapup == Cardinality({i \in Existing : req[i].wrapup}) <= 1

\* What the test asserted at pushdown_runtime_tests.rs:3251.
ReadSeesLatch == test = "read" => latchSeen

\* A polled source records the completed wrap-up.
LatchRecorded == CompletedWrapup ~> goal.completed

====
