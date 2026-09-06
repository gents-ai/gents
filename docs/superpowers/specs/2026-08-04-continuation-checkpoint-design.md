# Compaction continuation checkpoints

**Date:** 2026-08-04

## Problem

The bounded compaction work in #1017 made summary generation mechanically safe,
but its three-field narrative contract does not reliably preserve the state a
coding agent needs to continue a long task. Terminal-Bench traces exposed the
cost, but the replacement must not encode those traces as universal rules. In
particular, telling a successor never to re-establish a fact is unsound: files,
services, and worktrees can change, summaries can be ambiguous, and a targeted
verification is often cheaper than acting on a stale assumption.

## Prior art

Three local agent runtimes informed this design:

- OpenAI Codex uses a deliberately small handoff prompt covering progress,
  decisions, constraints, remaining work, and critical references. Its carrier
  asks the successor to build on prior work and avoid duplication, but does not
  forbid verification.
- Grok Build records current work, errors, exact artifacts, pending tasks, and a
  direct next step. It keeps model-authored semantic memory separate from a
  deterministic reminder of live runtime state.
- oh-my-pi uses explicit goal, constraints, progress, decisions, next steps,
  and critical-context sections. Recompaction updates the previous checkpoint
  as progress moves. Its deterministic archive mode explicitly recommends
  re-reading files or rerunning commands when exact details matter rather than
  guessing.

Gents adopts oh-my-pi's compact progress shape and Grok Build's distinction
between semantic memory and structural runtime facts. It does not adopt Grok
Build's exhaustive all-message and full-code restatement, which would invite
the summary amplification #1017 removed.

## Design

### Typed semantic checkpoint

The internal structured-output completion returns a
`ContinuationCheckpoint` with:

- the current goal;
- constraints and preferences;
- completed, in-progress, and blocked work;
- the exact immediate work state;
- decisions and rationale;
- errors and fixes;
- verification results;
- uncertainties worth re-checking;
- ordered next actions; and
- critical context, including any unanswered user request.

The schema still denies unknown fields. Every list defaults empty, is prompted
to remain short, and is defensively rendered at no more than eight items. The
goal and every list item use the existing per-item byte and control-character
sanitization.

If an older checkpoint appears in source history, the prompt tells the model to
update it rather than blindly append another chronology: preserve relevant
facts, advance progress states, and remove claims proven obsolete.

### Deterministic state and recent history

Model-authored checkpoint fields never include file lists. Gents continues to
extract file activity structurally from tool calls and renders that state after
the semantic checkpoint. The pair-safe recent history tail remains verbatim.
The active request remains the owned loop's current prompt rather than being
rewritten into the checkpoint.

The rendered representation remains Markdown. Request-boundary reductions put
it in `CompactionEntry.summary`; per-turn reductions put it in the immutable
request-local `ProviderContextReduction.summary`. The render order is:

1. goal;
2. constraints and preferences;
3. progress;
4. current work;
5. decisions;
6. errors and fixes;
7. verification;
8. uncertainties;
9. ordered next actions;
10. critical context; and
11. structurally extracted files read and modified.

### Continuation guidance

Both request-boundary and per-turn compaction use one carrier function. It
gives the successor this policy:

> Treat recorded results as evidence, not as a prohibition on verification.
> Re-check facts when state may have changed, the checkpoint is ambiguous, or
> correctness depends on them. Avoid repeating completed or expensive work
> without a concrete reason.

This distinguishes useful revalidation from harmful duplication. Opening a
file before editing it, confirming mutable service state, or rerunning a
critical test can be task-effective. Redownloading unchanged data, repeating a
failed approach without new evidence, or rediscovering settled architecture is
not.

### Durable reduction identities and recovery

`CompactionEntry` and `ProviderContextReduction` are distinct facts. The former
reduces a cumulative session prefix for later request-boundary loading. The
latter replaces the sticky provider projection inside one physical
`AgentRequest`; it never advances the session prefix.

A provider-context reduction is identified by the canonical tuple `(agent DID,
session ID, request document ID, turn index, reduction index)`. Redelivery with
the same immutable payload is idempotent. Rebinding that key to a different
source boundary, split, producer call, checkpoint, or parent is an integrity
error. The split persists the exact compacted prefix and retained suffix, and
the source boundary pins the physical claim commit plus the newest canonical
`AgentMessage` document/version visible when reduction began. That bounded
high-water identity and the exact stored split avoid both a mutable message
count and an unbounded session snapshot. Claim commits are provenance of each
fact, not a request-chain invariant: a later claim may append the next reduction
under a different commit.

The owned loop may activate a checkpoint only after create-and-compare
persistence succeeds. A crash after summary completion but before the next
provider call therefore leaves a durable unconsumed checkpoint. Recovery
restores that exact checkpoint and its ordered reduction-key chain. Once a
inference-scope `RenderedRequest.AssemblyTrace` cites the latest key, recovery
deliberately derives from canonical history instead of assuming an unknown
provider outcome. Title and compaction captures never count as consumption, and
wall-clock ordering is not consulted.
Repeated reductions form a strict parent chain within one request. Forked and
concurrent requests receive distinct identities; these local audit payloads are
not copied by session fork or participant replication.

## Formal-model impact

`Proofs.Compaction.DurableReduction` models immutable create-and-compare,
persist-before-activate, exact crash restoration, ordered request-local
identity, and pair-closed send admission. The existing Compaction and
PromptAssembly proofs remain authoritative for split selection and provider
sanitization; the new model composes their output with durable ownership.

## Validation

- Unit tests fence the generated JSON schema and every rendered section.
- Prompt tests fence anti-injection rules, iterative checkpoint updates, and
  the verification-versus-duplication policy.
- Generated durable-reduction cases fence identity, idempotent redelivery,
  conflicting rebinding, pair closure, and send admission.
- Runtime tests cover multiple reductions, crash restoration, rendered-request
  consumption, request concurrency, and security catalog classification.
- The full `cargo test -p gents` and
  `cargo check --workspace --all-targets` gates must pass before merge.

## Durable goals are the terminal condition

Provider-context reduction remains a request-local crash cut. It cannot make a
new physical request after the current request terminates. Long-running work
that must survive ordinary terminal assistant turns uses a `Goal` document as
controller state instead.

Goal tools have an explicit capability independent of generic MCP
discovery/describe/call tools. An unset `enable_goal_tools` retains the legacy
behavior (it follows `enable_meta_tools`); setting it explicitly permits a
least-privilege surface such as goal get/update with generic meta tools off.
Model creation has a second, opt-in `enable_goal_creation` gate and is effective
only when both gates survive the behavior, operator-ceiling, and runtime meet.
The model never supplies a DID or session ID: the persistence hook binds create,
get, and update to the authenticated request principal and current session.

`gents chat --session-id <stable-id> --goal-objective <text>
[--goal-token-budget <positive-int>]`
atomically commits the new goal and first pending request. The request does not
become visible to watchers without its goal. Exact retries use deterministic
goal/submission keys scoped by the required stable session ID; rerun the same
command with that ID after an ambiguous transport failure. A changed immutable
objective, budget, prompt, or behavior is a conflict, and rollback never deletes
a possibly committed goal. `--goal-objective` is only for the initial atomic
submission or its exact replay; omit it when sending later turns in that goal
session.

Feature-implementation graphs compose with this interface at the existing Task
boundary. A Task may set `goal_objective_template` and an optional positive
`goal_token_budget`. The trigger engine renders that objective from the same
template scope as the request and atomically commits the creation claim, Goal,
and first request under a deterministic per-fire session/retry identity. A
retry of the same durable event or schedule fire therefore converges on the
same pair; a watcher can never observe the request without its Goal.

The stage's ordinary `ToolSelection` enables `get_goal`/`update_goal` while
leaving generic meta tools and model-facing `create_goal` disabled. The runtime
provisions the declared Goal, so granting creation would be redundant. The
stage request's DID/session owns the Goal and the existing `GoalSource` plus
goal-continuation queue supplies the durable terminal condition. Graph edges
remain document triggers; the graph DSL does not acquire or duplicate Goal or
steering lifecycle authority. `packs/pipeline` is the checked-in composition
example.
