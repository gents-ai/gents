---- MODULE ClientRequestCredentials ----
\* lean: Identity.ClientDelegation.credentialForActor
\* crates: gents-store
\* models: Identity.ClientDelegation.credentialForActor
EXTENDS Naturals, LeanDecision_Identity_ClientDelegation

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
  LET candidate == IF ScopePerRequest THEN bound[r] ELSE ambient
  IN IF supplied[r] = "none" /\ ~ExplicitEmpty THEN ambient
     ELSE IF credentialForActor(supplied[r] # "none", candidate = r) THEN candidate ELSE "none"

Read(r) ==
  /\ r \in active
  /\ observed' = [observed EXCEPT ![r] = Credential(r)]
  /\ active' = active \ {r}
  /\ UNCHANGED <<supplied, bound, ambient>>

Next == \E r \in Requests : Begin(r) \/ BeginAnonymous(r) \/ Read(r)
Spec == Init /\ [][Next]_vars
RequestBound == \A r \in Requests : observed[r] = "unseen" \/ observed[r] = supplied[r]
====
