# Harbor MaxTurn verifier scoring (issue #1019)

## Problem

`scripts/harbor/run_gents.sh` exits non-zero for every terminal Gents error
response. `scripts/harbor/gents_agent.py` turns that into a `RuntimeError`, so
Harbor records an agent exception and never runs the verifier — even when the
terminal condition is the configured turn ceiling and the task filesystem holds
real work. In PR #988's Terminal-Bench 2.1 run, `gcode-to-text` and
`make-doom-for-mips` hit `MaxTurnError` at 250 turns and were counted as
infrastructure exceptions instead of verifier-scored attempts.

## Decision

Classify in the Harbor adapter only. No runtime, schema, or Lean changes: the
issue scopes the work to the harness, and a substring that only the max-turn
path can produce gives a safe default — anything unrecognized stays a hard
agent exception.

The discriminator: budget exhaustion is raised exclusively in
`agent/loop_stream.rs` as `StreamingError::Prompt(PromptError::MaxTurnsError)`
and persisted by `agent/daemon/inference.rs` as
`agent stream failed: PromptError: MaxTurnError: (reached max turn limit: N)`.
The classifier anchors on the full quoted key-plus-prefix
(`"error_message": "agent stream failed: PromptError: MaxTurnError: `) rather
than the bare token: a provider error can embed upstream text that mentions
`MaxTurnError:`, and JSON escaping guarantees the quoted key sequence cannot
occur inside a string value. The runtime max-turns test pins the rendered
prefix so a rig wording change breaks CI instead of silently reverting the
classification.

## Design

### run_gents.sh

- New `classify_response <response.json>` function. It reads the
  `"error_message"` line and the `"status"` line and prints one of:
  - `completed` — status `complete`/`completed`
  - `max_turns_exhausted` — status `error` and the error_message line contains
    `MaxTurnError:`
  - `agent_error` — status `error` without the token
  - `unexpected:<status>` — anything else (missing status included)
- The main flow replaces the current status `case` with the classifier:
  - `completed` and `max_turns_exhausted` exit 0. The max-turn case logs the
    terminal error and turn limit to stderr for the trial log.
  - Everything else keeps today's behavior: diagnostic to stderr, exit 1.
- In every classified case the runner writes `/logs/agent/gents-outcome.json`
  before exiting: `outcome`, `response_status`, `max_turns` (the configured
  `GENTS_MAX_TURNS`), and `request_id`. All values are shell-controlled, so no
  JSON escaping hazard. `response.json` and `trajectory.json` are already
  written before classification and remain the artifacts of record for the
  terminal error text.
- New `self-test` mode (`run_gents.sh self-test`), dispatched before the
  required-environment expansions so it needs no configuration. It builds
  fixture response files — complete, completed, MaxTurn error, provider
  (`CompletionError`) error, compaction error, unexpected status, missing
  status — and asserts the classifier's output for each. Follows the
  `scripts/rename-to-gents.sh self-test` precedent.

### gents_agent.py

- `run()` control flow is unchanged: exit 0 proceeds (Harbor then verifies),
  non-zero still raises through `_require_success`.
- `populate_context_post_run` additionally reads `gents-outcome.json` and
  `response.json` from `self.logs_dir` and stamps into
  `context.metadata["gents"]`:
  - `outcome` (string from the outcome file)
  - `budget_exhausted` (true iff outcome is `max_turns_exhausted`)
  - `terminal_error` (the response's `error_message`, when present)
  Missing or malformed files degrade to warnings, matching the existing
  trajectory handling.

### CI

`rename-guard` (ubuntu-latest) gains two steps mirroring the rename-tool
pattern: `bash -n scripts/harbor/run_gents.sh` and
`scripts/harbor/run_gents.sh self-test`.

## Acceptance mapping

- MaxTurn → completed agent phase with budget-exhaustion metadata: classifier
  exit 0 + `outcome`/`budget_exhausted`/`terminal_error` stamps.
- Verifier runs and records workspace reward: automatic once the agent command
  exits 0.
- Artifacts retain terminal error and turn limit: `response.json` /
  `trajectory.json` untouched; `gents-outcome.json` records `max_turns`.
- Infrastructure failures still fail the phase: only the `MaxTurnError:` token
  reclassifies; provider/compaction/unexpected all exit 1.
- Tests cover complete, MaxTurn, and provider/compaction error responses: the
  fixture-driven self-test, run in CI.
