---- MODULE ClientAccessRecovery ----
\* lean: ClientRuntimeAccess.admitRequest
\* crates: gents-store, gents-desktop-bridge, gents-host, gents-protocol
EXTENDS Naturals, LeanDecision_ClientRuntimeAccess
CONSTANT RenewExpired
VARIABLES phase, local, retained, expired, response
vars == <<phase, local, retained, expired, response>>
Init == /\ phase = "initial" /\ local = "none"
        /\ retained = {} /\ expired = FALSE /\ response = "none"
Persist == /\ phase = "initial" /\ phase' = "prepared" /\ local' = "old"
           /\ UNCHANGED <<retained, expired, response>>
ArriveBeforeCrash ==
    /\ phase = "prepared" /\ ~expired /\ "old" \notin retained
    /\ retained' = retained \cup {"old"}
    /\ UNCHANGED <<phase, local, expired, response>>
Crash == /\ phase = "prepared" /\ phase' = "crashed"
         /\ UNCHANGED <<local, retained, expired, response>>
Expire == /\ local # "none" /\ ~expired /\ expired' = TRUE
          /\ UNCHANGED <<phase, local, retained, response>>
Restart == /\ phase = "crashed" /\ phase' = "send"
           /\ UNCHANGED <<local, retained, expired, response>>
Send == /\ phase = "send" /\ phase' = "receive"
        /\ UNCHANGED <<local, retained, expired, response>>
Receive ==
    /\ phase = "receive" /\ phase' = "responded"
    /\ response' = admitRequest(TRUE, TRUE, TRUE, local \in retained,
                                local \in retained, local = "new" \/ ~expired,
                                local = "old" /\ expired)
    /\ retained' = IF response' = "accepted" THEN retained \cup {local} ELSE retained
    /\ UNCHANGED <<local, expired>>
Reconcile ==
    /\ phase = "responded"
    /\ phase' = IF response = "expired" THEN "send" ELSE "done"
    /\ local' = IF response = "expired" /\ RenewExpired THEN "new" ELSE local
    /\ UNCHANGED <<retained, expired, response>>
Next == Persist \/ ArriveBeforeCrash \/ Crash \/ Expire \/ Restart \/ Send \/ Receive \/ Reconcile
Spec == Init /\ [][Next]_vars /\ WF_vars(Next)
TypeOK == /\ phase \in {"initial", "prepared", "crashed", "send", "receive", "responded", "done"}
          /\ local \in {"none", "old", "new"} /\ retained \subseteq {"old", "new"}
          /\ expired \in BOOLEAN /\ response \in {"none", "accepted", "expired", "refused"}
NeverReplaceRetained == local = "new" => "old" \notin retained
Recovers == <> (phase = "done")
====
