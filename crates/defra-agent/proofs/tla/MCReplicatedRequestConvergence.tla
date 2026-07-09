---- MODULE MCReplicatedRequestConvergence ----
EXTENDS ReplicatedRequestConvergence

\* Constants are bound via .cfg (Cap = 3 — the shipping TERMINAL_REDRIVE_CAP,
\* large enough to outlast one drop + one crash: SingleClaimer holds AND
\* TerminalConverges holds).
====
