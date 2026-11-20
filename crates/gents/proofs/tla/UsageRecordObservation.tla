---- MODULE UsageRecordObservation ----
\* lean: none
\* crates: gents-inference
(***************************************************************************)
(* A provider response's usage headers reach the runtime's `UsageCache`    *)
(* through a detached write, and a reader expects the observation.         *)
(*                                                                          *)
(* Actors. The client (`ProviderHttpClient::send_streaming`) sees each      *)
(* response's headers and calls `UsageReporter::observe`, which spawns one  *)
(* writer and returns: the response never waits on credential resolution.  *)
(* Each writer (`record_usage` -> `write`) resolves its credential row      *)
(* (`target`: one read transaction) and then records under the row's      *)
(* `_docID` (`UsageCache::record`: insert under the entries lock, then bump *)
(* the record count `subscribe_records` publishes). Between the two        *)
(* responses the caller backfills the row's `provider_account_key`         *)
(* (`upsert_oauth_credential`), an update that keeps the `_docID`. The      *)
(* reader (`usage_wiring_codex_key_backfill_lands_under_the_key`) resolves *)
(* the backend's account and loads the cache entry.                        *)
(*                                                                          *)
(* `Answered`: the reader that read after both observations were recorded  *)
(* finds usage under the backfilled key. `NoLostWakeup`: a reader waiting   *)
(* for a record is never left waiting once every writer has recorded.      *)
(*                                                                          *)
(* GREEN: the reader subscribes to the record count, marks it seen, and     *)
(* reads only once the count covers both writers, waiting on the next     *)
(* change otherwise. RED: `ReadOnce` reads as soon as the second stream     *)
(* returns (the CI failure of 2026-11-19: nothing orders the read after    *)
(* either detached write, so it finds no entry); `SubscribeAfterRead`       *)
(* opens the subscription after a read that came up short, so a record     *)
(* landing in between is never signalled.                                  *)
(*                                                                          *)
(* The product side holds in every configuration: each writer's binding is *)
(* the stable `_docID`, so the backfill cannot strand an observation.      *)
(***************************************************************************)

EXTENDS Naturals

CONSTANTS
  AwaitRecords,      \* the reader waits on the record count before it reads
  SubscribeFirst     \* the subscription opens before the reader's first check

Writers == {1, 2}

VARIABLES
  client,    \* s1 | backfill | s2 | returned
  key,       \* the row's provider_account_key: none | set
  writer,    \* per writer: idle | spawned | resolved | recorded
  bound,     \* per writer: the binding its resolve read
  cache,     \* bindings with a recorded observation
  count,     \* records published (`UsageCache::recorded`)
  reader,    \* waiting | subscribe | check | wait | answered | failed
  seen,      \* the count the reader's receiver has marked seen
  subscribed

vars == <<client, key, writer, bound, cache, count, reader, seen, subscribed>>

Doc == "doc"

TypeOK ==
  /\ client \in {"s1", "backfill", "s2", "returned"}
  /\ key \in {"none", "set"}
  /\ writer \in [Writers -> {"idle", "spawned", "resolved", "recorded"}]
  /\ bound \in [Writers -> {"none", Doc}]
  /\ cache \subseteq {Doc}
  /\ count \in 0..2
  /\ reader \in {"waiting", "subscribe", "check", "wait", "answered", "failed"}
  /\ seen \in 0..2
  /\ subscribed \in BOOLEAN

Init ==
  /\ client = "s1"
  /\ key = "none"
  /\ writer = [w \in Writers |-> "idle"]
  /\ bound = [w \in Writers |-> "none"]
  /\ cache = {}
  /\ count = 0
  /\ reader = "waiting"
  /\ seen = 0
  /\ subscribed = FALSE

\* `observe` on the first response's headers spawns writer 1 and returns.
Stream1 ==
  /\ client = "s1"
  /\ client' = "backfill"
  /\ writer' = [writer EXCEPT ![1] = "spawned"]
  /\ UNCHANGED <<key, bound, cache, count, reader, seen, subscribed>>

\* `upsert_oauth_credential` updates the row in place: same `_docID`.
Backfill ==
  /\ client = "backfill"
  /\ client' = "s2"
  /\ key' = "set"
  /\ UNCHANGED <<writer, bound, cache, count, reader, seen, subscribed>>

Stream2 ==
  /\ client = "s2"
  /\ client' = "returned"
  /\ writer' = [writer EXCEPT ![2] = "spawned"]
  /\ reader' = IF SubscribeFirst THEN "subscribe" ELSE "check"
  /\ UNCHANGED <<key, bound, cache, count, seen, subscribed>>

\* `target`: one read of the row, before or after the backfill; the binding
\* is its `_docID` either way.
Resolve(w) ==
  /\ writer[w] = "spawned"
  /\ writer' = [writer EXCEPT ![w] = "resolved"]
  /\ bound' = [bound EXCEPT ![w] = Doc]
  /\ UNCHANGED <<client, key, cache, count, reader, seen, subscribed>>

\* `UsageCache::record`: the entry, then the published count.
Record(w) ==
  /\ writer[w] = "resolved"
  /\ writer' = [writer EXCEPT ![w] = "recorded"]
  /\ cache' = cache \cup {bound[w]}
  /\ count' = count + 1
  /\ UNCHANGED <<client, key, bound, reader, seen, subscribed>>

Subscribe ==
  /\ reader = "subscribe"
  /\ reader' = IF SubscribeFirst THEN "check" ELSE "wait"
  /\ subscribed' = TRUE
  /\ seen' = count
  /\ UNCHANGED <<client, key, writer, bound, cache, count>>

\* The reader's check: with `AwaitRecords`, read once the count covers both
\* writers; otherwise read at once. The read resolves the row (bound to the
\* `_docID`) and loads its entry.
Check ==
  /\ reader = "check"
  /\ IF AwaitRecords /\ count < 2
       THEN /\ reader' = IF subscribed THEN "wait" ELSE "subscribe"
            /\ UNCHANGED <<seen, subscribed>>
       ELSE /\ reader' = IF Doc \in cache THEN "answered" ELSE "failed"
            /\ UNCHANGED <<seen, subscribed>>
  /\ UNCHANGED <<client, key, writer, bound, cache, count>>

\* `records.changed()` returns once the count moved past what was seen.
Wake ==
  /\ reader = "wait"
  /\ count > seen
  /\ reader' = "check"
  /\ seen' = count
  /\ UNCHANGED <<client, key, writer, bound, cache, count, subscribed>>

Next ==
  \/ Stream1 \/ Backfill \/ Stream2
  \/ \E w \in Writers : Resolve(w) \/ Record(w)
  \/ Subscribe \/ Check \/ Wake

Spec == Init /\ [][Next]_vars

Answered == reader # "failed"

NoLostWakeup ==
  ~(reader = "wait" /\ seen = count /\ \A w \in Writers : writer[w] = "recorded")

====
