---- MODULE MCReplicatedRequestConvergencePeerClaim ----
EXTENDS ReplicatedRequestConvergence

\* Constants are bound via .cfg (AllowPeerClaim = TRUE — the adversarial
\* peer-claim action is armed). Diagnostic: a peer transitions itself into
\* Claimed, reachably VIOLATING SingleClaimer. This proves SingleClaimer
\* clause (1) is falsifiable — its green result in the real specs is evidence
\* the agent_did watcher fence holds, not a type artifact. EXPECTED to report
\* an INVARIANT SingleClaimer violation.
====
