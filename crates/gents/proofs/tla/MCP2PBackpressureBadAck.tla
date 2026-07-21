---- MODULE MCP2PBackpressureBadAck ----
EXTENDS P2PBackpressure

\* Diagnostic: success-ack at pending-DAG capacity without registering or
\* merging the DAG. SuccessAckBacked is expected to fail.
====
