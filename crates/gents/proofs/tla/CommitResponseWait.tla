---- MODULE CommitResponseWait ----
\* lean: none
\* crates: gents-store, gents-protocol, gents-pack
\* models: TransactionReplay.clientMayRepeat
(***************************************************************************)
(* A client's wait for a store request's answer over the runtime's HTTP    *)
(* endpoint, on a loaded host (#3965). The case that matters is a          *)
(* transaction's commit (`graphql::txn_commit`, reached from               *)
(* `transact_write_with_finality`, e.g. `pack.document_graph.install`);    *)
(* an auto-commit mutation is the same with no open state. A read is the   *)
(* same wait with nothing durable at stake: `Decide` is its answer and    *)
(* `Agreement` holds trivially, but `UnknownOnlyWithoutProgress` still    *)
(* says whether a live server's answer is thrown away.                     *)
(*                                                                          *)
(* The transaction is open on the server; the client posts `/tx/{id}`.     *)
(* The request `Arrive`s while the connection is up, the server works      *)
(* through the commit (`Work`, as many steps as the host's load makes it,  *)
(* each scheduled within `MaxDelay` ticks), then commits or aborts and     *)
(* answers. DefraDB queues the commit to the transaction's owner worker    *)
(* and takes the handle out of its table first (`OwnerTable::finish`), so  *)
(* a commit that has arrived runs to its end whether or not anyone still  *)
(* waits for the answer, and afterwards the ID names nothing.              *)
(*                                                                          *)
(* Ways a wait can end without the answer:                                 *)
(*  - `Crash`: the server dies. Its host's kernel resets the connection    *)
(*    (`Reset`), unless the host itself is gone (`HostLoss`): then the     *)
(*    connection is half-open and only the client's TCP keepalive probes   *)
(*    (`Keepalive`), answered by a live host's kernel whatever its load,  *)
(*    discover it.                                                          *)
(*  - `Wedge` (allowed when not `ServerBounded`): the server lives but     *)
(*    stops working on the commit. Keepalive probes are still answered.   *)
(*  - `Drop` (allowed when `Drops`): the connection breaks while the       *)
(*    server lives, before or after the commit arrived.                    *)
(*  - The client's detector (`Detector`) gives up:                         *)
(*      "none"     never; the answer, a reset or keepalive ends the wait.  *)
(*      "clock"    after `Ceiling` ticks without the answer: the shared    *)
(*                 client's 30 s total timeout, or equally a read-idle     *)
(*                 timeout, since a commit sends nothing until it answers. *)
(*      "progress" after `Ceiling` ticks without a commit step, which the  *)
(*                 server would have to publish. `MaxDelay < Ceiling` is   *)
(*                 the premise that a live, working host schedules a step  *)
(*                 before the ceiling (`WallClockBound`, `STALL_CEILING`). *)
(*                                                                          *)
(* Once the answer is lost (`Drop`), the client either reports the outcome *)
(* unknown or, with `OutcomeQuery`, asks the server for the transaction's  *)
(* outcome: still open (the commit never arrived; the client discards it   *)
(* and the outcome is a confirmed abort), still committing (ask again),    *)
(* committed, or aborted. DefraDB has no such query yet.                   *)
(*                                                                          *)
(* An unknown outcome reaches the caller as "transaction outcome is        *)
(* unconfirmed" and is never repeated (`TransactionReplay.clientMayRepeat`,*)
(* `unconfirmed_commit_conflict_is_never_repeated`).                       *)
(*                                                                          *)
(* `Agreement`: a confirmed outcome is the server's outcome.                *)
(* `UnknownOnlyWithoutProgress`: the client reports an unknown outcome     *)
(* only for a server that is dead or no longer working on the commit,      *)
(* never for one that is still making progress or has decided.             *)
(* `ClientSettles`: the client reaches a verdict.                          *)
(*                                                                          *)
(* GREEN `MCCommitResponseWait` (the delivered client): no detector,       *)
(* keepalive, a bounded server that may crash or vanish. REDs, one         *)
(* safeguard removed each: `ClientWindow` (the "clock" detector, the CI    *)
(* failure) violates `UnknownOnlyWithoutProgress`; `NoKeepalive` with a   *)
(* vanished host violates `ClientSettles`; `Wedged` (the server premise    *)
(* dropped, no progress signal) violates `ClientSettles`; `DroppedAnswer`  *)
(* (a broken connection with no outcome query) violates                    *)
(* `UnknownOnlyWithoutProgress`. Upstream designs, green only once         *)
(* DefraDB offers them: `WedgedProgress` (a published commit step lets the *)
(* client give up on a wedged server and only on it) and                   *)
(* `DroppedAnswerQueried` (the outcome query resolves a lost answer).      *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  MaxWork,        \* bound on the model's work steps (any load fits under it)
  MaxDelay,       \* ticks a live, working host takes to schedule a step
  Ceiling,        \* ticks a detector waits before it gives up
  Detector,       \* "none" | "clock" | "progress"
  Crashes,        \* the server may die mid-commit
  HostLoss,       \* a death may take the host, leaving the connection half-open
  Keepalive,      \* the client probes an idle connection's peer
  ServerBounded,  \* the server answers every commit it works on
  Drops,          \* the connection may break while the server lives
  OutcomeQuery    \* the server answers a query for a transaction's outcome

ASSUME MaxDelay < Ceiling

VARIABLES
  s,       \* server: open | working | committed | aborted | wedged | dead
  work,    \* commit steps done
  sent,    \* the server's answer is on the wire
  conn,    \* connection: up | broken (the client sees an error) | halfopen
  elapsed, \* ticks since the post, capped at Ceiling
  idle,    \* ticks since the last commit step, capped at Ceiling
  fate,    \* the durable outcome: none | committed | aborted
  c        \* client: waiting | committed | aborted | unknown

vars == <<s, work, sent, conn, elapsed, idle, fate, c>>

Live == s \in {"open", "working", "committed", "aborted", "wedged"}
Decided == s \in {"committed", "aborted"}
Waiting == c = "waiting"

TypeOK ==
  /\ s \in {"open", "working", "committed", "aborted", "wedged", "dead"}
  /\ work \in 0..MaxWork
  /\ sent \in BOOLEAN
  /\ conn \in {"up", "broken", "halfopen"}
  /\ elapsed \in 0..Ceiling
  /\ idle \in 0..Ceiling
  /\ fate \in {"none", "committed", "aborted"}
  /\ c \in {"waiting", "committed", "aborted", "unknown"}

Init ==
  /\ s = "open" /\ work = 0 /\ sent = FALSE /\ conn = "up"
  /\ elapsed = 0 /\ idle = 0 /\ fate = "none" /\ c = "waiting"

Arrive ==
  /\ s = "open" /\ conn = "up"
  /\ s' = "working"
  /\ idle' = 0
  /\ UNCHANGED <<fate, work, sent, conn, elapsed, c>>

Work ==
  /\ s = "working" /\ work < MaxWork
  /\ work' = work + 1
  /\ idle' = 0
  /\ UNCHANGED <<fate, s, sent, conn, elapsed, c>>

Decide ==
  /\ s = "working"
  /\ \E outcome \in {"committed", "aborted"} : s' = outcome /\ fate' = outcome
  /\ sent' = (conn = "up")
  /\ idle' = 0
  /\ UNCHANGED <<work, conn, elapsed, c>>

Wedge ==
  /\ ~ServerBounded /\ s = "working"
  /\ s' = "wedged"
  /\ UNCHANGED <<fate, work, sent, conn, elapsed, idle, c>>

Crash ==
  /\ Crashes /\ Live
  /\ s' = "dead"
  /\ sent' = FALSE
  /\ conn' \in IF HostLoss /\ conn = "up" THEN {"broken", "halfopen"} ELSE {conn}
  /\ UNCHANGED <<fate, work, elapsed, idle, c>>

\* Before the commit arrives, while it runs, or with its answer on the wire.
Drop ==
  /\ Drops /\ Live /\ Waiting /\ conn = "up"
  /\ conn' = "broken"
  /\ sent' = FALSE
  /\ UNCHANGED <<fate, s, work, elapsed, idle, c>>

\* Time passes while the client waits. A live host that has a commit step to
\* run runs it within `MaxDelay` ticks; otherwise time is free up to the cap.
Tick ==
  /\ Waiting
  /\ s \in {"open", "working"} /\ conn = "up" => idle < MaxDelay
  /\ elapsed < Ceiling \/ idle < Ceiling
  /\ elapsed' = IF elapsed < Ceiling THEN elapsed + 1 ELSE elapsed
  /\ idle' = IF idle < Ceiling THEN idle + 1 ELSE idle
  /\ UNCHANGED <<fate, s, work, sent, conn, c>>

Receive ==
  /\ Waiting /\ sent /\ conn = "up"
  /\ c' = s
  /\ UNCHANGED <<fate, s, work, sent, conn, elapsed, idle>>

\* A broken connection surfaces as a request error. Without an outcome query
\* the client cannot tell a commit that never arrived from one that landed.
Broken ==
  /\ Waiting /\ conn = "broken" /\ (~OutcomeQuery \/ s = "dead")
  /\ c' = "unknown"
  /\ UNCHANGED <<fate, s, work, sent, conn, elapsed, idle>>

Probe ==
  /\ Waiting /\ Keepalive /\ conn = "halfopen"
  /\ conn' = "broken"
  /\ UNCHANGED <<fate, s, work, sent, elapsed, idle, c>>

\* The outcome query, on a fresh connection to the same live server. An open
\* transaction is one whose commit never arrived: the client discards it.
Query ==
  /\ Waiting /\ OutcomeQuery /\ conn = "broken" /\ s # "dead"
  /\ \/ /\ Decided
        /\ c' = s
        /\ UNCHANGED <<s, fate>>
     \/ /\ s = "open"
        /\ s' = "aborted" /\ fate' = "aborted"
        /\ c' = "aborted"
  /\ UNCHANGED <<work, sent, conn, elapsed, idle>>

GiveUp ==
  /\ Waiting /\ ~sent
  /\ \/ Detector = "clock" /\ elapsed >= Ceiling
     \/ Detector = "progress" /\ s # "open" /\ idle >= Ceiling
  /\ c' = "unknown"
  /\ UNCHANGED <<fate, s, work, sent, conn, elapsed, idle>>

Next ==
  \/ Arrive \/ Work \/ Decide \/ Wedge \/ Crash \/ Drop \/ Tick
  \/ Receive \/ Broken \/ Probe \/ Query \/ GiveUp

Spec ==
  /\ Init /\ [][Next]_vars
  /\ WF_vars(Arrive) /\ WF_vars(Decide) /\ WF_vars(Tick)
  /\ WF_vars(Receive) /\ WF_vars(Broken) /\ WF_vars(Probe)
  /\ WF_vars(Query) /\ WF_vars(GiveUp)

Agreement == c \in {"committed", "aborted"} => c = fate

UnknownOnlyWithoutProgress == c = "unknown" => s \in {"dead", "wedged"}

ClientSettles == <>(c # "waiting")

====
