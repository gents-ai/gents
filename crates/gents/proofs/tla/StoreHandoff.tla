---- MODULE StoreHandoff ----
\* lean: none
\* crates: gents-store, gents-client-host, gents-desktop-core, gents-desktop-bridge, gents-cli, gents-service, gents-host, gents
\* models: StoreHandoff.decide, StoreHandoff.mayRelease, ClientInstallation.step
(***************************************************************************)
(* Handing one local store from the process that holds it to the next one *)
(* that opens it: a client installation that retires and the attachment   *)
(* that starts its successor, a desktop that quits and is reopened, a     *)
(* runtime that a replacement or a service stop takes down, and the same  *)
(* handoffs inside one process (a client core closed and started again, a *)
(* listing node a pack update shuts down, an init followed by another     *)
(* open of its home). The store is excluded by an OS advisory lock on      *)
(* `<data dir>.lock` (`lock_store`), and that lock file                    *)
(* also carries what the holder last declared: its process id and, once it *)
(* has begun to stop, that it is releasing.                                *)
(*                                                                          *)
(* The storage engine inside the node takes a second OS lock of its own,  *)
(* `<data dir>/LOCK` (`dir` here), when the node opens, and an opener that *)
(* finds it held fails ("database directory is already locked for         *)
(* read-write access"), whether it opens read-write or read-only: a shared *)
(* lock conflicts with the holder's exclusive one. Every opener takes the  *)
(* store lock first, so the store lock must cover the engine's for as long *)
(* as the engine holds it.                                                 *)
(*                                                                          *)
(* `H` holds the store and serves. It may serve forever (a live runtime),  *)
(* or begin to stop: declare the release (`StoreLock::declare_releasing`), *)
(* shut its node down (`Node::shutdown`; `CloseNode`), release the store   *)
(* lock and acknowledge the stop, retire or quit to whoever asked. Regolith *)
(* keeps its directory locked after its close, until the engine itself     *)
(* drops (`Db::close`: "Close does not give the directory back"), unless   *)
(* `CloseReleasesDirectory`; the engine drops with the last handle of the  *)
(* node (`DropEngine`). Other handles of the node (another access, a node  *)
(* clone) share its write gate, and with it the store claim, and keep the  *)
(* engine too (`otherAccess`). Whoever acknowledges lets go of its handles *)
(* once the shutdown returned and acknowledges after the release; with     *)
(* `HolderOutlivesAck` they are kept until after the acknowledgement, as a *)
(* shutdown that awaited the release itself would do. H may instead        *)
(* drop its claim without a shutdown while the node stays up (`Abandon`:   *)
(* a `ConfigAccess` dropped beside a node somebody else still holds). It   *)
(* may crash at any time, and a wedged shutdown may hang without ever      *)
(* releasing. Each acquirer is a process that starts at any time and tries *)
(* the store lock once: a free lock is taken; a held one is then read for  *)
(* the holder's declaration, and the acquirer either waits for the release *)
(* (bounded by its own operation's deadline) or is refused at once with    *)
(* the holder's name. An acquirer that takes the store then opens the      *)
(* engine. The kernel drops both locks when their holder exits; a pid file *)
(* would not.                                                              *)
(*                                                                          *)
(* The sequential halves are Lean's: the holder's order is                 *)
(* `ClientInstallation.step` (`retire`, `release`, `close`, `acknowledge`),*)
(* an acquirer's choice is `StoreHandoff.decide` and the holder's release  *)
(* is `StoreHandoff.mayRelease`. `BeginStop`, `Release` and `Ack` are the  *)
(* holder's steps, `Read` applies `decide` to what the lock file says and  *)
(* `Release` applies `mayRelease` to the engine's lock; this spec adds the *)
(* interleaving of the processes and the kernel's release on exit.         *)
(*                                                                          *)
(* Code (gents-store `local/home/lock.rs`, `config_client/txn/gate.rs`;    *)
(* gents-desktop-core `client/core.rs`; gents-client-host `native.rs`):    *)
(* `BeginStop` is `begin_store_release`; `CloseNode` is `Node::shutdown`;  *)
(* `Release` is the drop of the `StoreLock`, taken out of the claim by     *)
(* `release_store` once the shutdown returned or dropped with the claim's  *)
(* last owner, and it waits for the engine's directory lock when           *)
(* `ReleaseWaitsForDirectory`; `Ack` is the retiring installation's answer *)
(* once it let go of its core and the release notice resolved, and init's  *)
(* return once its node and the pack installer's are gone and released    *)
(* (gents-cli `commands/init.rs`). `Read` is `try_store`: every opener   *)
(* that takes the store to act on it applies it through `acquire_store`   *)
(* (init, offline commands, desktop and service resets and repairs,       *)
(* runtime preparation, trial homes); a probe that only reports whether   *)
(* the store is held now takes `lock_store` and is no acquirer here.      *)
(* Before the fix (#3802) the claim was released only when the last owner *)
(* of the node's write gate dropped it (`ReleaseByShutdownOwner = FALSE`), *)
(* at once, with the engine's lock still held (`ReleaseWaitsForDirectory  *)
(* = FALSE`), and `ClientCore::shutdown` awaited that release while its    *)
(* caller still held the core (`HolderOutlivesAck`).                       *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  H,                        \* the outgoing holder
  Acquirers,                \* processes that open the store after it
  AckAfterRelease,          \* the stop is acknowledged only once the lock is free
  MarkReleasing,            \* beginning to stop declares it in the lock file
  WaitOnReleasing,          \* an acquirer waits for a holder that declared it
  BoundedWait,              \* that wait ends at the operation's own deadline
  KernelReleasesOnExit,     \* the lock is an OS lock, not a pid file
  CloseReleasesDirectory,   \* the engine's close releases its directory lock
  ReleaseWaitsForDirectory, \* the store lock is released only once the engine's is free
  ReleaseByShutdownOwner,   \* the shutdown releases the claim, whoever else shares the gate
  OtherAccess,              \* another handle of the node is held across the shutdown
  HolderOutlivesAck,        \* node handles are let go only after the acknowledgement
  CanAbandon,               \* the claim may be dropped while the node stays up
  CanCrash,
  CanHang

Procs == {H} \cup Acquirers

VARIABLES
  lock,        \* "free" or the process holding the store lock
  dir,         \* "free" or the process whose engine holds the data directory
  content,     \* what the lock file says: owner and releasing declaration
  hp,          \* H: serving | stopping | closed | released | abandoned | hung | crashed
  stopAsked,   \* somebody asked H to stop and awaits the acknowledgement
  acked,       \* H acknowledged its stop to the process that asked
  engineAlive, \* H's own handle of its node (and with it the engine) exists
  otherAccess, \* another handle of H's node, sharing its write gate, is held
  ac,          \* acquirer: idle | trying | observed | waiting | holding | open
               \*           | dirRefused | refused | timedOut
  afterAck,    \* ghost: the acquirer started after the acknowledgement
  seen,        \* ghost: holder and its phase when the acquirer found the lock held
  refusal      \* ghost: holder named by a refusal and its phase at the refusal

vars == <<lock, dir, content, hp, stopAsked, acked, engineAlive, otherAccess,
          ac, afterAck, seen, refusal>>

Phases == {"serving", "stopping", "closed", "released", "abandoned", "hung", "crashed"}
None == [holder |-> "none", phase |-> "none"]

PhaseOf(p) == IF p = H THEN hp ELSE "serving"

\* Phases in which the holder is on its way to releasing the store.
Releasing == {"stopping", "closed", "released", "abandoned"}

\* The acquirer holds the store lock.
Holding(a) == ac[a] \in {"holding", "open", "dirRefused"}

TypeOK ==
  /\ lock \in Procs \cup {"free"}
  /\ dir \in Procs \cup {"free"}
  /\ content \in [owner : Procs, releasing : BOOLEAN]
  /\ hp \in Phases
  /\ stopAsked \in BOOLEAN
  /\ acked \in BOOLEAN
  /\ engineAlive \in BOOLEAN
  /\ otherAccess \in BOOLEAN
  /\ ac \in [Acquirers -> {"idle", "trying", "observed", "waiting", "holding",
                           "open", "dirRefused", "refused", "timedOut"}]
  /\ afterAck \in [Acquirers -> BOOLEAN]
  /\ seen \in [Acquirers -> [holder : Procs \cup {"none"}, phase : Phases \cup {"none"}]]
  /\ refusal \in [Acquirers -> [holder : Procs \cup {"none"}, phase : Phases \cup {"none"}]]

Init ==
  /\ lock = H
  /\ dir = H
  /\ content = [owner |-> H, releasing |-> FALSE]
  /\ hp = "serving"
  /\ stopAsked = FALSE
  /\ acked = FALSE
  /\ engineAlive = TRUE
  /\ otherAccess = OtherAccess
  /\ ac = [a \in Acquirers |-> "idle"]
  /\ afterAck = [a \in Acquirers |-> FALSE]
  /\ seen = [a \in Acquirers |-> None]
  /\ refusal = [a \in Acquirers |-> None]

acquirerVars == <<ac, afterAck, seen, refusal>>

(* The holder. *)

BeginStop ==
  /\ hp = "serving"
  /\ hp' = "stopping"
  /\ stopAsked' = TRUE
  /\ content' = IF MarkReleasing THEN [content EXCEPT !.releasing = TRUE] ELSE content
  /\ UNCHANGED <<lock, dir, acked, engineAlive, otherAccess, acquirerVars>>

\* `Node::shutdown` returned: its store is closed. Regolith keeps its
\* directory locked until the engine itself drops, unless its close
\* releases it.
CloseNode ==
  /\ hp = "stopping"
  /\ hp' = "closed"
  /\ dir' = IF CloseReleasesDirectory /\ dir = H THEN "free" ELSE dir
  /\ UNCHANGED <<lock, content, stopAsked, acked, engineAlive, otherAccess, acquirerVars>>

\* The claim's last owner drops it while the node keeps running. The claim
\* declares the release first, as the shutdown does.
Abandon ==
  /\ CanAbandon /\ hp = "serving"
  /\ hp' = "abandoned"
  /\ content' = IF MarkReleasing THEN [content EXCEPT !.releasing = TRUE] ELSE content
  /\ UNCHANGED <<lock, dir, stopAsked, acked, engineAlive, otherAccess, acquirerVars>>

\* Lean `StoreHandoff.mayRelease`: the store lock goes only once the engine's
\* directory lock is free. Before the fix the claim lived in the node's write
\* gate until its last owner dropped it, so a shutdown released it only once
\* every other access on the node was gone.
Release ==
  /\ \/ hp = "closed" /\ (ReleaseByShutdownOwner \/ ~otherAccess)
     \/ hp = "abandoned"
  /\ ReleaseWaitsForDirectory => dir # H
  /\ hp' = "released"
  /\ lock' = "free"
  /\ UNCHANGED <<dir, content, stopAsked, acked, engineAlive, otherAccess, acquirerVars>>

Ack ==
  /\ stopAsked /\ ~acked
  /\ IF AckAfterRelease THEN hp = "released" ELSE hp \in {"stopping", "closed", "released"}
  /\ acked' = TRUE
  /\ UNCHANGED <<lock, dir, content, hp, stopAsked, engineAlive, otherAccess, acquirerVars>>

\* Handles of the node are let go once its shutdown returned, or, when the
\* holder outlives its acknowledgement, only after it. A node nobody asked to
\* stop goes whenever its holders let go. The engine, and with it its
\* directory lock, goes with the last handle.
MayLetGo ==
  /\ hp \in {"closed", "released", "abandoned"}
  /\ (HolderOutlivesAck /\ stopAsked) => acked

DropEngine ==
  /\ engineAlive /\ MayLetGo
  /\ engineAlive' = FALSE
  /\ dir' = IF dir = H /\ ~otherAccess THEN "free" ELSE dir
  /\ UNCHANGED <<lock, content, hp, stopAsked, acked, otherAccess, acquirerVars>>

DropOtherAccess ==
  /\ otherAccess /\ MayLetGo
  /\ otherAccess' = FALSE
  /\ dir' = IF dir = H /\ ~engineAlive THEN "free" ELSE dir
  /\ UNCHANGED <<lock, content, hp, stopAsked, acked, engineAlive, acquirerVars>>

Hang ==
  /\ CanHang /\ hp = "stopping"
  /\ hp' = "hung"
  /\ UNCHANGED <<lock, dir, content, stopAsked, acked, engineAlive, otherAccess, acquirerVars>>

Crash ==
  /\ CanCrash /\ hp \in {"serving", "stopping", "closed", "abandoned", "hung"}
  /\ hp' = "crashed"
  /\ lock' = IF KernelReleasesOnExit /\ lock = H THEN "free" ELSE lock
  /\ dir' = IF dir = H THEN "free" ELSE dir
  /\ engineAlive' = FALSE
  /\ otherAccess' = FALSE
  /\ UNCHANGED <<content, stopAsked, acked, acquirerVars>>

(* An acquirer. *)

Take(a) ==
  /\ lock' = a
  /\ content' = [owner |-> a, releasing |-> FALSE]
  /\ ac' = [ac EXCEPT ![a] = "holding"]

holderVars == <<dir, hp, stopAsked, acked, engineAlive, otherAccess>>

Start(a) ==
  /\ ac[a] = "idle"
  /\ ac' = [ac EXCEPT ![a] = "trying"]
  /\ afterAck' = [afterAck EXCEPT ![a] = acked]
  /\ UNCHANGED <<lock, content, holderVars, seen, refusal>>

TryLock(a) ==
  /\ ac[a] = "trying"
  /\ IF lock = "free"
       THEN /\ Take(a) /\ UNCHANGED seen
       ELSE /\ ac' = [ac EXCEPT ![a] = "observed"]
            /\ seen' = [seen EXCEPT ![a] = [holder |-> lock, phase |-> PhaseOf(lock)]]
            /\ UNCHANGED <<lock, content>>
  /\ UNCHANGED <<holderVars, afterAck, refusal>>

\* Lean `StoreHandoff.decide` on the lock file's declaration.
Read(a) ==
  /\ ac[a] = "observed"
  /\ IF WaitOnReleasing /\ content.releasing
       THEN /\ ac' = [ac EXCEPT ![a] = "waiting"] /\ UNCHANGED refusal
       ELSE /\ ac' = [ac EXCEPT ![a] = "refused"]
            /\ refusal' = [refusal EXCEPT ![a] =
                 [holder |-> content.owner, phase |-> PhaseOf(content.owner)]]
  /\ UNCHANGED <<lock, content, holderVars, afterAck, seen>>

Acquire(a) ==
  /\ ac[a] = "waiting" /\ lock = "free"
  /\ Take(a)
  /\ UNCHANGED <<holderVars, afterAck, seen, refusal>>

\* The new holder builds its node; the engine takes the directory or fails.
Open(a) ==
  /\ ac[a] = "holding"
  /\ IF dir = "free"
       THEN /\ dir' = a /\ ac' = [ac EXCEPT ![a] = "open"]
       ELSE /\ ac' = [ac EXCEPT ![a] = "dirRefused"] /\ UNCHANGED dir
  /\ UNCHANGED <<lock, content, hp, stopAsked, acked, engineAlive, otherAccess,
                 afterAck, seen, refusal>>

\* The operation's deadline outlasts any shutdown that is making progress, so
\* it ends a wait only on a holder that is not on its way to releasing.
Deadline(a) ==
  /\ BoundedWait
  /\ ac[a] = "waiting" /\ lock # "free" /\ PhaseOf(lock) \notin Releasing
  /\ ac' = [ac EXCEPT ![a] = "timedOut"]
  /\ UNCHANGED <<lock, content, holderVars, afterAck, seen, refusal>>

Next ==
  \/ BeginStop \/ CloseNode \/ Abandon \/ Release \/ Ack \/ DropEngine
  \/ DropOtherAccess \/ Hang \/ Crash
  \/ \E a \in Acquirers :
       Start(a) \/ TryLock(a) \/ Read(a) \/ Acquire(a) \/ Open(a) \/ Deadline(a)

Fairness ==
  /\ WF_vars(CloseNode) /\ WF_vars(Release) /\ WF_vars(Ack)
  /\ WF_vars(DropEngine) /\ WF_vars(DropOtherAccess)
  /\ \A a \in Acquirers :
       WF_vars(TryLock(a)) /\ WF_vars(Read(a)) /\ WF_vars(Acquire(a))
       /\ WF_vars(Open(a)) /\ WF_vars(Deadline(a))

Spec == Init /\ [][Next]_vars /\ Fairness

(* Properties. *)

\* Never two holders: the outgoing process uses the store until it releases,
\* and at most one acquirer takes it.
NeverTwoHolders ==
  /\ hp \in {"serving", "stopping", "closed", "abandoned", "hung"}
       => \A a \in Acquirers : ~Holding(a)
  /\ \A a, b \in Acquirers : a # b => ~(Holding(a) /\ Holding(b))

\* An acknowledgement of a stop, retire or quit is never observable while the
\* store is still held by the process that acknowledged it.
AckFollowsRelease == acked => lock # H

\* An acquirer that starts after the acknowledgement finds the store free,
\* unless another acquirer took it first.
AckedHandoff ==
  \A a \in Acquirers :
    afterAck[a] /\ ac[a] \in {"observed", "waiting", "refused", "timedOut"}
      => seen[a].holder \in Acquirers

\* A refusal ("already using") never names a holder that is on its way to
\* releasing; a holder that crashed after the acquirer found it serving
\* linearizes at that observation.
RefusalNamesLiveHolder ==
  \A a \in Acquirers :
    ac[a] = "refused" => refusal[a].phase \notin Releasing

\* A holder that died is never found holding the store.
CrashedHolderNeverBlocks ==
  \A a \in Acquirers : seen[a].phase # "crashed"

\* An acquirer that takes the store opens it: the engine of the process it
\* took the store from never still holds the data directory.
SuccessorOpensStore ==
  \A a \in Acquirers : ac[a] # "dirRefused"

\* Every acquirer settles: it opens the store, is refused, or reaches its
\* deadline; it never hangs.
AcquirerSettles ==
  \A a \in Acquirers :
    ac[a] \in {"trying", "observed", "waiting", "holding"}
      ~> ac[a] \in {"open", "dirRefused", "refused", "timedOut"}

\* An acquirer waiting on a stopping holder takes the store once it is
\* released, unless another acquirer took it or the shutdown wedged.
WaiterTakesReleasedStore ==
  \A a \in Acquirers :
    (ac[a] = "waiting" /\ hp \in {"stopping", "closed", "released"})
      ~> (Holding(a) \/ hp = "hung"
          \/ \E b \in Acquirers \ {a} : Holding(b))

\* A stop that does not wedge or crash is acknowledged.
StopAcknowledged == hp = "stopping" ~> (acked \/ hp \in {"hung", "crashed"})

====
