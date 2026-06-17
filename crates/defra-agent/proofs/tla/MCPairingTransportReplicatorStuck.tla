---- MODULE MCPairingTransportReplicatorStuck ----
EXTENDS PairingTransport

\* MODE B/C: the ticket is dialable (connect succeeds) but the replicator
\* install can never succeed — its SEPARATE transport dial times out (MODE B)
\* or a pre-dial check fails (MODE C: collection-cid not_found / filter
\* validation). TLC is EXPECTED to reach Connected + subscribed + ~installed
\* and report it as the counterexample — the exact "subscribed collections,
\* replicator_addresses = null" durable partial row. Constants in the .cfg.
====
