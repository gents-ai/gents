# Grok TUI port: worked run history

## Outcome

[PR #1363](https://github.com/gents-ai/gents/pull/1363) merged the consolidated
stock Grok TUI implementation as `1da8b929`. The target was an unchanged client
backed by Gents documents and runtime owners, not a second agent runtime in
the shim.

## How the pack was used

GLM-5.3 and GLM-5.3-Flash powered repeated implementation runs with isolated
workspaces, sealed review, serial integration, convergence, live probes and PR
review. These were supervised development runs. The merged result also
included human-directed and Codex fixes; it is not evidence of an entirely
unattended graph completing without intervention.

## Lessons retained

- Sealed writers cannot reopen: repairs need fresh workspaces and attempts.
- Host receipts, exact owned paths and pinned revisions carry integration proof.
- Durable goals help stages persist required outputs; they do not replace
  output contracts or verified code.
- Small slices and the combined change need different review breadth.
- Large review evidence needs complete pagination, not silent truncation.
- Background tasks, cancellation, wakeups and replay need stock-client tests.
- Long reasoning and context reductions are telemetry, not proof of failure.

## Follow-up and evidence

Run-discovered runtime work includes goal invocation chains (#1357), atomic
goal resume (#1354), exact path capabilities (#1349), and compiler-artifact
separation from sealed source (#1358). Their issues/PRs are the status authority;
this record does not claim they are all merged. Identity and the shared client
database are tracked in [#1393](https://github.com/gents-ai/gents/issues/1393).

The detailed verification chronology and reviewer evidence remain in
`../../docs/design-notes/grok-ui-hydration-audit.md` and PR #1363. Raw databases
and logs remain operator artifacts. The live probe scripts test the shipped
compatibility surface; they do not prove the original generation was correct.
