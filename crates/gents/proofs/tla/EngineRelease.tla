---- MODULE EngineRelease ----
\* lean: none
\* crates: gents-store, gents-cli
\* models: StoreHandoff.mayRelease
(***************************************************************************)
(* One process opens a store's engine again after shutting down the node  *)
(* that held it, while it keeps the store lock throughout: first-time      *)
(* setup (`FreshBackendStore::initialize`) builds a temporary node, shuts  *)
(* it down, drops it and returns, and its caller opens the store at once   *)
(* (`gents init` builds its serving node; the protected start's preflight  *)
(* opens it read-only first). No store-lock handoff stands between the two *)
(* opens, so nothing in `StoreHandoff` covers them.                        *)
(*                                                                          *)
(* The engine (regolith) holds `<data dir>/LOCK` until its last handle     *)
(* drops; `Db::close` does not give the directory back. The node's handles *)
(* are its own fields and the background tasks it owns (defra-node         *)
(* `BackgroundTasks`: vector rebuild, downsample, deferred index, parked   *)
(* replay, transaction sweep), each a daemon on one worker of the node's   *)
(* pool holding a database clone. `Node::shutdown` signals each task       *)
(* (`Background::abort`), which ends at its next poll on its worker and    *)
(* drops its clone there. With `JoinTasks` the shutdown returns only once  *)
(* every task has dropped its clone; without it, it stops waiting for a    *)
(* task at a deadline (`BackgroundTasks::shutdown` gives each one second,  *)
(* the pool's stop thirty) and returns with the clone still held, which a  *)
(* starved scheduler makes possible. With `AwaitDirectory` the opener      *)
(* applies Lean `StoreHandoff.mayRelease` to the engine's directory lock   *)
(* before it opens, waiting on that lock.                                  *)
(*                                                                          *)
(* `Signal`, `GiveUp`/`Joined` and `Return` are `Node::shutdown`; `Drop`   *)
(* is the caller's drop of the node; `Observe(t)` is task `t` ending at    *)
(* its next poll; `Open` is the next node's engine taking the directory.   *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  Tasks,          \* the node's background tasks that hold a store handle
  JoinTasks,      \* shutdown returns only once every task dropped its handle
  AwaitDirectory, \* the opener waits for the engine's directory lock first
  CanWedge        \* a task may never be polled again

VARIABLES
  phase,    \* owner: running | stopping | returned | dropped | open | refused
  holds,    \* tasks still holding a handle of the engine
  awaited,  \* tasks the shutdown is done waiting for, joined or given up
  wedged    \* tasks that are never polled again

vars == <<phase, holds, awaited, wedged>>

\* The engine, and with it the directory lock, lives while any handle does:
\* the node's own until the caller drops it, and each task's.
DirectoryHeld == phase \in {"running", "stopping", "returned"} \/ holds # {}

TypeOK ==
  /\ phase \in {"running", "stopping", "returned", "dropped", "open", "refused"}
  /\ holds \subseteq Tasks
  /\ awaited \subseteq Tasks
  /\ wedged \subseteq Tasks

Init ==
  /\ phase = "running"
  /\ holds = Tasks
  /\ awaited = {}
  /\ wedged = {}

Signal ==
  /\ phase = "running"
  /\ phase' = "stopping"
  /\ UNCHANGED <<holds, awaited, wedged>>

\* The task's next poll sees its stop and drops its handle on its worker.
Observe(t) ==
  /\ phase # "running"
  /\ t \in holds /\ t \notin wedged
  /\ holds' = holds \ {t}
  /\ UNCHANGED <<phase, awaited, wedged>>

Wedge(t) ==
  /\ CanWedge /\ t \in holds /\ t \notin wedged
  /\ wedged' = wedged \cup {t}
  /\ UNCHANGED <<phase, holds, awaited>>

Joined(t) ==
  /\ phase = "stopping" /\ t \notin awaited /\ t \notin holds
  /\ awaited' = awaited \cup {t}
  /\ UNCHANGED <<phase, holds, wedged>>

\* The wait for a task ends at its deadline: the scheduler has not polled it.
GiveUp(t) ==
  /\ ~JoinTasks
  /\ phase = "stopping" /\ t \notin awaited /\ t \in holds
  /\ awaited' = awaited \cup {t}
  /\ UNCHANGED <<phase, holds, wedged>>

Return ==
  /\ phase = "stopping" /\ awaited = Tasks
  /\ phase' = "returned"
  /\ UNCHANGED <<holds, awaited, wedged>>

Drop ==
  /\ phase = "returned"
  /\ phase' = "dropped"
  /\ UNCHANGED <<holds, awaited, wedged>>

\* Lean `StoreHandoff.mayRelease` on the directory, when the opener asks it.
Open ==
  /\ phase = "dropped"
  /\ AwaitDirectory => ~DirectoryHeld
  /\ phase' = IF DirectoryHeld THEN "refused" ELSE "open"
  /\ UNCHANGED <<holds, awaited, wedged>>

Next ==
  \/ Signal \/ Return \/ Drop \/ Open
  \/ \E t \in Tasks : Observe(t) \/ Wedge(t) \/ Joined(t) \/ GiveUp(t)

Spec ==
  /\ Init /\ [][Next]_vars
  /\ WF_vars(Signal) /\ WF_vars(Return) /\ WF_vars(Drop) /\ WF_vars(Open)
  /\ \A t \in Tasks : WF_vars(Observe(t)) /\ WF_vars(Joined(t))

\* The next open never finds the directory still held by the node it follows.
ReopenSucceeds == phase # "refused"

\* Shutdown returns only once no task of the node holds the engine.
ReturnReleasesEngine == phase \in {"returned", "dropped", "open", "refused"} => holds = {}

\* The owner opens the store again.
OwnerOpens == <>(phase = "open")

====
