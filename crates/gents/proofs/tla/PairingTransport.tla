---- MODULE PairingTransport ----
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
  Dialable,
  ReplicatorInstallable

ASSUME DialableIsBool == Dialable \in BOOLEAN
ASSUME ReplicatorInstallableIsBool == ReplicatorInstallable \in BOOLEAN

VARIABLES
  connState,
  subscribed,
  replicatorInstalled,
  docsReplicated

vars == <<connState, subscribed, replicatorInstalled, docsReplicated>>

ConnStates == {"Disconnected", "Connecting", "Connected", "Failed"}

TypeOK ==
  /\ connState \in ConnStates
  /\ subscribed \in BOOLEAN
  /\ replicatorInstalled \in BOOLEAN
  /\ docsReplicated \in BOOLEAN

Init ==
  /\ connState = "Disconnected"
  /\ subscribed = FALSE
  /\ replicatorInstalled = FALSE
  /\ docsReplicated = FALSE

Dial ==
  /\ connState = "Disconnected"
  /\ connState' = "Connecting"
  /\ UNCHANGED <<subscribed, replicatorInstalled, docsReplicated>>

DialSucceed ==
  /\ connState = "Connecting"
  /\ Dialable
  /\ connState' = "Connected"
  /\ UNCHANGED <<subscribed, replicatorInstalled, docsReplicated>>

DialFail ==
  /\ connState = "Connecting"
  /\ connState' = "Failed"
  /\ UNCHANGED <<subscribed, replicatorInstalled, docsReplicated>>

Redial ==
  /\ connState = "Failed"
  /\ connState' = "Connecting"
  /\ UNCHANGED <<subscribed, replicatorInstalled, docsReplicated>>

Subscribe ==
  /\ connState = "Connected"
  /\ ~subscribed
  /\ subscribed' = TRUE
  /\ UNCHANGED <<connState, replicatorInstalled, docsReplicated>>

InstallReplicator ==
  /\ connState = "Connected"
  /\ subscribed
  /\ ReplicatorInstallable
  /\ ~replicatorInstalled
  /\ replicatorInstalled' = TRUE
  /\ UNCHANGED <<connState, subscribed, docsReplicated>>

Replicate ==
  /\ connState = "Connected"
  /\ replicatorInstalled
  /\ ~docsReplicated
  /\ docsReplicated' = TRUE
  /\ UNCHANGED <<connState, subscribed, replicatorInstalled>>

Next ==
  \/ Dial
  \/ DialSucceed
  \/ DialFail
  \/ Redial
  \/ Subscribe
  \/ InstallReplicator
  \/ Replicate

Fairness ==
  /\ SF_vars(Dial)
  /\ SF_vars(Redial)
  /\ SF_vars(DialSucceed)
  /\ WF_vars(Subscribe)
  /\ WF_vars(InstallReplicator)
  /\ WF_vars(Replicate)

Spec == Init /\ [][Next]_vars /\ Fairness

ReplicatorLiveness == <>replicatorInstalled
EndToEndLiveness == <>docsReplicated

PartialApplyHasProgress ==
  (connState = "Connected" /\ subscribed /\ ~replicatorInstalled)
    => ENABLED InstallReplicator

ReplicationImpliesReplicator ==
  docsReplicated => replicatorInstalled
====
