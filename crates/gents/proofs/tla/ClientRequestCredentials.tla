---- MODULE ClientRequestCredentials ----
\* lean: none
\* crates: gents-store
\* models: Identity.ClientDelegation.credentialForActor
EXTENDS Naturals

CONSTANTS ScopePerRequest, ExplicitEmpty
Requests == {"first", "second"}
VARIABLES supplied, bound, active, ambient, observed
vars == <<supplied, bound, active, ambient, observed>>

Init ==
  /\ supplied = [r \in Requests |-> "absent"]
  /\ bound = [r \in Requests |-> "none"]
  /\ active = {}
  /\ ambient = "none"
  /\ observed = [r \in Requests |-> "unseen"]

Begin(r) ==
  /\ supplied[r] = "absent"
  /\ supplied' = [supplied EXCEPT ![r] = r]
  /\ bound' = [bound EXCEPT ![r] = r]
  /\ active' = active \cup {r}
  /\ ambient' = r
  /\ UNCHANGED observed

BeginAnonymous(r) ==
  /\ supplied[r] = "absent"
  /\ supplied' = [supplied EXCEPT ![r] = "none"]
  /\ bound' = [bound EXCEPT ![r] = "none"]
  /\ active' = active \cup {r}
  /\ UNCHANGED <<ambient, observed>>

Credential(r) ==
  IF supplied[r] = "none"
    THEN IF ExplicitEmpty THEN "none" ELSE ambient
    ELSE IF ScopePerRequest THEN bound[r] ELSE ambient

Read(r) ==
  /\ r \in active
  /\ observed' = [observed EXCEPT ![r] = Credential(r)]
  /\ active' = active \ {r}
  /\ UNCHANGED <<supplied, bound, ambient>>

Next == \E r \in Requests : Begin(r) \/ BeginAnonymous(r) \/ Read(r)
Spec == Init /\ [][Next]_vars
RequestBound == \A r \in Requests : observed[r] = "unseen" \/ observed[r] = supplied[r]
====
