---- MODULE MCP2PBackpressureTimeoutStall ----
EXTENDS P2PBackpressure

\* Diagnostic: if a timed-out/nonresponsive peer does not release its push
\* worker slot, TLC can fill the one-slot semaphore with `slow` and strand a
\* healthy peer forever. HealthyPeersDeliver is expected to fail.
====
