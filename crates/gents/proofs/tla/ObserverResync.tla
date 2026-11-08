---- MODULE ObserverResync ----
\* The desktop observer's delivery and resync loop for runtime documents
\* (`observe.rs`, `observe/delivery.rs`, `observe/projection_store.rs`) and
\* the direct refresh path beside it (`ClientCore::refresh_store`,
\* `refresh_node`, `refresh_config_source`).
\*
\* Runtime. Each node's runtime commits projected writes (`db`). DefraDB
\* delivers a change event for a commit only on a subscription armed before
\* it, after the commit is visible to a new read, and gives no initial
\* snapshot; a dropped stream loses every undelivered event. `Renew` is a
\* runtime write the projection does not show (an execution-lease renewal):
\* it emits an event without a projected version. At most `K` renewals land on
\* a node while one of its loads is in flight (the bounded event rate).
\*
\* Observer (one task, per-node state). Arming every root precedes the
\* snapshot request: `SubscriptionsReady` queues the node. A queued node starts
\* one load, which captures the store fence, then reads the runtime in one
\* snapshot, then publishes through the fence. A rejected load is queued
\* again; with `RejectDelay` it first waits the 250 ms timer (before the fix).
\* A dropped stream aborts the node's in-flight load.
\*
\* Fences. A publication (an accepted load or direct refresh) moves its node's
\* fence. A delivered event is a request event: `EventScope` "all" moves every
\* scope (`observe_request_event(None)`, before the fix) and "node" only the
\* source node's. `EventsFenceReloads` TRUE puts request events on the fence a
\* snapshot reload checks (before the fix); FALSE leaves them on the patch
\* fence only, which this model does not exercise (Lean
\* `ClientDataPublication.request_event_preserves_reload`). The reconnect
\* request event fences like any unclassified event.
\*
\* Direct refresh. A user mutation refreshes its node once: capture, read,
\* publish through the fence. A rejected refresh queues the observer; with
\* `FallbackOnReject` it instead reads again for the same mutation, as
\* `refresh_config_source` and the session commands fell back to
\* `refresh_store` when `refresh_node` was rejected.
\*
\* Each remaining constant breaks one guard of the fixed design.
EXTENDS Naturals

CONSTANTS
  Nodes,                    \* runtimes the desktop observes
  RenewNodes,               \* nodes whose runtimes renew leases during loads
  LiveNodes,                \* nodes EventualPublication is checked for
  MaxWrites,                \* projected writes per node
  K,                        \* renewals per node per in-flight load
  MaxDrops,                 \* stream drops per node
  MaxRefreshes,             \* user mutations that refresh a node directly
  EventScope,               \* "all" | "node"
  EventsFenceReloads,       \* request events reject snapshot reloads
  PublicationsFenceReloads, \* a publication rejects an older reload
  RejectDelay,              \* a rejected load waits the retry timer
  EventQueues,              \* a delivered event queues its node
  ArmFirst,                 \* the reconnect snapshot waits for arming
  FallbackOnReject          \* a rejected refresh reads again

ASSUME /\ RenewNodes \subseteq Nodes /\ LiveNodes \subseteq Nodes
       /\ EventScope \in {"all", "node"}
       /\ \A flag \in {EventsFenceReloads, PublicationsFenceReloads, RejectDelay,
                       EventQueues, ArmFirst, FallbackOnReject} : flag \in BOOLEAN

VARIABLES
  db,            \* node -> projected version committed on the runtime
  undelivered,   \* node -> a committed change whose event is not delivered
  stream,        \* node -> "arming" | "armed"
  drops,         \* node -> stream drops so far
  pending,       \* node -> queued for a load
  waiting,       \* node -> the retry timer must fire before the next load
  load,          \* node -> "idle" | "captured" | "read"
  loadFenced,    \* node -> the load's fence moved since its capture
  loadRead,      \* node -> version the load read
  renews,        \* node -> renewals during the current load
  refresh,       \* node -> "idle" | "captured" | "read"
  refreshFenced, \* node -> the refresh's fence moved since its capture
  refreshRead,   \* node -> version the refresh read
  refreshes,     \* node -> user mutations so far
  refreshReads,  \* node -> reads for the current user mutation
  store,         \* node -> projected version the desktop shows
  outcome,       \* node -> "none" | "published" | "rejected": the last load
  fresh,         \* node -> a reason to read arrived since the last load start
  duplicate      \* a load started without a new reason to read

vars == <<db, undelivered, stream, drops, pending, waiting, load, loadFenced,
          loadRead, renews, refresh, refreshFenced, refreshRead, refreshes,
          refreshReads, store, outcome, fresh, duplicate>>

runtimeVars == <<db, drops>>
loadVars == <<load, loadFenced, loadRead, renews, outcome>>
refreshVars == <<refresh, refreshFenced, refreshRead, refreshes, refreshReads>>

Init ==
  /\ db = [n \in Nodes |-> 0]
  /\ undelivered = [n \in Nodes |-> FALSE]
  /\ stream = [n \in Nodes |-> "arming"]
  /\ drops = [n \in Nodes |-> 0]
  /\ pending = [n \in Nodes |-> ~ArmFirst]
  /\ waiting = [n \in Nodes |-> FALSE]
  /\ load = [n \in Nodes |-> "idle"]
  /\ loadFenced = [n \in Nodes |-> FALSE]
  /\ loadRead = [n \in Nodes |-> 0]
  /\ renews = [n \in Nodes |-> 0]
  /\ refresh = [n \in Nodes |-> "idle"]
  /\ refreshFenced = [n \in Nodes |-> FALSE]
  /\ refreshRead = [n \in Nodes |-> 0]
  /\ refreshes = [n \in Nodes |-> 0]
  /\ refreshReads = [n \in Nodes |-> 0]
  /\ store = [n \in Nodes |-> 0]
  /\ outcome = [n \in Nodes |-> "none"]
  /\ fresh = [n \in Nodes |-> ~ArmFirst]
  /\ duplicate = FALSE

\* Nodes whose captured reloads a request event from `n` rejects.
EventFenced(n) ==
  IF ~EventsFenceReloads THEN {}
  ELSE IF EventScope = "all" THEN Nodes ELSE {n}

PublicationFenced(n) == IF PublicationsFenceReloads THEN {n} ELSE {}

Moved(set, fenced) == [m \in Nodes |-> fenced[m] \/ m \in set]

Reason(n) == fresh' = [fresh EXCEPT ![n] = TRUE]

----
\* Runtime.

Write(n) ==
  /\ db[n] < MaxWrites
  /\ db' = [db EXCEPT ![n] = @ + 1]
  /\ undelivered' = [undelivered EXCEPT ![n] = @ \/ stream[n] = "armed"]
  /\ UNCHANGED <<drops, stream, pending, waiting, fresh, duplicate, store>>
  /\ UNCHANGED loadVars /\ UNCHANGED refreshVars

Renew(n) ==
  /\ n \in RenewNodes
  /\ stream[n] = "armed"
  /\ load[n] # "idle"
  /\ renews[n] < K
  /\ renews' = [renews EXCEPT ![n] = @ + 1]
  /\ undelivered' = [undelivered EXCEPT ![n] = TRUE]
  /\ UNCHANGED <<db, drops, stream, pending, waiting, fresh, duplicate, store,
                 load, loadFenced, loadRead, outcome>>
  /\ UNCHANGED refreshVars

\* Delivery (`observe/delivery.rs`): request event, then queue the source.
Deliver(n) ==
  /\ stream[n] = "armed"
  /\ undelivered[n]
  /\ undelivered' = [undelivered EXCEPT ![n] = FALSE]
  /\ loadFenced' = Moved(EventFenced(n), loadFenced)
  /\ refreshFenced' = Moved(EventFenced(n), refreshFenced)
  /\ pending' = [pending EXCEPT ![n] = @ \/ EventQueues]
  /\ Reason(n)
  /\ UNCHANGED <<db, drops, stream, waiting, duplicate, store, load, loadRead,
                 renews, outcome, refresh, refreshRead, refreshes, refreshReads>>

\* A drop loses undelivered events and aborts the node's load. Without
\* `ArmFirst` the reconnect snapshot is queued before the streams arm.
Drop(n) ==
  /\ stream[n] = "armed"
  /\ drops[n] < MaxDrops
  /\ stream' = [stream EXCEPT ![n] = "arming"]
  /\ drops' = [drops EXCEPT ![n] = @ + 1]
  /\ undelivered' = [undelivered EXCEPT ![n] = FALSE]
  /\ load' = [load EXCEPT ![n] = "idle"]
  /\ renews' = [renews EXCEPT ![n] = 0]
  /\ loadFenced' = Moved(EventFenced(n), loadFenced)
  /\ refreshFenced' = Moved(EventFenced(n), refreshFenced)
  /\ pending' = [pending EXCEPT ![n] = @ \/ ~ArmFirst]
  /\ waiting' = [waiting EXCEPT ![n] = FALSE]
  /\ Reason(n)
  /\ UNCHANGED <<db, duplicate, store, loadRead, outcome, refresh, refreshRead,
                 refreshes, refreshReads>>

\* Every root armed: `SubscriptionsReady` queues the snapshot. Without
\* `ArmFirst` the snapshot was queued at the drop instead.
Arm(n) ==
  /\ stream[n] = "arming"
  /\ stream' = [stream EXCEPT ![n] = "armed"]
  /\ pending' = [pending EXCEPT ![n] = @ \/ ArmFirst]
  /\ Reason(n)
  /\ UNCHANGED <<db, drops, undelivered, waiting, duplicate, store>>
  /\ UNCHANGED loadVars /\ UNCHANGED refreshVars

----
\* Observer loads.

Start(n) ==
  /\ pending[n] /\ ~waiting[n] /\ load[n] = "idle"
  /\ stream[n] = "armed" \/ ~ArmFirst
  /\ pending' = [pending EXCEPT ![n] = FALSE]
  /\ load' = [load EXCEPT ![n] = "captured"]
  /\ loadFenced' = [loadFenced EXCEPT ![n] = FALSE]
  /\ renews' = [renews EXCEPT ![n] = 0]
  /\ duplicate' = (duplicate \/ ~fresh[n])
  /\ fresh' = [fresh EXCEPT ![n] = FALSE]
  /\ UNCHANGED <<db, drops, undelivered, stream, waiting, store, loadRead, outcome>>
  /\ UNCHANGED refreshVars

Read(n) ==
  /\ load[n] = "captured"
  /\ load' = [load EXCEPT ![n] = "read"]
  /\ loadRead' = [loadRead EXCEPT ![n] = db[n]]
  /\ UNCHANGED <<db, drops, undelivered, stream, pending, waiting, fresh,
                 duplicate, store, loadFenced, renews, outcome>>
  /\ UNCHANGED refreshVars

\* A rejection is a reason to read: the fence moved for a newer fact.
Publish(n) ==
  /\ load[n] = "read"
  /\ load' = [load EXCEPT ![n] = "idle"]
  /\ renews' = [renews EXCEPT ![n] = 0]
  /\ IF loadFenced[n]
       THEN /\ pending' = [pending EXCEPT ![n] = TRUE]
            /\ waiting' = [waiting EXCEPT ![n] = RejectDelay]
            /\ outcome' = [outcome EXCEPT ![n] = "rejected"]
            /\ Reason(n)
            /\ UNCHANGED <<store, refreshFenced>>
       ELSE /\ store' = [store EXCEPT ![n] = loadRead[n]]
            /\ outcome' = [outcome EXCEPT ![n] = "published"]
            /\ refreshFenced' = Moved(PublicationFenced(n), refreshFenced)
            /\ UNCHANGED <<pending, waiting, fresh>>
  /\ UNCHANGED <<db, drops, undelivered, stream, duplicate, loadFenced, loadRead,
                 refresh, refreshRead, refreshes, refreshReads>>

Timer(n) ==
  /\ waiting[n]
  /\ waiting' = [waiting EXCEPT ![n] = FALSE]
  /\ UNCHANGED <<db, drops, undelivered, stream, pending, fresh, duplicate, store>>
  /\ UNCHANGED loadVars /\ UNCHANGED refreshVars

----
\* Direct refresh after a user mutation.

RefreshBegin(n) ==
  /\ refresh[n] = "idle"
  /\ refreshes[n] < MaxRefreshes
  /\ refreshes' = [refreshes EXCEPT ![n] = @ + 1]
  /\ refresh' = [refresh EXCEPT ![n] = "captured"]
  /\ refreshFenced' = [refreshFenced EXCEPT ![n] = FALSE]
  /\ refreshReads' = [refreshReads EXCEPT ![n] = 1]
  /\ UNCHANGED <<db, drops, undelivered, stream, pending, waiting, fresh,
                 duplicate, store, refreshRead>>
  /\ UNCHANGED loadVars

RefreshRead(n) ==
  /\ refresh[n] = "captured"
  /\ refresh' = [refresh EXCEPT ![n] = "read"]
  /\ refreshRead' = [refreshRead EXCEPT ![n] = db[n]]
  /\ UNCHANGED <<db, drops, undelivered, stream, pending, waiting, fresh,
                 duplicate, store, refreshFenced, refreshes, refreshReads>>
  /\ UNCHANGED loadVars

RefreshPublish(n) ==
  /\ refresh[n] = "read"
  /\ IF refreshFenced[n]
       THEN IF FallbackOnReject
              THEN /\ refresh' = [refresh EXCEPT ![n] = "captured"]
                   /\ refreshFenced' = [refreshFenced EXCEPT ![n] = FALSE]
                   /\ refreshReads' = [refreshReads EXCEPT ![n] = @ + 1]
                   /\ UNCHANGED <<pending, fresh, store, loadFenced>>
              ELSE /\ refresh' = [refresh EXCEPT ![n] = "idle"]
                   /\ pending' = [pending EXCEPT ![n] = TRUE]
                   /\ Reason(n)
                   /\ UNCHANGED <<store, loadFenced, refreshFenced, refreshReads>>
       ELSE /\ refresh' = [refresh EXCEPT ![n] = "idle"]
            /\ store' = [store EXCEPT ![n] = refreshRead[n]]
            /\ loadFenced' = Moved(PublicationFenced(n), loadFenced)
            /\ UNCHANGED <<pending, fresh, refreshFenced, refreshReads>>
  /\ UNCHANGED <<db, drops, undelivered, stream, waiting, duplicate, load,
                 loadRead, renews, outcome, refreshRead, refreshes>>

----

Environment(n) == Write(n) \/ Renew(n) \/ Drop(n) \/ RefreshBegin(n)
Observer(n) ==
  \/ Deliver(n) \/ Arm(n) \/ Start(n) \/ Read(n) \/ Publish(n) \/ Timer(n)
  \/ RefreshRead(n) \/ RefreshPublish(n)

Next == \E n \in Nodes : Environment(n) \/ Observer(n)

Fairness ==
  \A n \in Nodes :
    /\ WF_vars(Deliver(n)) /\ WF_vars(Arm(n)) /\ WF_vars(Start(n))
    /\ WF_vars(Read(n)) /\ WF_vars(Publish(n)) /\ WF_vars(Timer(n))
    /\ WF_vars(RefreshRead(n)) /\ WF_vars(RefreshPublish(n))

Spec == Init /\ [][Next]_vars /\ Fairness

----

TypeOK ==
  /\ db \in [Nodes -> 0..MaxWrites]
  /\ store \in [Nodes -> 0..MaxWrites]
  /\ stream \in [Nodes -> {"arming", "armed"}]
  /\ load \in [Nodes -> {"idle", "captured", "read"}]
  /\ refresh \in [Nodes -> {"idle", "captured", "read"}]
  /\ pending \in [Nodes -> BOOLEAN]
  /\ waiting \in [Nodes -> BOOLEAN]
  /\ renews \in [Nodes -> 0..K]
  /\ outcome \in [Nodes -> {"none", "published", "rejected"}]

\* NoLostWrite: once a node's stream is armed, a write the desktop does not
\* show is owned by an undelivered event, a queued load, a load or refresh
\* that can still see it (not read yet, read it, or will be rejected), or the
\* re-arm of a dropped stream, which queues a snapshot.
NoLostWrite ==
  \A n \in Nodes :
    store[n] < db[n] =>
      \/ undelivered[n]
      \/ pending[n]
      \/ stream[n] = "arming"
      \/ load[n] = "captured"
      \/ load[n] = "read" /\ (loadRead[n] = db[n] \/ loadFenced[n])
      \/ refresh[n] = "captured"
      \/ refresh[n] = "read" /\ (refreshRead[n] = db[n] \/ refreshFenced[n])

\* NoStaleCommit: a publication never replaces a newer published version.
NoStaleCommit == [][\A n \in Nodes : store'[n] >= store[n]]_vars

\* NoDuplicateHostedRead: every observer load has its own reason to read, and
\* a user mutation reads its node once.
NoDuplicateHostedRead ==
  /\ ~duplicate
  /\ \A n \in Nodes : refreshReads[n] <= 1

\* EventualPublication: an observed node keeps publishing snapshots.
EventualPublication == \A n \in LiveNodes : []<>(outcome[n] = "published")

\* Convergence: the shown version eventually stays the runtime's version.
Convergence == \A n \in Nodes : <>[](store[n] = db[n])

====
