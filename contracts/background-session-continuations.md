# Background controls in client-owned sessions

Runtime local-control requests (background completion, steering, and Goal
continuation) retain the runtime DID as requester and admission signer. They must
not create a second AgentSession when their enrolled parent belongs to a desktop
requester. Claim projection follows authenticated physical local-control edges
inside its existing transaction until it reaches that session's requester-signed
ancestor. Every edge retains agent, session and behavior; forged, foreign and
cyclic ancestry fails closed. Missing replicated dependencies remain retryable.

This attachment does not rewrite request identity, relax ACP, reopen a closed
session, or replace its exact-requester latest user observation. The existing
all-requester background observation remains separate. Deterministic invalid
bindings use the existing admission rejection owner to persist failed/error
rather than retrying a duplicate session creation forever.

The AgentSession Lean model's `preserveControlSession` contract preserves the
entire document. Its witnesses are exercised by the database control regression
for background, steering and Goal producers, recursive ancestry, signature
tampering, foreign ancestry, durable rejection and a subsequent desktop claim.

Explicit parent interruption also reaches native `spawn_process` workers in the
existing background registry. Selection is exact physical parent and agent,
background/native/running/cascade only. The existing tool cancellation owner
persists Interrupted before cancelling the process token; detached and unrelated
workers remain untouched. Normal parent completion still permits background
work to continue. The regression launches a real managed subprocess and checks
that cancellation reaps it.

Remaining boundary: ProcessControlScope still enforces exact requester scope.
A runtime-signed continuation cannot directly read/cancel a desktop-owned
process through those model-facing tools merely because its claim attaches to
the same session. Completion notifications carry results; extending process
controls requires reusing authenticated ancestry at that existing authority
boundary, not substituting requester identities. This change does not claim
that broader process-control behavior is implemented.
