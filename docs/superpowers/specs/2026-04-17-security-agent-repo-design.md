# security-agent repo — MVP design

**Status:** draft, not yet implemented
**Date:** 2026-04-17
**Depends on:** sourcenetwork/defra-agent#47, sourcenetwork/defra-agent#48

## Purpose

Package security task pipelines (starting with Clearwing sourcehunt) as a standalone repo consumed by `defra-agent`. The repo is a prompts-and-manifests library; defra-agent is the runtime. No new schemas, collections, or Rust code in defra-agent for MVP.

Long term, repeated patterns get lifted into first-class defra-agent features (schemas for findings, scheduled-task pipelines, fleet aggregation). MVP intentionally stops short of that to validate the shape end-to-end first.

## Non-goals

- No `SecurityFinding` schema, no new collections, no changes to the defra-agent runtime.
- No authorization / scope gate. Operator is responsible for not pointing it at things they shouldn't.
- No GitHub issue publishing. That is a planned follow-on use case, not part of MVP.
- No fleet aggregation or P2P command-node story. Future work; the repo shape should not preclude it.
- No scheduling via `ScheduledTask` documents. MVP runs are operator-triggered via a shell driver.

## First use case

Two-stage Clearwing sourcehunt: scan → triage → human-readable markdown report.

- **Stage 1 (non-agent):** invoke `clearwing sourcehunt <target>` directly as a shell command. Clearwing produces its own JSON + markdown output in a run directory.
- **Stage 2 (agent):** a defra-agent behavior (`sourcehunt-triage`) reads the Clearwing output, deduplicates, ranks, and produces a concise markdown report for a human to read.

An "agent stage 1" (e.g., LLM-driven scope selection) is possible and the repo shape supports it without restructuring, but it is not included in MVP because there is no concrete work for it to do yet. Adding it later is a prompt + behavior + tool-selection addition, not a design change.

## Dependencies on defra-agent

This spec is blocked on two loader-side changes to `defra-agent config apply`. Filed as:

- **#47** `apply: support system_prompt_file on DesiredAgentBehavior` — lets behaviors reference a markdown system prompt via a path relative to the manifest root, rather than inlining the prompt as a JSON-escaped string.
- **#48** `apply: support directory-form collections for behaviors, tool-selections, inference-backends, inference-profiles` — generalizes the existing `tool-services/` and `scheduled-tasks/` directory-form pattern to the other collections, so each document can live in its own file.

Both are small loader-only changes in `crates/defra-agent-cli/src/desired_state.rs`.

The MVP sequence is: land #47 and #48 in defra-agent → build the security-agent repo against them. Shipping the security-agent repo first with a `build-manifest.sh` workaround is rejected because the apply substrate is the explicit human-/AI-maintainability story, and a preprocessing step defeats that.

## Repo layout

```
security-agent/
  sourcehunt/                         # one use case = one manifest root = one AgentPrincipal
    agent-principal.json              # single principal document
    agent-behaviors/                  # directory form (per #48)
      sourcehunt-triage.json          # references system_prompt_file (per #47)
    tool-selections/
      files-readonly.json             # file ReadOnly; the triage behavior only reads Clearwing's output
    inference-backends/
      local-ollama.json               # or whichever backend the operator uses
    prompts/
      triage.system.md                # behavior's persistent system prompt
      triage.message.md               # per-run user message template ({{CLEARWING_REPORT_PATH}})
    scripts/
      run.sh                          # driver for this use case
    README.md                         # what this use case does, how to run
  README.md                           # repo-level index
  .env.example                        # TARGET_REPO, OUTPUT_DIR, CLEARWING_BIN, DEFRA_AGENT_BIN
```

Top-level is the use case name. Prompts and scripts live with their manifest. The apply loader ignores `prompts/` and `scripts/` because they are not in its known-path list.

One behavior for MVP (`sourcehunt-triage`). Adding use cases later (`endpoint-monitor/`, `host-audit/`, `github-issue-publisher/`) each gets its own top-level directory with the same internal shape.

### Principal and behavior

```json
// sourcehunt/agent-principal.json
{
  "agent_did": "did:defra-agent:security-sourcehunt",
  "display_name": "Security — sourcehunt",
  "default_behavior_id": "sourcehunt-triage",
  "enabled": true
}
```

```json
// sourcehunt/agent-behaviors/sourcehunt-triage.json
{
  "behavior_id": "sourcehunt-triage",
  "agent_did": "did:defra-agent:security-sourcehunt",
  "display_name": "Sourcehunt triage",
  "system_prompt_file": "prompts/triage.system.md",
  "backend_id": "local-ollama",
  "model_name": "gemma4-26b-a4b",
  "tool_selection_id": "files-readonly",
  "enabled": true
}
```

```json
// sourcehunt/tool-selections/files-readonly.json
{
  "selection_id": "files-readonly",
  "agent_did": "did:defra-agent:security-sourcehunt",
  "display_name": "Files read-only",
  "enable_file_tools": true,
  "file_tools_mode": "ReadOnly",
  "file_tool_root": null,
  "enable_bash": false,
  "bash_mode": "Disabled",
  "enable_meta_tools": true
}
```

`file_tool_root` left null at the manifest level; the operator or driver passes it at apply time if narrower scoping is wanted. The triage behavior does not need bash (no exploits, no writes, just read the Clearwing output files and write the summary via the agent's normal response path).

## Data flow

```
operator sets env: TARGET_REPO=... OUTPUT_DIR=./run CLEARWING_BIN=clearwing DEFRA_AGENT_BIN=defra-agent
  |
  v
./sourcehunt/scripts/run.sh
  |
  | (1) $DEFRA_AGENT_BIN config apply --root ./sourcehunt
  |     idempotent; first run creates principal/behaviors/selections/backends,
  |     later runs reconcile diffs (e.g., prompt edits).
  |
  | (2) $CLEARWING_BIN sourcehunt "$TARGET_REPO" --output-dir "$OUTPUT_DIR/clearwing" --depth standard
  |     Clearwing produces $OUTPUT_DIR/clearwing/sh-<id>/ with JSON + markdown.
  |     This step can take minutes to hours depending on repo size.
  |
  | (3) render prompts/triage.message.md with:
  |       {{CLEARWING_REPORT_PATH}} = $OUTPUT_DIR/clearwing/sh-<id>/findings.md
  |     write to /tmp/triage-msg.<id>
  |
  |     $DEFRA_AGENT_BIN chat
  |       --agent-did did:defra-agent:security-sourcehunt
  |       --behavior-id sourcehunt-triage
  |       --message-file /tmp/triage-msg.<id>
  |       --output-file $OUTPUT_DIR/triage.md
  |
  v
operator reads $OUTPUT_DIR/triage.md
```

The security-agent principal coexists with the operator's default principal — `config apply` on the `sourcehunt/` root creates a second principal alongside whatever already exists. `chat` selects between them via `--agent-did`.

The triage user-message template does not embed the Clearwing findings inline; it references the path on disk. The triage behavior has file-read tools enabled and reads the findings file itself. This keeps request/response documents in DefraDB small, reuses the file-tool surface the agent already has, and lets the LLM decide how much of the Clearwing output to pull into its context.

## Prompt content

### `prompts/triage.system.md` (sketch)

> You are a security triage assistant. The user will point you at a Clearwing sourcehunt findings report on the local filesystem. Read the report, deduplicate findings that describe the same underlying root cause, and produce a concise markdown report organized by evidence level (highest first: patch_validated, exploit_demonstrated, root_cause_explained, crash_reproduced, static_corroboration, suspicion). For each finding, include: one-line summary, affected file and line range, evidence level, Clearwing's reasoning, and a short "why this matters" note. Do not invent findings. Do not speculate beyond what the Clearwing report states.

### `prompts/triage.message.md` (template)

> The Clearwing report for this run is at: `{{CLEARWING_REPORT_PATH}}`.
>
> Read it, triage the findings per your system instructions, and write the triaged report directly as your response.

Both prompts are drafts. The MVP smoke test (see below) is the real validation gate for prompt quality.

## Testing / smoke

- `$DEFRA_AGENT_BIN config apply --root ./sourcehunt` against a fresh agent home succeeds.
- Second apply is a no-op (idempotence).
- `./sourcehunt/scripts/run.sh` against `clearwing/tests/fixtures/vuln_samples/` (or an equivalent small, intentionally-vulnerable target shipped with Clearwing) completes and produces a non-empty `triage.md`.
- Manual eyeball: the triage report is coherent, references the Clearwing findings, and does not hallucinate evidence levels.
- Longer smoke: one real small repo chosen by the operator.

No unit tests in the security-agent repo for MVP. The driver is a short shell script; the end-to-end smoke is the real gate. If any of the prompt/manifest/driver surfaces grow past what a shell script can reasonably cover, that is the signal to add real tests.

## What ships in MVP

In the `security-agent` repo:

- `sourcehunt/` manifest root (principal, one behavior, one tool-selection, one backend, two prompts, one driver script).
- Repo README explaining prerequisites (defra-agent installed with #47/#48 landed, Clearwing installed, local backend reachable), env vars, and how to run.
- `.env.example` documenting required environment variables.

Out of MVP, planned as follow-on use cases each getting their own top-level directory:

- `github-issue-publisher/` — consumes a triage report, opens idempotent GitHub issues via the `gh` CLI.
- `endpoint-monitor/` — host-local monitoring loop.
- `host-audit/` — one-shot or scheduled host configuration audit.
- A fleet-aggregation variant once first-class `SecurityFinding` documents exist in defra-agent.

## Open questions deliberately deferred

- **Where does `file_tool_root` point?** Default null (agent's configured root) is fine for MVP. If we find the triage behavior needs a narrower scope (e.g., "only the output dir"), revisit.
- **Does the triage run also want to consume Clearwing's JSON, not just markdown?** Markdown is easier for the LLM; JSON is more precise. MVP uses markdown; revisit if the LLM produces sloppy triage.
- **When do we add agent stage 1?** When we have a concrete reason (e.g., "pick subdirectories to scan based on a repo survey"). Not in MVP.
- **When do we lift to first-class `SecurityFinding` documents?** When we want GraphQL-level querying across findings (fleet aggregation) or want findings replicated over P2P to a command node. Not in MVP.

## Open issues / cross-references

- sourcenetwork/defra-agent#47 — `system_prompt_file` on DesiredAgentBehavior (required for this spec)
- sourcenetwork/defra-agent#48 — directory-form collections (required for this spec)
- sourcenetwork/defra-agent#9 — Principal/Behavior/Deployment split (orthogonal but relevant; this spec treats one principal per use case, which aligns with the direction of #9)
