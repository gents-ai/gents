---- MODULE StoreReopen ----
\* lean: none
\* crates: gents-store, gents-documents, gents-tools, gents-cli, gents-host, gents
\* models: StoreHandoff.mayRelease
(***************************************************************************)
(* One holder of a home's store lock building node after node over the    *)
(* same data directory: a fresh store's initialization, its preflight and  *)
(* the served node (`ServedHome::open_with_identity`, a `TestStore` or     *)
(* `SignedHome` template, `gents init`), and a node a test or              *)
(* `ServedHome::reopen` shuts down and starts again. No other opener       *)
(* reaches the engine while the store lock is held, so whatever holds the  *)
(* engine's own lock on the data directory (regolith's `<data dir>/LOCK`,  *)
(* `dir` here) is an engine of a node this holder built.                   *)
(*                                                                          *)
(* An engine takes the directory when its node opens and keeps it after   *)
(* `Node::shutdown` returned, until the engine drops with its last handle  *)
(* (the external premise of `StoreHandoff.mayRelease`). The caller's       *)
(* `Arc<Node>` is one handle; defra-node holds the database in handles of  *)
(* its own (`Tasks`), and its shutdown does not await every one of them    *)
(* (it waits at most a second for each background task), so one may let   *)
(* go after the shutdown returned and after the caller dropped its last    *)
(* `Arc<Node>` (`TasksOutliveShutdown`). An opener that finds the          *)
(* directory held fails: "database directory is already locked for        *)
(* read-write access", read-only or read-write.                            *)
(*                                                                          *)
(* Code (gents-store `local/backend_protection.rs`, `local/home/lock.rs`): *)
(* `Observe` is `engine_directory`, a try of the engine's lock taken       *)
(* before every node build over the locked store (`build_configured`);     *)
(* with `WaitForEngine` a held directory is awaited on the OS lock itself  *)
(* (`wait_for_engine_release`), for at most `STORE_RELEASE_WAIT` when      *)
(* `BoundedWait`; `Waited` is that waiter taking and letting go of the     *)
(* lock. `Open` is the node builder opening the engine. `BeginShutdown`    *)
(* and `ShutdownReturns` are `Node::shutdown`; `DropHandle` is the         *)
(* caller's `drop(node)` before its next build; `LetGo` is defra-node      *)
(* dropping one of its own handles; `Wedge` is one it never drops. Before  *)
(* the fix the next build opened at once (`WaitForEngine = FALSE`).        *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  N,                    \* nodes the holder builds one after another
  Tasks,                \* defra-node's own handles of a node's database
  WaitForEngine,        \* a build first awaits a held engine directory
  BoundedWait,          \* that wait ends at the opening operation's deadline
  TasksOutliveShutdown, \* `Node::shutdown` may return while one is still held
  CanWedge              \* one may never be dropped

Gens == 1..N

VARIABLES
  gen,      \* the node the holder is building or holds
  hp,       \* holder: observing | waiting | opening | open | stopping | closed
            \*         | refused | timedOut | done
  dir,      \* 0, or the node whose engine holds the data directory
  holders,  \* the live handles of each node's engine
  signaled, \* each node's shutdown began: its tasks may let go
  wedged    \* the task of a node that never lets go

vars == <<gen, hp, dir, holders, signaled, wedged>>

Handles == {"caller"} \cup Tasks

TypeOK ==
  /\ gen \in 1..(N + 1)
  /\ hp \in {"observing", "waiting", "opening", "open", "stopping", "closed",
             "refused", "timedOut", "done"}
  /\ dir \in 0..N
  /\ holders \in [Gens -> SUBSET Handles]
  /\ signaled \in [Gens -> BOOLEAN]
  /\ wedged \subseteq Gens \X Tasks

Init ==
  /\ gen = 1
  /\ hp = "observing"
  /\ dir = 0
  /\ holders = [g \in Gens |-> {}]
  /\ signaled = [g \in Gens |-> FALSE]
  /\ wedged = {}

\* Engine `g` drops with its last handle, and its directory lock with it.
Released(g, left) == IF dir = g /\ left = {} THEN 0 ELSE dir

(* The holder. *)

\* Lean `StoreHandoff.mayRelease`, for the holder's own next node: the build
\* proceeds only once the engine's directory is free.
Observe ==
  /\ hp = "observing"
  /\ hp' = IF WaitForEngine /\ dir # 0 THEN "waiting" ELSE "opening"
  /\ UNCHANGED <<gen, dir, holders, signaled, wedged>>

Waited ==
  /\ hp = "waiting" /\ dir = 0
  /\ hp' = "opening"
  /\ UNCHANGED <<gen, dir, holders, signaled, wedged>>

Open ==
  /\ hp = "opening"
  /\ IF dir = 0
       THEN /\ dir' = gen
            /\ holders' = [holders EXCEPT ![gen] = Handles]
            /\ hp' = "open"
       ELSE /\ hp' = "refused"
            /\ UNCHANGED <<dir, holders>>
  /\ UNCHANGED <<gen, signaled, wedged>>

BeginShutdown ==
  /\ hp = "open"
  /\ hp' = "stopping"
  /\ signaled' = [signaled EXCEPT ![gen] = TRUE]
  /\ UNCHANGED <<gen, dir, holders, wedged>>

\* The store is closed; the engine still holds the directory.
ShutdownReturns ==
  /\ hp = "stopping"
  /\ TasksOutliveShutdown \/ holders[gen] \subseteq {"caller"}
  /\ hp' = "closed"
  /\ UNCHANGED <<gen, dir, holders, signaled, wedged>>

DropHandle ==
  /\ hp = "closed"
  /\ LET left == holders[gen] \ {"caller"}
     IN /\ holders' = [holders EXCEPT ![gen] = left]
        /\ dir' = Released(gen, left)
  /\ gen' = gen + 1
  /\ hp' = IF gen = N THEN "done" ELSE "observing"
  /\ UNCHANGED <<signaled, wedged>>

\* The deadline outlasts any engine that is letting go, so it ends a wait
\* only on one a wedged task keeps.
Deadline ==
  /\ BoundedWait /\ hp = "waiting" /\ dir # 0
  /\ \E t \in Tasks : <<dir, t>> \in wedged
  /\ hp' = "timedOut"
  /\ UNCHANGED <<gen, dir, holders, signaled, wedged>>

(* defra-node's tasks. *)

LetGo(g, t) ==
  /\ signaled[g] /\ t \in holders[g] /\ <<g, t>> \notin wedged
  /\ LET left == holders[g] \ {t}
     IN /\ holders' = [holders EXCEPT ![g] = left]
        /\ dir' = Released(g, left)
  /\ UNCHANGED <<gen, hp, signaled, wedged>>

Wedge(g, t) ==
  /\ CanWedge /\ wedged = {} /\ t \in holders[g]
  /\ wedged' = {<<g, t>>}
  /\ UNCHANGED <<gen, hp, dir, holders, signaled>>

Next ==
  \/ Observe \/ Waited \/ Open \/ BeginShutdown \/ ShutdownReturns
  \/ DropHandle \/ Deadline
  \/ \E g \in Gens, t \in Tasks : LetGo(g, t) \/ Wedge(g, t)

Fairness ==
  /\ WF_vars(Observe) /\ WF_vars(Waited) /\ WF_vars(Open)
  /\ WF_vars(BeginShutdown) /\ WF_vars(ShutdownReturns)
  /\ WF_vars(DropHandle) /\ WF_vars(Deadline)
  /\ \A g \in Gens, t \in Tasks : WF_vars(LetGo(g, t))

Spec == Init /\ [][Next]_vars /\ Fairness

(* Properties. *)

\* No node build finds the directory still held by an engine of a node this
\* holder stopped.
ReopenFindsDirectoryFree == hp # "refused"

\* Every wait ends: the stopped engine lets go, or the deadline reports one a
\* wedged task keeps.
ReopenSettles == hp = "waiting" ~> hp \in {"opening", "timedOut"}

\* The holder never hangs: it builds every node, or one of its builds ends.
HolderSettles == <>(hp \in {"done", "timedOut", "refused"})

====
