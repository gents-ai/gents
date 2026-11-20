---- MODULE OwedSettlement ----
\* lean: none
\* crates: gents-store, gents-host
\* models: Identity.OwedRegistration.handling, Identity.OwedRegistration.entryStep, Identity.OwedRegistration.continues
(***************************************************************************)
(* What a User's writes still owe the document access control of a Vera   *)
(* home, and the User's client settling it with no one asked (gents#3831). *)
(*                                                                          *)
(* Each entry is a registration or relationship change DefraDB submits    *)
(* after its write commits, under the credential the write carried. The   *)
(* provider can be out of reach for a while (`Outage`, at most            *)
(* `MaxOutages` times); a two-minute delegation lapses meanwhile and the  *)
(* entry waits for authority (`Lapse`). Only the User's client can mint a *)
(* new one. While it is online it runs one pass at a time (`Pass`): the   *)
(* runtime hands the delegation the pass presented in for each entry that *)
(* waits for authority (Lean `handling`), and the client reports each     *)
(* entry the provider will never hold (Lean `entryStep`). The client asks *)
(* again while a pass left anything to move (Lean `continues`) and after  *)
(* it comes back online. A client may go offline at most `MaxOffline`     *)
(* times. The node submits a staged entry while the provider is reachable *)
(* (`Submit`), and the provider may refuse it for good.                   *)
(*                                                                          *)
(* `HandsIn`, `Reports` and `GiveUpAfter` are the safeguards; each broken  *)
(* configuration drops one.                                                *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
  Entries,
  MaxOutages,
  MaxOffline,
  HandsIn,       \* a pass hands a fresh delegation in for an entry waiting for authority
  Reports,       \* a pass reports an entry the provider will never hold
  GiveUpAfter    \* 0: ask again while anything moves; n > 0: at most n passes per online period

VARIABLES
  entry,      \* sending | awaiting | settled | failed
  reported,   \* entries the person was told were not kept
  online,
  reachable,  \* the provider is reachable from the node
  outages,
  offlines,
  passes,     \* passes in this online period
  left        \* the last pass left something to move

vars == <<entry, reported, online, reachable, outages, offlines, passes, left>>

States == {"sending", "awaiting", "settled", "failed"}

TypeOK ==
  /\ entry \in [Entries -> States]
  /\ reported \subseteq Entries
  /\ online \in BOOLEAN
  /\ reachable \in BOOLEAN
  /\ outages \in 0..MaxOutages
  /\ offlines \in 0..MaxOffline
  /\ passes \in 0..GiveUpAfter
  /\ left \in BOOLEAN

Init ==
  /\ entry = [e \in Entries |-> "sending"]
  /\ reported = {}
  /\ online = TRUE
  /\ reachable \in BOOLEAN
  /\ outages = 0
  /\ offlines = 0
  /\ passes = 0
  /\ left = TRUE

Submit(e) ==
  /\ entry[e] = "sending"
  /\ reachable
  /\ \E outcome \in {"settled", "failed"} : entry' = [entry EXCEPT ![e] = outcome]
  /\ UNCHANGED <<reported, online, reachable, outages, offlines, passes, left>>

Lapse(e) ==
  /\ entry[e] = "sending"
  /\ ~reachable
  /\ entry' = [entry EXCEPT ![e] = "awaiting"]
  /\ UNCHANGED <<reported, online, reachable, outages, offlines, passes, left>>

Outage ==
  /\ reachable
  /\ outages < MaxOutages
  /\ reachable' = FALSE
  /\ outages' = outages + 1
  /\ UNCHANGED <<entry, reported, online, offlines, passes, left>>

Recover ==
  /\ ~reachable
  /\ reachable' = TRUE
  /\ UNCHANGED <<entry, reported, online, outages, offlines, passes, left>>

GoOffline ==
  /\ online
  /\ offlines < MaxOffline
  /\ online' = FALSE
  /\ offlines' = offlines + 1
  /\ UNCHANGED <<entry, reported, reachable, outages, passes, left>>

GoOnline ==
  /\ ~online
  /\ online' = TRUE
  /\ passes' = 0
  /\ left' = TRUE
  /\ UNCHANGED <<entry, reported, reachable, outages, offlines>>

Moves(state) == state \in {"sending", "awaiting"}

Pass ==
  /\ online
  /\ left
  /\ GiveUpAfter = 0 \/ passes < GiveUpAfter
  /\ entry' = [e \in Entries |->
                 IF HandsIn /\ entry[e] = "awaiting" THEN "sending" ELSE entry[e]]
  /\ reported' = IF Reports THEN reported \cup {e \in Entries : entry[e] = "failed"}
                 ELSE reported
  /\ left' = \E e \in Entries : Moves(entry[e])
  /\ passes' = IF GiveUpAfter = 0 THEN passes ELSE passes + 1
  /\ UNCHANGED <<online, reachable, outages, offlines>>

Next ==
  \/ \E e \in Entries : Submit(e) \/ Lapse(e)
  \/ Outage \/ Recover \/ GoOffline \/ GoOnline \/ Pass

Spec ==
  /\ Init /\ [][Next]_vars
  /\ \A e \in Entries : WF_vars(Submit(e))
  /\ WF_vars(Recover) /\ WF_vars(GoOnline) /\ WF_vars(Pass)

\* The person is told only of what the provider will never hold.
ReportsOnlyFailures == \A e \in reported : entry[e] = "failed"

\* An entry the provider holds stays held.
SettledStays == [][\A e \in Entries : entry[e] = "settled" => entry'[e] = "settled"]_vars

\* Every owed entry is eventually held by the provider or told to the person.
EveryEntryResolves == \A e \in Entries : <>(entry[e] = "settled" \/ e \in reported)

\* Nothing the provider refused is dropped silently.
NeverSilentlyLost == \A e \in Entries : [](entry[e] = "failed" => <>(e \in reported))
====
