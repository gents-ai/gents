# SPEC: Claude spike Phase 5 — Billing confirmation

Parent: [`claude-subscription-spike.md`](./claude-subscription-spike.md)

## Goal

Decide Go/No-Go: did spike traffic bill the **Claude subscription / plan meter**,
or Console API / extra-usage?

## Non-goals

- More feature work
- Packaging (Phase 6) before Go
- Assuming early-phase meter glances were conclusive

## Constraints

- Prefer correlation of **existing** Phase 1–3 timestamps vs Claude usage UI / invoices
- Any additional Claude completion requires write-gate approval
- Human owns the meter reading; agent prepares the correlation checklist

## Correlation inputs

- `.scratch/claude-spike/logs/` adapter + proxy timestamps
- Phase 3 `InferenceCall` / request timestamps
- Number of approved Claude write requests actually executed
- Claude plan usage UI / billing export (human-provided)

## Acceptance criteria

- [ ] Checklist lists every approved Claude write that ran (id, time, purpose)
- [ ] Human confirms whether usage landed on **plan** meter vs API/extra-usage
- [ ] Confirm child env did not supply `ANTHROPIC_API_KEY` / auth-token paths for those calls
- [ ] Explicit written verdict in `.scratch/claude-spike/logs/phase5-verdict.md`:
  - **Go** — subscription-as-completer looks viable; Phase 6 may be considered
  - **No-Go** — stop packaging; keep adapter/proxy notes as negative evidence
  - **Inconclusive** — what additional **approved** probe would resolve it

## Exit

Verdict file committed to spike logs (not necessarily git). Phase 6 SPEC only
on **Go**.
