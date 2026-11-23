---- MODULE PluginStoreWatcherStop ----
\* lean: RuntimeReconcile.SurfacePublication.watchStore, PluginStoreHome.acquire
\* crates: gents, gents-pack
EXTENDS LeanDecision_RuntimeReconcile_SurfacePublication, LeanDecision_PluginStoreHome

\* The runtime's control watcher and the actors around its plugin store. The
\* watcher stops when its store is gone, and that stops the runtime. Setup or
\* a first install creates the store in two steps, its directory and then its
\* lock file, possibly after the runtime started; an installer may hold the
\* lock; a person may remove or replace the home. The runtime pins the home
\* at the path when it starts and captures the store at the first read that
\* finds it complete (`StoreReadPin::identity`), and each watcher tick then
\* reads the store under the shared lock (`try_shared`): two steps, between
\* which any other actor may act.
\*
\* Safeguard, removed by one constant: `RespectCapture` passes whether the
\* store was captured to Lean `watchStore`; without it, a store still being
\* created reads as one that was removed.

CONSTANT RespectCapture

Homes == {"pinned", "other"}

VARIABLES atPath, store, held, captured, observed, stopped, disrupted
vars == <<atPath, store, held, captured, observed, stopped, disrupted>>

Stages == {"none", "dir", "ready"}

Init ==
  /\ atPath = "pinned"
  /\ store \in [Homes -> {"none"}] \cup {[h \in Homes |-> IF h = "pinned" THEN s ELSE "none"] : s \in Stages}
  /\ held = FALSE
  /\ captured = (store["pinned"] = "ready")
  /\ observed = "none"
  /\ stopped = FALSE
  /\ disrupted = FALSE

Here == atPath = "pinned"
SameHome == atPath \in {"pinned", "none"}
StorePresent == Here /\ store["pinned"] \in {"dir", "ready"}
LockPresent == Here /\ store["pinned"] = "ready"

\* Setup or an installer creates the next part of the store in the home at
\* the path; creation never replaces what is already there.
Create ==
  /\ atPath # "none"
  /\ store[atPath] # "ready"
  /\ store' = [store EXCEPT ![atPath] = IF @ = "none" THEN "dir" ELSE "ready"]
  /\ UNCHANGED <<atPath, held, captured, observed, stopped, disrupted>>

Hold ==
  /\ ~held /\ atPath # "none" /\ store[atPath] = "ready"
  /\ held' = TRUE
  /\ UNCHANGED <<atPath, store, captured, observed, stopped, disrupted>>

Release ==
  /\ held
  /\ held' = FALSE
  /\ UNCHANGED <<atPath, store, captured, observed, stopped, disrupted>>

Remove ==
  /\ atPath = "pinned"
  /\ atPath' = "none"
  /\ held' = FALSE
  /\ disrupted' = TRUE
  /\ UNCHANGED <<store, captured, observed, stopped>>

Replace ==
  /\ atPath # "other"
  /\ atPath' = "other"
  /\ held' = FALSE
  /\ disrupted' = TRUE
  /\ UNCHANGED <<store, captured, observed, stopped>>

\* The first read that finds the pinned home's store complete captures it.
Capture ==
  /\ ~stopped /\ ~captured
  /\ acquire(SameHome, StorePresent, LockPresent, FALSE) = "acquired"
  /\ captured' = TRUE
  /\ UNCHANGED <<atPath, store, held, observed, stopped, disrupted>>

\* A tick reads the store under the shared lock and decides from what it saw.
Tick ==
  /\ ~stopped
  /\ LET outcome == acquire(SameHome, StorePresent, LockPresent, held)
         action == watchStore(IF RespectCapture THEN captured ELSE TRUE, outcome)
     IN /\ observed' = action
        /\ stopped' = (action = "stop")
  /\ UNCHANGED <<atPath, store, held, captured, disrupted>>

Next == Create \/ Hold \/ Release \/ Remove \/ Replace \/ Capture \/ Tick
Spec == Init /\ [][Next]_vars

TypeOK ==
  /\ atPath \in Homes \cup {"none"}
  /\ store \in [Homes -> Stages]
  /\ observed \in {"none", "reconcile", "contended", "stop"}

\* The watcher, and with it the runtime, stops only after its home was
\* removed or replaced, never while its store is still being created.
StopsOnlyForAGoneStore == stopped => disrupted
====
