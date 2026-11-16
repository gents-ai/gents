---- MODULE BackgroundWakeIdentity ----
(***************************************************************************)
(* Concurrent background-completion notifications of one finished request *)
(* coalescing into one wake request (#3810).                               *)
(*                                                                          *)
(* Each notifier publishes its tool notification through                    *)
(* `persist_background_completion_with_message_canonical_inner`             *)
(* (gents-session request/lifecycle/queue/atomic_inputs.rs:97-190). It      *)
(* first takes the process gate for (node, session, owner, queue key)      *)
(* (`background_completion_gate`, :23-51; held to :189), then runs one     *)
(* `transact_idempotent` transaction (:130-156) that reads the pending      *)
(* wakes of that queue and the session's message sequence, reuses a pending *)
(* wake or creates the next generation, and appends the notification bound *)
(* to that wake (:383-737). DefraDB commits optimistically; a commit whose *)
(* reads changed aborts and the transaction is rebuilt, a bounded number   *)
(* of times (`IdempotentTransactionRetry::Standard`).                       *)
(*                                                                          *)
(* Before the notifications, the fixture of                                 *)
(* `many_concurrent_notifications_publish_one_wake_identity` prepares each  *)
(* background tool in turn (gents-toolcalls test_support/queue.rs           *)
(* `prepare_notification`): it appends the tool's output, which the        *)
(* delivery owner refuses once its observation time passes the tool's      *)
(* accepted deadline, the parent claim's deadline (claim time + 60 s). On  *)
(* main the observation time is the wall clock (`append_tool_output`); the *)
(* fix observes at the claim instant (`append_tool_output_at`). `Tick` is   *)
(* the host's scheduling: nothing bounds how much wall time setup takes.   *)
(*                                                                          *)
(* `ReadConflicts` is DefraDB's premise that a commit aborts when a row it  *)
(* read or a range it queried changed after its snapshot; with it FALSE    *)
(* two transactions that both saw no wake both create one.                 *)
(***************************************************************************)
EXTENDS Integers, FiniteSets

CONSTANTS
  Notifiers,       \* concurrent notifications of the same completed request
  Gate,            \* the process gate serializes the publications
  ReadConflicts,   \* a commit aborts when what it read changed
  MaxAttempts,     \* bounded rebuilds of an aborted transaction
  Deadline,        \* ticks from the claim to the accepted tool deadline
  ObserveAtClaim   \* the fixture appends observed at the claim instant

VARIABLES
  clock,     \* ticks since the parent's claim
  prepared,  \* tools the fixture has prepared
  setup,     \* preparing | refused | ready
  wakes,     \* wake requests created (generation numbers)
  version,   \* commits so far: a snapshot older than this saw stale rows
  holder,    \* notifier holding the gate, or "none"
  st,        \* notifier: idle | queued | read | done | failed
  snapAt,    \* version the notifier's snapshot read
  snapWake,  \* the pending wake its snapshot saw, or -1
  attempts,  \* attempts so far
  got        \* identity returned to the notifier, or -1

vars == <<clock, prepared, setup, wakes, version, holder, st, snapAt, snapWake,
          attempts, got>>

N == Cardinality(Notifiers)

Init ==
  /\ clock = 0 /\ prepared = 0 /\ setup = "preparing"
  /\ wakes = {} /\ version = 0 /\ holder = "none"
  /\ st = [n \in Notifiers |-> "idle"]
  /\ snapAt = [n \in Notifiers |-> 0]
  /\ snapWake = [n \in Notifiers |-> -1]
  /\ attempts = [n \in Notifiers |-> 0]
  /\ got = [n \in Notifiers |-> -1]

Rest == <<wakes, version, holder, st, snapAt, snapWake, attempts, got>>

(* The fixture. *)

Tick ==
  /\ setup = "preparing" /\ clock <= Deadline
  /\ clock' = clock + 1
  /\ UNCHANGED <<prepared, setup>> /\ UNCHANGED Rest

\* `prepare_notification`: the output append checks its observation time
\* against the accepted deadline (delivery `append_tool_output_with_time`).
Prepare ==
  /\ setup = "preparing" /\ prepared < N
  /\ IF ObserveAtClaim \/ clock < Deadline
       THEN /\ prepared' = prepared + 1
            /\ setup' = IF prepared + 1 = N THEN "ready" ELSE "preparing"
       ELSE /\ setup' = "refused" /\ UNCHANGED prepared
  /\ UNCHANGED clock /\ UNCHANGED Rest

(* A notifier. *)

Enqueue(n) ==
  /\ setup = "ready" /\ st[n] = "idle"
  /\ st' = [st EXCEPT ![n] = "queued"]
  /\ UNCHANGED <<clock, prepared, setup, wakes, version, holder, snapAt, snapWake,
                 attempts, got>>

\* atomic_inputs.rs:122-123 (gate) and :421-591 (the snapshot read).
Read(n) ==
  /\ st[n] = "queued"
  /\ Gate => holder \in {"none", n}
  /\ holder' = IF Gate THEN n ELSE holder
  /\ st' = [st EXCEPT ![n] = "read"]
  /\ snapAt' = [snapAt EXCEPT ![n] = version]
  /\ snapWake' = [snapWake EXCEPT ![n] = IF wakes = {} THEN -1 ELSE CHOOSE w \in wakes : TRUE]
  /\ attempts' = [attempts EXCEPT ![n] = @ + 1]
  /\ UNCHANGED <<clock, prepared, setup, wakes, version, got>>

Release(n) == holder' = IF holder = n THEN "none" ELSE holder

\* :617-736: reuse the pending wake or create the next generation, append
\* the notification bound to it, commit.
Commit(n) ==
  /\ st[n] = "read"
  /\ IF ReadConflicts /\ snapAt[n] # version
       THEN \* aborted: rebuild, or give up after the bounded attempts
            /\ IF attempts[n] < MaxAttempts
                 THEN /\ st' = [st EXCEPT ![n] = "queued"] /\ UNCHANGED holder
                 ELSE /\ st' = [st EXCEPT ![n] = "failed"] /\ Release(n)
            /\ UNCHANGED <<wakes, version, got>>
       ELSE /\ LET w == IF snapWake[n] = -1 THEN Cardinality(wakes) ELSE snapWake[n]
               IN /\ wakes' = wakes \cup {w}
                  /\ got' = [got EXCEPT ![n] = w]
            /\ version' = version + 1
            /\ st' = [st EXCEPT ![n] = "done"]
            /\ Release(n)
  /\ UNCHANGED <<clock, prepared, setup, snapAt, snapWake, attempts>>

Next ==
  \/ Tick \/ Prepare
  \/ \E n \in Notifiers : Enqueue(n) \/ Read(n) \/ Commit(n)

Fairness ==
  /\ WF_vars(Prepare)
  /\ \A n \in Notifiers : WF_vars(Enqueue(n)) /\ WF_vars(Read(n)) /\ WF_vars(Commit(n))

Spec == Init /\ [][Next]_vars /\ Fairness

(* Properties. *)

TypeOK ==
  /\ setup \in {"preparing", "refused", "ready"}
  /\ st \in [Notifiers -> {"idle", "queued", "read", "done", "failed"}]

\* The fixture's setup is not refused by a deadline the test does not exercise.
SetupNotRefused == setup # "refused"

\* One wake request for the notifications of one completion.
OneWake == Cardinality(wakes) <= 1

\* Every notifier that finished was handed that same wake.
SameIdentity == \A m, n \in Notifiers :
  st[m] = "done" /\ st[n] = "done" => got[m] = got[n]

\* No notification is lost to aborted attempts.
NoneFailed == \A n \in Notifiers : st[n] # "failed"

\* Every notifier is answered with the wake identity.
EveryNotifierAnswered == \A n \in Notifiers : st[n] = "queued" ~> st[n] = "done"

====
