---- MODULE EventSourceWake ----
\* lean: none
\* crates: gents-automation
\* models: EventDelivery.Wake.action, EventDelivery.Durable.pending, EventDelivery.Durable.checkpointAccepted, EventDelivery.Durable.triggerAdmit
(***************************************************************************)
(* An event Trigger's source documents arrive while the database's event    *)
(* transport loses, delays and redelivers their change notifications.      *)
(* Actors and their atomic steps, from the code:                            *)
(*                                                                          *)
(* Writer: commits a source document, which appends its arrival to the     *)
(* collection's journal and hands the transport one notification.          *)
(*                                                                          *)
(* Transport (DefraDB's update subscription): delivers a notification as a  *)
(* wake, drops it, or redelivers it. No fairness: it may hold a            *)
(* notification forever.                                                    *)
(*                                                                          *)
(* Tick (`EventSource::rescan_tick`, every `EVENT_SOURCE_RESCAN_INTERVAL`): *)
(* wakes the source. `Rescan` removes it.                                   *)
(*                                                                          *)
(* Source (`triggers/engine/event_source{,/durable}.rs`): on any wake       *)
(* (`EventDelivery.Wake.action`: a notification names no work) it reads     *)
(* the journal after its durable cursor (`Durable.pending`) and offers the  *)
(* first pending arrival, holding it until the engine answers. An answered  *)
(* offer checkpoints the cursor past the arrival                            *)
(* (`Durable.checkpointAccepted`) and scans again; a lost answer (the       *)
(* driver died after admission) leaves the arrival pending.                 *)
(*                                                                          *)
(* Engine (`triggers/engine/dispatch.rs`): admits the offer. An existing    *)
(* receipt for the arrival's fire identity answers duplicate                *)
(* (`Durable.triggerAdmit`); otherwise it commits the receipt and the       *)
(* request. `Dedupe` removes the receipt check.                             *)
(*                                                                          *)
(* Safeguards judged here:                                                  *)
(*  - `Rescan`: the tick wakes the source whatever the transport did.       *)
(*  - `Dedupe`: admission refuses a second request for one arrival.         *)
(*                                                                          *)
(* `MCEventSourceWake`: both hold; every written document is checkpointed   *)
(* and admitted exactly once. `MCEventSourceWakeNoRescan`: a dropped        *)
(* notification is never recovered. `MCEventSourceWakeNoDedupe`: a lost   *)
(* answer re-offers an admitted arrival and it is admitted twice.           *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  Docs,       \* how many documents the writer commits
  MaxDup,     \* how many redeliveries the transport makes
  MaxLost,    \* how many engine answers are lost
  Rescan,     \* the periodic tick wakes the source
  Dedupe      \* admission answers duplicate for an existing receipt

Positions == 1..Docs

VARIABLES
  written,    \* arrivals 1..written are in the journal
  channel,    \* notifications the transport holds, per arrival
  dups,       \* redeliveries left
  lost,       \* lost answers left
  wake,       \* the source has a wake to serve
  cursor,     \* arrivals 1..cursor are checkpointed
  offer,      \* the arrival offered and not yet answered, or 0
  answered,   \* the engine answered the offer
  receipts,   \* arrivals with a committed receipt
  admissions  \* requests committed per arrival

vars == <<written, channel, dups, lost, wake, cursor, offer, answered, receipts, admissions>>

TypeOK ==
  /\ written \in 0..Docs
  /\ channel \in [Positions -> 0..(1 + MaxDup)]
  /\ dups \in 0..MaxDup
  /\ lost \in 0..MaxLost
  /\ wake \in BOOLEAN
  /\ cursor \in 0..Docs
  /\ offer \in 0..Docs
  /\ answered \in BOOLEAN
  /\ receipts \subseteq Positions
  /\ admissions \in [Positions -> 0..2]

Init ==
  /\ written = 0
  /\ channel = [p \in Positions |-> 0]
  /\ dups = MaxDup
  /\ lost = MaxLost
  /\ wake = TRUE
  /\ cursor = 0
  /\ offer = 0
  /\ answered = FALSE
  /\ receipts = {}
  /\ admissions = [p \in Positions |-> 0]

Write ==
  /\ written < Docs
  /\ written' = written + 1
  /\ channel' = [channel EXCEPT ![written + 1] = 1]
  /\ UNCHANGED <<dups, lost, wake, cursor, offer, answered, receipts, admissions>>

Deliver(p) ==
  /\ channel[p] > 0
  /\ channel' = [channel EXCEPT ![p] = @ - 1]
  /\ wake' = TRUE
  /\ UNCHANGED <<written, dups, lost, cursor, offer, answered, receipts, admissions>>

Drop(p) ==
  /\ channel[p] > 0
  /\ channel' = [channel EXCEPT ![p] = @ - 1]
  /\ UNCHANGED <<written, dups, lost, wake, cursor, offer, answered, receipts, admissions>>

Redeliver(p) ==
  /\ channel[p] > 0 /\ dups > 0
  /\ channel' = [channel EXCEPT ![p] = @ + 1]
  /\ dups' = dups - 1
  /\ UNCHANGED <<written, lost, wake, cursor, offer, answered, receipts, admissions>>

Tick ==
  /\ Rescan /\ ~wake
  /\ wake' = TRUE
  /\ UNCHANGED <<written, channel, dups, lost, cursor, offer, answered, receipts, admissions>>

Scan ==
  /\ wake /\ offer = 0
  /\ wake' = FALSE
  /\ offer' = IF cursor < written THEN cursor + 1 ELSE 0
  /\ UNCHANGED <<written, channel, dups, lost, cursor, answered, receipts, admissions>>

Admit ==
  /\ offer # 0 /\ ~answered
  /\ answered' = TRUE
  /\ IF Dedupe /\ offer \in receipts
       THEN UNCHANGED <<receipts, admissions>>
       ELSE /\ receipts' = receipts \cup {offer}
            /\ admissions' = [admissions EXCEPT ![offer] = @ + 1]
  /\ UNCHANGED <<written, channel, dups, lost, wake, cursor, offer>>

Checkpoint ==
  /\ answered
  /\ cursor' = offer
  /\ offer' = 0
  /\ answered' = FALSE
  /\ wake' = TRUE
  /\ UNCHANGED <<written, channel, dups, lost, receipts, admissions>>

LoseAnswer ==
  /\ answered /\ lost > 0
  /\ lost' = lost - 1
  /\ offer' = 0
  /\ answered' = FALSE
  /\ UNCHANGED <<written, channel, dups, wake, cursor, receipts, admissions>>

Next ==
  \/ Write \/ Tick \/ Scan \/ Admit \/ Checkpoint \/ LoseAnswer
  \/ \E p \in Positions : Deliver(p) \/ Drop(p) \/ Redeliver(p)

Spec == Init /\ [][Next]_vars
          /\ WF_vars(Tick) /\ WF_vars(Scan) /\ WF_vars(Admit) /\ WF_vars(Checkpoint)

AtMostOnce == \A p \in Positions : admissions[p] <= 1

CheckpointNeedsReceipt == \A p \in 1..cursor : p \in receipts

EveryArrivalDelivered ==
  \A p \in Positions : [](p <= written => <>(p <= cursor /\ admissions[p] = 1))

====
