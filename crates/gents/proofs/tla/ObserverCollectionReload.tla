---- MODULE ObserverCollectionReload ----
\* lean: ClientDataPublication.reloadAcceptedSince
\* crates: gents-observe
\* models: ObserverResync.deliver, ObserverResync.start, ObserverResync.collectionStartable, ObserverResync.startCollections, ObserverResync.settleCollections
\* The desktop observer's collection reloads beside its full node reloads
\* (`observe.rs`, `observe/delivery.rs`, `observe/projection_store.rs`). One
\* node: per-node independence is `ObserverResync`'s.
\*
\* Runtime. The node's runtime commits projected writes to two collections:
\* "cfg", whose snapshot root is one self-contained view (a collection reload
\* reads exactly its rows), and "req", whose rows depend on other roots (a
\* request, session, trigger or the Node). DefraDB delivers a change event for
\* a commit only on a subscription armed before it, after the commit is
\* visible to a new read; a dropped stream loses every undelivered event.
\* `Touch` is a "cfg" rewrite with no projected change (the per-minute catalog
\* `observed_at` rewrite of an inference backend observation); `Renew` is a
\* "req" write the projection does not show (an execution-lease renewal). At
\* most `K` of each land per full load.
\*
\* Delivery (Lean `ObserverResync.deliver`). Every delivered change is a
\* request event of the node's store. A "cfg" change queues the collection for
\* its own reload; a "req" change queues the node's full reload.
\*
\* Observer. Per node at most one read is in flight (`OneReadPerNode`): the
\* full reload or one collection reload. A full reload starts when queued
\* (`ObserverResync.start`) and takes the queued collections with it: its read
\* begins after the events that queued them. A collection reload starts only
\* when no full reload is queued (`ObserverResync.collectionStartable`,
\* `startCollections`). Each read captures the store fence, reads the runtime,
\* then publishes through the fence (`ObserverResync.settleCollections`): an
\* accepted collection reload replaces only its collection's rows; a rejected
\* one queues its collections again (`RequeueRejected`); a failed one queues
\* the full reload, which owns status, back-off and refusal
\* (`FailureResyncs`).
\*
\* Fences. Every publication (full reload, collection reload, direct refresh)
\* moves the store fence; a request event does not. Both are Lean
\* `ClientDataPublication.reloadAcceptedSince`, read from the generated table;
\* `CollectionsFenced` FALSE lets a collection reload publish through a moved
\* fence.
\*
\* Direct refresh. A user mutation reads the node once and publishes every
\* collection through the fence; a rejected refresh queues the full reload.
\*
\* Each boolean constant is TRUE in the fixed design and breaks one guard.
EXTENDS Naturals, LeanDecision_ClientDataPublication

CONSTANTS
  MaxWrites,         \* projected writes per collection
  K,                 \* touches of "cfg" and renewals of "req" per full load
  MaxDrops,          \* stream drops
  MaxRefreshes,      \* user mutations that refresh the node directly
  MaxFailures,       \* collection reads that fail
  CollectionsFenced, \* a collection reload publishes only through its fence
  RequeueRejected,   \* a rejected collection reload queues its collections again
  FailureResyncs,    \* a failed collection reload queues the full reload
  OneReadPerNode     \* a collection reload never overlaps a full reload

ASSUME \A flag \in {CollectionsFenced, RequeueRejected, FailureResyncs, OneReadPerNode} :
         flag \in BOOLEAN

Collections == {"cfg", "req"}

VARIABLES
  db,            \* collection -> projected version committed on the runtime
  undelivered,   \* collection -> a committed change whose event is not delivered
  stream,        \* "arming" | "armed"
  drops,         \* stream drops so far
  touches,       \* "cfg" touches since the last full load started
  renews,        \* "req" renewals during the current full load
  failures,      \* failed collection reads so far
  pending,       \* the full reload is queued
  queued,        \* "cfg" is queued for its collection reload
  full,          \* "idle" | "captured" | "read": the full reload
  fullFenced,    \* the full reload's fence moved since its capture
  fullRead,      \* collection -> version the full reload read
  coll,          \* "idle" | "captured" | "read": the collection reload
  collFenced,    \* the collection reload's fence moved since its capture
  collRead,      \* "cfg" version the collection reload read
  refresh,       \* "idle" | "captured" | "read": the direct refresh
  refreshFenced, \* the refresh's fence moved since its capture
  refreshRead,   \* collection -> version the refresh read
  refreshes,     \* user mutations so far
  store,         \* collection -> projected version the desktop shows
  outcome        \* "none" | "published" | "rejected": the last full reload

vars == <<db, undelivered, stream, drops, touches, renews, failures, pending,
          queued, full, fullFenced, fullRead, coll, collFenced, collRead,
          refresh, refreshFenced, refreshRead, refreshes, store, outcome>>

fullVars == <<full, fullFenced, fullRead>>
collVars == <<coll, collFenced, collRead>>
refreshVars == <<refresh, refreshFenced, refreshRead, refreshes>>

\* Whether a request event, or a publication, since a capture rejects a reload.
EventMovesReloads == ~reloadAcceptedSince(FALSE, TRUE)
PublicationMovesReloads == ~reloadAcceptedSince(TRUE, FALSE)

Init ==
  /\ db = [c \in Collections |-> 0]
  /\ undelivered = [c \in Collections |-> FALSE]
  /\ stream = "arming"
  /\ drops = 0
  /\ touches = 0
  /\ renews = 0
  /\ failures = 0
  /\ pending = FALSE
  /\ queued = FALSE
  /\ full = "idle"
  /\ fullFenced = FALSE
  /\ fullRead = [c \in Collections |-> 0]
  /\ coll = "idle"
  /\ collFenced = FALSE
  /\ collRead = 0
  /\ refresh = "idle"
  /\ refreshFenced = FALSE
  /\ refreshRead = [c \in Collections |-> 0]
  /\ refreshes = 0
  /\ store = [c \in Collections |-> 0]
  /\ outcome = "none"

----
\* Runtime.

Write(c) ==
  /\ db[c] < MaxWrites
  /\ db' = [db EXCEPT ![c] = @ + 1]
  /\ undelivered' = [undelivered EXCEPT ![c] = @ \/ stream = "armed"]
  /\ UNCHANGED <<stream, drops, touches, renews, failures, pending, queued,
                 store, outcome>>
  /\ UNCHANGED fullVars /\ UNCHANGED collVars /\ UNCHANGED refreshVars

Touch ==
  /\ stream = "armed"
  /\ touches < K
  /\ touches' = touches + 1
  /\ undelivered' = [undelivered EXCEPT !["cfg"] = TRUE]
  /\ UNCHANGED <<db, stream, drops, renews, failures, pending, queued, store,
                 outcome>>
  /\ UNCHANGED fullVars /\ UNCHANGED collVars /\ UNCHANGED refreshVars

Renew ==
  /\ stream = "armed"
  /\ full # "idle"
  /\ renews < K
  /\ renews' = renews + 1
  /\ undelivered' = [undelivered EXCEPT !["req"] = TRUE]
  /\ UNCHANGED <<db, stream, drops, touches, failures, pending, queued, store,
                 outcome>>
  /\ UNCHANGED fullVars /\ UNCHANGED collVars /\ UNCHANGED refreshVars

\* A drop loses undelivered events and aborts both reads; the re-arm queues
\* the full reload. The reconnect is a request event of the store.
Drop ==
  /\ stream = "armed"
  /\ drops < MaxDrops
  /\ stream' = "arming"
  /\ drops' = drops + 1
  /\ undelivered' = [c \in Collections |-> FALSE]
  /\ full' = "idle"
  /\ coll' = "idle"
  /\ refreshFenced' = (refreshFenced \/ EventMovesReloads)
  /\ UNCHANGED <<db, touches, renews, failures, pending, queued, fullFenced,
                 fullRead, collFenced, collRead, refresh, refreshRead,
                 refreshes, store, outcome>>

RefreshBegin ==
  /\ refresh = "idle"
  /\ refreshes < MaxRefreshes
  /\ refreshes' = refreshes + 1
  /\ refresh' = "captured"
  /\ refreshFenced' = FALSE
  /\ UNCHANGED <<db, undelivered, stream, drops, touches, renews, failures,
                 pending, queued, refreshRead, store, outcome>>
  /\ UNCHANGED fullVars /\ UNCHANGED collVars

----
\* Delivery (`observe/delivery.rs`, Lean `ObserverResync.deliver`): a request
\* event of the store, then the collection's queue or the node's.

Deliver(c) ==
  /\ stream = "armed"
  /\ undelivered[c]
  /\ undelivered' = [undelivered EXCEPT ![c] = FALSE]
  /\ fullFenced' = (fullFenced \/ EventMovesReloads)
  /\ collFenced' = (collFenced \/ EventMovesReloads)
  /\ refreshFenced' = (refreshFenced \/ EventMovesReloads)
  /\ IF c = "cfg"
       THEN queued' = TRUE /\ UNCHANGED pending
       ELSE pending' = TRUE /\ UNCHANGED queued
  /\ UNCHANGED <<db, stream, drops, touches, renews, failures, full, fullRead,
                 coll, collRead, refresh, refreshRead, refreshes, store,
                 outcome>>

\* Every root armed: `SubscriptionsReady` queues the full reload.
Arm ==
  /\ stream = "arming"
  /\ stream' = "armed"
  /\ pending' = TRUE
  /\ UNCHANGED <<db, undelivered, drops, touches, renews, failures, queued,
                 store, outcome>>
  /\ UNCHANGED fullVars /\ UNCHANGED collVars /\ UNCHANGED refreshVars

----
\* Full reload (`ObserverResync.start`, `settle`).

StartFull ==
  /\ pending /\ full = "idle" /\ stream = "armed"
  /\ coll = "idle" \/ ~OneReadPerNode
  /\ pending' = FALSE
  /\ queued' = FALSE
  /\ full' = "captured"
  /\ fullFenced' = FALSE
  /\ touches' = 0
  /\ renews' = 0
  /\ UNCHANGED <<db, undelivered, stream, drops, failures, fullRead, store,
                 outcome>>
  /\ UNCHANGED collVars /\ UNCHANGED refreshVars

ReadFull ==
  /\ full = "captured"
  /\ full' = "read"
  /\ fullRead' = db
  /\ UNCHANGED <<db, undelivered, stream, drops, touches, renews, failures,
                 pending, queued, fullFenced, store, outcome>>
  /\ UNCHANGED collVars /\ UNCHANGED refreshVars

PublishFull ==
  /\ full = "read"
  /\ full' = "idle"
  /\ IF fullFenced
       THEN /\ pending' = TRUE
            /\ outcome' = "rejected"
            /\ UNCHANGED <<store, collFenced, refreshFenced>>
       ELSE /\ store' = fullRead
            /\ outcome' = "published"
            /\ collFenced' = (collFenced \/ PublicationMovesReloads)
            /\ refreshFenced' = (refreshFenced \/ PublicationMovesReloads)
            /\ UNCHANGED pending
  /\ UNCHANGED <<db, undelivered, stream, drops, touches, renews, failures,
                 queued, fullFenced, fullRead, coll, collRead, refresh,
                 refreshRead, refreshes>>

----
\* Collection reload (`ObserverResync.collectionStartable`,
\* `startCollections`, `settleCollections`).

StartColl ==
  /\ queued /\ ~pending /\ coll = "idle" /\ stream = "armed"
  /\ full = "idle" \/ ~OneReadPerNode
  /\ queued' = FALSE
  /\ coll' = "captured"
  /\ collFenced' = FALSE
  /\ UNCHANGED <<db, undelivered, stream, drops, touches, renews, failures,
                 pending, collRead, store, outcome>>
  /\ UNCHANGED fullVars /\ UNCHANGED refreshVars

ReadColl ==
  /\ coll = "captured"
  /\ coll' = "read"
  /\ collRead' = db["cfg"]
  /\ UNCHANGED <<db, undelivered, stream, drops, touches, renews, failures,
                 pending, queued, collFenced, store, outcome>>
  /\ UNCHANGED fullVars /\ UNCHANGED refreshVars

\* A transport failure or a refusal of the collection read.
FailColl ==
  /\ coll # "idle"
  /\ failures < MaxFailures
  /\ failures' = failures + 1
  /\ coll' = "idle"
  /\ pending' = (pending \/ FailureResyncs)
  /\ UNCHANGED <<db, undelivered, stream, drops, touches, renews, queued,
                 collFenced, collRead, store, outcome>>
  /\ UNCHANGED fullVars /\ UNCHANGED refreshVars

PublishColl ==
  /\ coll = "read"
  /\ coll' = "idle"
  /\ IF collFenced /\ CollectionsFenced
       THEN /\ queued' = (queued \/ RequeueRejected)
            /\ UNCHANGED <<store, fullFenced, refreshFenced>>
       ELSE /\ store' = [store EXCEPT !["cfg"] = collRead]
            /\ fullFenced' = (fullFenced \/ PublicationMovesReloads)
            /\ refreshFenced' = (refreshFenced \/ PublicationMovesReloads)
            /\ UNCHANGED queued
  /\ UNCHANGED <<db, undelivered, stream, drops, touches, renews, failures,
                 pending, full, fullRead, collFenced, collRead, refresh,
                 refreshRead, refreshes, outcome>>

----
\* Direct refresh after a user mutation.

RefreshRead ==
  /\ refresh = "captured"
  /\ refresh' = "read"
  /\ refreshRead' = db
  /\ UNCHANGED <<db, undelivered, stream, drops, touches, renews, failures,
                 pending, queued, refreshFenced, refreshes, store, outcome>>
  /\ UNCHANGED fullVars /\ UNCHANGED collVars

RefreshPublish ==
  /\ refresh = "read"
  /\ refresh' = "idle"
  /\ IF refreshFenced
       THEN /\ pending' = TRUE
            /\ UNCHANGED <<store, fullFenced, collFenced>>
       ELSE /\ store' = refreshRead
            /\ fullFenced' = (fullFenced \/ PublicationMovesReloads)
            /\ collFenced' = (collFenced \/ PublicationMovesReloads)
            /\ UNCHANGED pending
  /\ UNCHANGED <<db, undelivered, stream, drops, touches, renews, failures,
                 queued, full, fullRead, coll, collRead, refreshFenced,
                 refreshRead, refreshes, outcome>>

----

Environment ==
  \/ \E c \in Collections : Write(c)
  \/ Touch \/ Renew \/ Drop \/ RefreshBegin \/ FailColl

Observer ==
  \/ \E c \in Collections : Deliver(c)
  \/ Arm \/ StartFull \/ ReadFull \/ PublishFull
  \/ StartColl \/ ReadColl \/ PublishColl
  \/ RefreshRead \/ RefreshPublish

Next == Environment \/ Observer

Fairness ==
  /\ \A c \in Collections : WF_vars(Deliver(c))
  /\ WF_vars(Arm) /\ WF_vars(StartFull) /\ WF_vars(ReadFull) /\ WF_vars(PublishFull)
  /\ WF_vars(StartColl) /\ WF_vars(ReadColl) /\ WF_vars(PublishColl)
  /\ WF_vars(RefreshRead) /\ WF_vars(RefreshPublish)

Spec == Init /\ [][Next]_vars /\ Fairness

----

TypeOK ==
  /\ db \in [Collections -> 0..MaxWrites]
  /\ store \in [Collections -> 0..MaxWrites]
  /\ undelivered \in [Collections -> BOOLEAN]
  /\ stream \in {"arming", "armed"}
  /\ full \in {"idle", "captured", "read"}
  /\ coll \in {"idle", "captured", "read"}
  /\ refresh \in {"idle", "captured", "read"}
  /\ pending \in BOOLEAN /\ queued \in BOOLEAN
  /\ touches \in 0..K /\ renews \in 0..K
  /\ collRead \in 0..MaxWrites
  /\ outcome \in {"none", "published", "rejected"}

\* What still owns a write of `c` the desktop does not show: its undelivered
\* event, a queued full reload, a queued collection reload of "cfg", the re-arm
\* of a dropped stream, or a read that can still see it (not read yet, read
\* it, or will be rejected and queued again).
Owned(c) ==
  \/ undelivered[c]
  \/ pending
  \/ stream = "arming"
  \/ c = "cfg" /\ queued
  \/ full = "captured"
  \/ full = "read" /\ (fullRead[c] = db[c] \/ fullFenced)
  \/ c = "cfg" /\ coll = "captured"
  \/ c = "cfg" /\ coll = "read" /\ (collRead = db[c] \/ collFenced)
  \/ refresh = "captured"
  \/ refresh = "read" /\ (refreshRead[c] = db[c] \/ refreshFenced)

\* NoLostWrite: once armed, every write the desktop does not show is owned.
NoLostWrite == \A c \in Collections : store[c] < db[c] => Owned(c)

\* NoStaleCommit: no publication replaces a newer published version of any
\* collection.
NoStaleCommit == [][\A c \in Collections : store'[c] >= store[c]]_vars

\* EventualPublication: the node keeps publishing full reloads.
EventualPublication == []<>(outcome = "published")

\* Convergence: every collection the desktop shows eventually stays the
\* runtime's version.
Convergence == <>[](store = db)

====
