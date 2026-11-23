---- MODULE PluginSurfacePublication ----
\* lean: none
\* crates: gents-tools, gents
\* models: ToolPolicy.PluginPublication.retry, ToolPolicy.PluginPublication.nextAttempt
EXTENDS Naturals

\* An installer in another process holds the plugin store's exclusive lock
\* while it publishes, and releases it at some step, before or after the
\* construction's deadline. A slot constructing its tool surface observes the
\* lock once per attempt; a request queued for the agent is served once the
\* slot is built, and fails only if construction fails. `remaining` is the
\* construction's original deadline in poll intervals: a busy observation
\* never extends it (Lean `nextAttempt` never passes the deadline).
\*
\* Safeguard, removed by one constant: `WaitForPublication` retries only typed
\* contention while the deadline is ahead (Lean `retry`).

CONSTANTS WaitForPublication, Budget

VARIABLES locked, remaining, constructing, ready, request, prematureFailure
vars == <<locked, remaining, constructing, ready, request, prematureFailure>>

Retry(busy, budgetLeft) == busy /\ budgetLeft

Init ==
  /\ locked = TRUE
  /\ remaining = Budget
  /\ constructing = TRUE
  /\ ready = FALSE
  /\ request = "pending"
  /\ prematureFailure = FALSE

Release ==
  /\ locked
  /\ locked' = FALSE
  /\ UNCHANGED <<remaining, constructing, ready, request, prematureFailure>>

Construct ==
  /\ constructing
  /\ IF WaitForPublication /\ Retry(locked, remaining > 0)
        THEN /\ remaining' = remaining - 1
             /\ UNCHANGED <<constructing, ready, prematureFailure>>
        ELSE /\ constructing' = FALSE
             /\ ready' = ~locked
             /\ prematureFailure' = (locked /\ remaining > 0)
             /\ UNCHANGED remaining
  /\ UNCHANGED <<locked, request>>

Route ==
  /\ request = "pending"
  /\ ~constructing
  /\ request' = IF ready THEN "completed" ELSE "failed"
  /\ UNCHANGED <<locked, remaining, constructing, ready, prematureFailure>>

Next == Release \/ Construct \/ Route
Spec == Init /\ [][Next]_vars /\ WF_vars(Release) /\ WF_vars(Construct) /\ WF_vars(Route)

\* A construction never gives up on contention its deadline still covers.
NoPrematureFailure == ~prematureFailure
\* A request queued behind construction is answered.
Settles == <>(request \in {"completed", "failed"})
====
