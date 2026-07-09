---- MODULE MCP2PBackpressureGreen ----
EXTENDS P2PBackpressure

\* Healthy bounded hub: one nonresponsive peer can time out and release the
\* only push worker, so responsive peers still deliver. Inbound admission nacks
\* at capacity instead of success-acking untracked work.
====
