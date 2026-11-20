---- MODULE AuthoredOutboxDelivery ----
\* lean: ClientAuthoredOutbox.acknowledge
\* crates: gents-store, gents-desktop-bridge
EXTENDS Naturals, LeanDecision_ClientAuthoredOutbox
CONSTANTS StableIdentity, KeepJournal, RespectReceipt
VARIABLES authored, pending, online, flight, reply, nativeRows
vars == <<authored, pending, online, flight, reply, nativeRows>>
Init == /\ authored = FALSE /\ pending = FALSE /\ online = FALSE
        /\ flight = FALSE /\ reply = FALSE /\ nativeRows = 0
Author == /\ ~authored /\ authored' = TRUE /\ pending' = TRUE
          /\ UNCHANGED <<online, flight, reply, nativeRows>>
Connect == /\ online' = ~online /\ UNCHANGED <<authored, pending, flight, reply, nativeRows>>
Begin == /\ online /\ pending /\ ~flight /\ flight' = TRUE
         /\ UNCHANGED <<authored, pending, online, reply, nativeRows>>
Commit == /\ online /\ flight /\ ~reply /\ nativeRows < 2
          /\ nativeRows' = IF StableIdentity THEN 1 ELSE nativeRows + 1
          /\ reply' = TRUE /\ UNCHANGED <<authored, pending, online, flight>>
Receive(binding) ==
    /\ online /\ pending /\ (reply \/ ~binding)
    /\ LET accepted == IF RespectReceipt THEN acknowledge(binding, nativeRows > 0) ELSE TRUE
       IN pending' = IF accepted THEN FALSE ELSE pending
    /\ flight' = FALSE /\ reply' = FALSE
    /\ UNCHANGED <<authored, online, nativeRows>>
LoseReply == /\ reply /\ reply' = FALSE /\ flight' = FALSE
             /\ UNCHANGED <<authored, pending, online, nativeRows>>
Reopen == /\ authored /\ (flight \/ pending)
          /\ pending' = IF KeepJournal THEN pending ELSE FALSE
          /\ flight' = FALSE /\ reply' = FALSE
          /\ UNCHANGED <<authored, online, nativeRows>>
Next == Author \/ Connect \/ Begin \/ Commit \/ LoseReply \/ Reopen \/ (\E bound \in BOOLEAN : Receive(bound))
Spec == Init /\ [][Next]_vars
NoLostIntent == (authored /\ nativeRows = 0) => pending
CreatedOnce == nativeRows <= 1
====
