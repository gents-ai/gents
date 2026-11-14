---- MODULE StoreHandoff ----
(***************************************************************************)
(* Handing one local store from the process that holds it to the next one *)
(* that opens it: a client installation that retires and the attachment   *)
(* that starts its successor, a desktop that quits and is reopened, a     *)
(* runtime that a replacement or a service stop takes down. The store is   *)
(* excluded by an OS advisory lock on `<data dir>.lock` (`lock_store`), and *)
(* that lock file also carries what the holder last declared: its process *)
(* id and, once it has begun to stop, that it is releasing.               *)
(*                                                                          *)
(* `H` holds the store and serves. It may serve forever (a live runtime),  *)
(* or begin to stop, release the lock and acknowledge the stop, retire or  *)
(* quit to whoever asked. It may crash at any time, and a wedged shutdown   *)
(* may hang without ever releasing. Each acquirer is a process that starts *)
(* at any time and tries the lock once: a free lock is taken; a held one   *)
(* is then read for the holder's declaration, and the acquirer either      *)
(* waits for the release (bounded by its own operation's deadline) or is   *)
(* refused at once with the holder's name. The kernel drops an advisory    *)
(* lock when its holder exits; a pid file would not.                       *)
(*                                                                          *)
(* The sequential halves are Lean's: the holder's order is                 *)
(* `ClientInstallation.step` (`retire`, `release`, `close`, `acknowledge`) *)
(* and an acquirer's choice is `StoreHandoff.decide`. `BeginStop`,         *)
(* `Release` and `Ack` are those steps, `Read` applies `decide` to what the *)
(* lock file says; this spec adds only the interleaving of the two         *)
(* processes and the kernel's release on exit.                             *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  H,                    \* the outgoing holder
  Acquirers,            \* processes that open the store after it
  AckAfterRelease,      \* the stop is acknowledged only once the lock is free
  MarkReleasing,        \* beginning to stop declares it in the lock file
  WaitOnReleasing,      \* an acquirer waits for a holder that declared it
  BoundedWait,          \* that wait ends at the operation's own deadline
  KernelReleasesOnExit, \* the lock is an OS lock, not a pid file
  CanCrash,
  CanHang

Procs == {H} \cup Acquirers

VARIABLES
  lock,      \* "free" or the process holding the OS lock
  content,   \* what the lock file says: owner and releasing declaration
  hp,        \* H: serving | stopping | released | hung | crashed
  acked,     \* H acknowledged its stop to the process that asked
  ac,        \* acquirer: idle | trying | observed | waiting | holding | refused | timedOut
  afterAck,  \* ghost: the acquirer started after the acknowledgement
  seen,      \* ghost: holder and its phase when the acquirer found the lock held
  refusal    \* ghost: holder named by a refusal and its phase at the refusal

vars == <<lock, content, hp, acked, ac, afterAck, seen, refusal>>

Phases == {"serving", "stopping", "released", "hung", "crashed"}
None == [holder |-> "none", phase |-> "none"]

PhaseOf(p) == IF p = H THEN hp ELSE "serving"

TypeOK ==
  /\ lock \in Procs \cup {"free"}
  /\ content \in [owner : Procs, releasing : BOOLEAN]
  /\ hp \in Phases
  /\ acked \in BOOLEAN
  /\ ac \in [Acquirers -> {"idle", "trying", "observed", "waiting", "holding",
                           "refused", "timedOut"}]
  /\ afterAck \in [Acquirers -> BOOLEAN]
  /\ seen \in [Acquirers -> [holder : Procs \cup {"none"}, phase : Phases \cup {"none"}]]
  /\ refusal \in [Acquirers -> [holder : Procs \cup {"none"}, phase : Phases \cup {"none"}]]

Init ==
  /\ lock = H
  /\ content = [owner |-> H, releasing |-> FALSE]
  /\ hp = "serving"
  /\ acked = FALSE
  /\ ac = [a \in Acquirers |-> "idle"]
  /\ afterAck = [a \in Acquirers |-> FALSE]
  /\ seen = [a \in Acquirers |-> None]
  /\ refusal = [a \in Acquirers |-> None]

(* The holder. *)

BeginStop ==
  /\ hp = "serving"
  /\ hp' = "stopping"
  /\ content' = IF MarkReleasing THEN [content EXCEPT !.releasing = TRUE] ELSE content
  /\ UNCHANGED <<lock, acked, ac, afterAck, seen, refusal>>

Release ==
  /\ hp = "stopping"
  /\ hp' = "released"
  /\ lock' = "free"
  /\ UNCHANGED <<content, acked, ac, afterAck, seen, refusal>>

Ack ==
  /\ ~acked
  /\ IF AckAfterRelease THEN hp = "released" ELSE hp \in {"stopping", "released"}
  /\ acked' = TRUE
  /\ UNCHANGED <<lock, content, hp, ac, afterAck, seen, refusal>>

Hang ==
  /\ CanHang /\ hp = "stopping"
  /\ hp' = "hung"
  /\ UNCHANGED <<lock, content, acked, ac, afterAck, seen, refusal>>

Crash ==
  /\ CanCrash /\ hp \in {"serving", "stopping", "hung"}
  /\ hp' = "crashed"
  /\ lock' = IF KernelReleasesOnExit /\ lock = H THEN "free" ELSE lock
  /\ UNCHANGED <<content, acked, ac, afterAck, seen, refusal>>

(* An acquirer. *)

Take(a) ==
  /\ lock' = a
  /\ content' = [owner |-> a, releasing |-> FALSE]
  /\ ac' = [ac EXCEPT ![a] = "holding"]

Start(a) ==
  /\ ac[a] = "idle"
  /\ ac' = [ac EXCEPT ![a] = "trying"]
  /\ afterAck' = [afterAck EXCEPT ![a] = acked]
  /\ UNCHANGED <<lock, content, hp, acked, seen, refusal>>

TryLock(a) ==
  /\ ac[a] = "trying"
  /\ IF lock = "free"
       THEN /\ Take(a) /\ UNCHANGED seen
       ELSE /\ ac' = [ac EXCEPT ![a] = "observed"]
            /\ seen' = [seen EXCEPT ![a] = [holder |-> lock, phase |-> PhaseOf(lock)]]
            /\ UNCHANGED <<lock, content>>
  /\ UNCHANGED <<hp, acked, afterAck, refusal>>

\* Lean `StoreHandoff.decide` on the lock file's declaration.
Read(a) ==
  /\ ac[a] = "observed"
  /\ IF WaitOnReleasing /\ content.releasing
       THEN /\ ac' = [ac EXCEPT ![a] = "waiting"] /\ UNCHANGED refusal
       ELSE /\ ac' = [ac EXCEPT ![a] = "refused"]
            /\ refusal' = [refusal EXCEPT ![a] =
                 [holder |-> content.owner, phase |-> PhaseOf(content.owner)]]
  /\ UNCHANGED <<lock, content, hp, acked, afterAck, seen>>

Acquire(a) ==
  /\ ac[a] = "waiting" /\ lock = "free"
  /\ Take(a)
  /\ UNCHANGED <<hp, acked, afterAck, seen, refusal>>

\* The operation's deadline outlasts any shutdown that is making progress, so
\* it ends a wait only on a holder that is not stopping.
Deadline(a) ==
  /\ BoundedWait
  /\ ac[a] = "waiting" /\ lock # "free" /\ PhaseOf(lock) # "stopping"
  /\ ac' = [ac EXCEPT ![a] = "timedOut"]
  /\ UNCHANGED <<lock, content, hp, acked, afterAck, seen, refusal>>

Next ==
  \/ BeginStop \/ Release \/ Ack \/ Hang \/ Crash
  \/ \E a \in Acquirers :
       Start(a) \/ TryLock(a) \/ Read(a) \/ Acquire(a) \/ Deadline(a)

Fairness ==
  /\ WF_vars(Release) /\ WF_vars(Ack)
  /\ \A a \in Acquirers :
       WF_vars(TryLock(a)) /\ WF_vars(Read(a)) /\ WF_vars(Acquire(a))
       /\ WF_vars(Deadline(a))

Spec == Init /\ [][Next]_vars /\ Fairness

(* Properties. *)

\* Never two holders: the outgoing process uses the store until it releases,
\* and at most one acquirer takes it.
NeverTwoHolders ==
  /\ hp \in {"serving", "stopping", "hung"} => \A a \in Acquirers : ac[a] # "holding"
  /\ \A a, b \in Acquirers : a # b => ~(ac[a] = "holding" /\ ac[b] = "holding")

\* An acknowledgement of a stop, retire or quit is never observable while the
\* store is still held by the process that acknowledged it.
AckFollowsRelease == acked => lock # H

\* An acquirer that starts after the acknowledgement finds the store free,
\* unless another acquirer took it first.
AckedHandoff ==
  \A a \in Acquirers :
    afterAck[a] /\ ac[a] \in {"observed", "waiting", "refused", "timedOut"}
      => seen[a].holder \in Acquirers

\* A refusal ("already using") never names a holder that is stopping or has
\* released; a holder that crashed after the acquirer found it serving
\* linearizes at that observation.
RefusalNamesLiveHolder ==
  \A a \in Acquirers :
    ac[a] = "refused" => refusal[a].phase \notin {"stopping", "released"}

\* A holder that died is never found holding the store.
CrashedHolderNeverBlocks ==
  \A a \in Acquirers : seen[a].phase # "crashed"

\* Every acquirer settles: it takes the store, is refused, or reaches its
\* deadline; it never hangs.
AcquirerSettles ==
  \A a \in Acquirers :
    ac[a] \in {"trying", "observed", "waiting"}
      ~> ac[a] \in {"holding", "refused", "timedOut"}

\* An acquirer waiting on a stopping holder takes the store once it is
\* released, unless another acquirer took it or the shutdown wedged.
WaiterTakesReleasedStore ==
  \A a \in Acquirers :
    (ac[a] = "waiting" /\ hp \in {"stopping", "released"})
      ~> (ac[a] = "holding" \/ hp = "hung"
          \/ \E b \in Acquirers \ {a} : ac[b] = "holding")

\* A stop that does not wedge or crash is acknowledged.
StopAcknowledged == hp = "stopping" ~> (acked \/ hp \in {"hung", "crashed"})

====
