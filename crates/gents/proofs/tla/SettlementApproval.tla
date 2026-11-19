---- MODULE SettlementApproval ----
\* lean: Identity.EnrollmentGrant.settlementStep, Identity.EnrollmentGrant.afterRemoval
\* crates: gents-store
\* models: Identity.EnrollmentGrant.grant, Identity.EnrollmentGrant.settle
(***************************************************************************)
(* The owner's owed-settlement pass withdrawing a member's Node            *)
(* `administrator` relation while an approval of that member runs          *)
(* (gents#3831). The withdrawal is owed (`oweRevoke`): the member holds    *)
(* the relation, a retired receipt owes its withdrawal and no live receipt *)
(* accounts for it.                                                        *)
(*                                                                          *)
(* The approval (`grant_with`) reads what the member holds and the         *)
(* receipts, writes its receipt in one transaction that also closes the    *)
(* owed receipts when it records the relation (Lean `grant`), then issues  *)
(* every tuple its receipt recorded. The pass (`settle_owed_withdrawals`)  *)
(* reads the receipts and decides (Lean `settlementStep`), removes the     *)
(* relation, reads the receipts again and decides (Lean `afterRemoval`),   *)
(* issues the relation again when told to and closes the owed receipts.    *)
(* Each step is one atomic read or write.                                  *)
(*                                                                          *)
(* `Reissues` is the safeguard: without it the pass can remove the         *)
(* relation an approval recorded.                                          *)
(***************************************************************************)
EXTENDS LeanDecision_Identity_EnrollmentGrant

CONSTANT Reissues

VARIABLES held, owed, live, approver, seen, settler, decision
vars == <<held, owed, live, approver, seen, settler, decision>>

TypeOK ==
  /\ held \in BOOLEAN /\ owed \in BOOLEAN /\ live \in BOOLEAN
  /\ approver \in {"idle", "read", "recorded", "done"}
  /\ seen \in [held : BOOLEAN, owed : BOOLEAN, live : BOOLEAN]
  /\ settler \in {"idle", "decided", "removed", "reissue", "close", "done"}

Init ==
  /\ held = TRUE /\ owed = TRUE /\ live = FALSE
  /\ approver = "idle" /\ seen = [held |-> TRUE, owed |-> TRUE, live |-> FALSE]
  /\ settler = "idle" /\ decision = "none"

ApproverRead ==
  /\ approver = "idle"
  /\ seen' = [held |-> held, owed |-> owed, live |-> live]
  /\ approver' = "read"
  /\ UNCHANGED <<held, owed, live, settler, decision>>

\* Lean `record`: the relation is recorded unless held independently.
Records == ~seen.held \/ seen.live \/ seen.owed

ApproverRecord ==
  /\ approver = "read"
  /\ IF Records THEN live' = TRUE /\ owed' = FALSE ELSE UNCHANGED <<live, owed>>
  /\ approver' = "recorded"
  /\ UNCHANGED <<held, seen, settler, decision>>

ApproverIssue ==
  /\ approver = "recorded"
  /\ IF live THEN held' = TRUE ELSE UNCHANGED held
  /\ approver' = "done"
  /\ UNCHANGED <<owed, live, seen, settler, decision>>

SettlerRead ==
  /\ settler = "idle"
  /\ decision' = settlementStep(owed, live)
  /\ settler' = "decided"
  /\ UNCHANGED <<held, owed, live, approver, seen>>

SettlerAct ==
  /\ settler = "decided"
  /\ CASE decision = "issue" -> held' = FALSE /\ owed' = owed /\ settler' = "removed"
       [] decision = "close" -> held' = held /\ owed' = FALSE /\ settler' = "done"
       [] OTHER -> held' = held /\ owed' = owed /\ settler' = "done"
  /\ UNCHANGED <<live, approver, seen, decision>>

SettlerReread ==
  /\ settler = "removed"
  /\ settler' = IF Reissues /\ afterRemoval(live) = "reissue" THEN "reissue" ELSE "close"
  /\ UNCHANGED <<held, owed, live, approver, seen, decision>>

SettlerReissue ==
  /\ settler = "reissue"
  /\ held' = TRUE /\ settler' = "close"
  /\ UNCHANGED <<owed, live, approver, seen, decision>>

SettlerClose ==
  /\ settler = "close"
  /\ owed' = FALSE /\ settler' = "done"
  /\ UNCHANGED <<held, live, approver, seen, decision>>

Next ==
  \/ ApproverRead \/ ApproverRecord \/ ApproverIssue
  \/ SettlerRead \/ SettlerAct \/ SettlerReread \/ SettlerReissue \/ SettlerClose

Spec == Init /\ [][Next]_vars

Quiet == approver \in {"idle", "done"} /\ settler = "done"

\* The relation ends held exactly when a live receipt accounts for it.
HeldExactlyWhenAccounted == Quiet => (held = live)

\* Nothing stays owed once the pass and the approval are done.
NothingStaysOwed == Quiet => ~owed
====
