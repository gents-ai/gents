import Proofs.Conformance.Triggers.Lifecycle
import Proofs.Conformance.Triggers.Materialization
import Proofs.Conformance.Triggers.Trace

/-!
# Conformance Mapping: Trigger Layer -> Request Lifecycle

Bridges the trigger-engine proof layer (`Proofs/Triggers.lean`) to the
request-lifecycle model (`Proofs/Request.lean`).

The trigger layer intentionally works with a thin request projection:

* `AgentRequest.causedBy`
* `AgentRequest.concurrency`
* `AgentRequest.isTerminal`
* `AgentRequest.executionOrigin`

This file relates that projection to the richer lifecycle model carried
by `RequestContext`. The key cross-layer relation is
`TriggerLifecycleCoherent`, which says:

* the trigger-layer terminal bit agrees with lifecycle terminality
* the trigger-layer execution origin matches the lifecycle origin

Together with `syncTriggerTerminal`, this gives a lightweight
"observational view" theorem: once a trigger-created request is related
to a lifecycle request, lifecycle transitions preserve the trigger
fields we care about.
-/
