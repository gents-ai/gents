---- MODULE MCPairingTransportUndialable ----
EXTENDS PairingTransport

\* Un-dialable address (THE BUG: a listen-form / under-specified address that
\* resolves to no reachable direct addr under no-relay/no-discovery). TLC is
\* EXPECTED to report a liveness violation on ReplicatorLiveness — the
\* counterexample is the live hang. Constants in the .cfg.
====
