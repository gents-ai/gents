# Onboarding behavioral acceptance

These cases extend the existing `e2e_configurator` target. They do not define a
second runner or report format. Default tests validate fixture bounds, canonical
inference document shapes, pending prerequisites, sampling, and prompt authority.
The ignored `live_onboarding_behavioral_acceptance` test runs one serial retained
diagnostic against an explicitly selected D4F endpoint.

## Coverage

| Case | Automated status | Evidence |
| --- | --- | --- |
| Fresh user configures one coding behavior and performs a harmless task | Live | Canonical Behavior/Context/Tools queries, exact files, completed shell tool call |
| Setup re-entry twice preserves the authored prompt and creates no duplicates | Live | Full selected-config snapshot equality after each pass |
| Conflicting user/project preferences require a choice; disabled remote tool stays inert | Live with supplied inventory fixture | Snapshot equality and zero remote grants |
| Root outside published authority is rejected, then the next request recovers within the allowed root | Live | Rejected tool outcome, unchanged snapshot, recovered canonical behavior |
| Change default, restart runtime, and use a fresh session | Live | Principal default, post-restart AgentSession behavior, exact task artifact |
| Multiple providers use canonical backend/profile documents | Deterministic fixture | Rust deserialization; zero credential documents; OAuth is shape-only |
| Sampling is temperature 1 and top-p 0.95 | Deterministic and live | Canonical InferenceSampling query plus retained run settings |

`pending_cases.json` records discovery and native-login cases that cannot pass on
the baseline. Each row is explicitly `pending` and names its capability
prerequisite. The discovery inventory is test input only; these tests do not
implement or claim a production scanner.

## Live command

Run one trial at concurrency one from the repository root:

```sh
GENTS_LIVE_ONBOARDING=1 \
GENTS_D4F_ENDPOINT=http://workstation-2:8000/v1 \
GENTS_D4F_MODEL=GLM-5.3-Flash-NVFP4 \
GENTS_LIVE_CONFIG_STAGE_TIMEOUT_SECS=1800 \
GENTS_EVAL_ROOT="$PWD/.gents-eval" \
cargo test -p gents --test e2e_configurator \
  configurator::onboarding_scenarios::live_onboarding_behavioral_acceptance \
  -- --ignored --exact --nocapture
```

The test creates a unique retained directory below `GENTS_EVAL_ROOT`. It records
the effective endpoint, model, sampling values, concurrency and stage budget in
`run-settings.json`. Prompts, request observations, inference diagnostics and
tool outcomes are retained under `evidence/`, outside the model-writable
`workspace/agent-root`. The synthetic foreign source contains a fake secret
sentinel; acceptance rejects disclosure and never reads personal agent homes.

## Manual acceptance checklist

Use a fresh synthetic home and an explicit runtime root. Do not point Setup or a
working behavior at a personal home.

1. Start Gents with the synthetic root published as its only workspace root and
   with one existing inference backend. Create `onboarding-high`,
   `onboarding-medium`, and `onboarding-low` profiles referencing a sampling
   document with temperature `1` and top-p `0.95`.
2. In Setup, submit `fresh_setup.md` after replacing `{{USER_HOME}}`. Confirm the
   preview, then query AgentBehavior, AgentContext, Tools, AgentPrincipal,
   InferenceBackend, InferenceProfile, InferenceSampling and OAuthCredential.
   Verify one `Onboarding Builder`, explicit write root, preserved Setup, one
   backend, and zero credentials or unrelated grants.
3. Start a fresh `Onboarding Builder` session and submit `harmless_task.md`.
   Inspect the completed shell tool outcome and verify the two exact files.
4. Submit `reentry.md` twice through Setup. Re-query the canonical documents and
   confirm the entire selected configuration is unchanged, including the prompt
   marker and document counts.
5. Supply `discovery_conflict.json` to `conflict.md`. Confirm Setup asks for the
   unresolved scope choice and performs no write. Confirm the disabled MCP item
   produced no service or Tools grant.
6. Replace `{{FORBIDDEN_ROOT}}` in `rejected_authority.md` with an existing path
   outside the published root. Inspect the failed preview/tool result and confirm
   no `Forbidden Root Builder` or other document was created.
7. On the next request, submit `recovery.md` with the allowed root. Confirm one
   `Recovered Builder`, while the previous builder, default and Setup remain.
8. Submit `change_default.md`, restart the runtime without changing its database,
   and open a fresh session using the principal default. Submit
   `after_restart.md`; verify its exact file and that AgentSession.behavior_id is
   the recovered behavior.
9. Re-query final state. Confirm Setup and both working behaviors remain, user
   edits survive, and there are zero unexpected credentials, backends, remote
   services, datastore surfaces, subagent grants, or duplicates.

The assistant's prose is supporting evidence only. Acceptance depends on the
canonical queries, tool outcomes, exact artifacts and fresh-session selection.
