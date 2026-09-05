# SPEC: Claude spike Phase 4 — Document verification

Parent: [`claude-subscription-spike.md`](./claude-subscription-spike.md)

## Goal

Prove the control-plane thesis from DefraDB evidence: gents owned the loop and
persisted the turn; Claude was only a completer.

## Non-goals

- New Claude traffic (avoid unless Phase 3 evidence is insufficient — then write-gate)
- Peer networking unless already easy in the spike home
- Billing decision

## Constraints

- Read from the spike GraphQL / spike home only
- Prefer reconstructing from Phase 3 artifacts + documents

## Acceptance criteria

- [ ] `AgentRequest` for the spike turn reaches a terminal **success** state
- [ ] `AgentMessage` rows exist for the turn (user + assistant at minimum)
- [ ] Inference audit doc present (`InferenceCall` or current equivalent) against the OpenAI-compatible backend / `claude-plan`
- [ ] No evidence of Claude built-in side effects outside `.scratch/claude-spike/workdir` (and none expected)
- [ ] No real Anthropic OAuth access token stored in DefraDB (dummy API key / no oat)
- [ ] Optional: second principal/peer can read transcript-shaped docs and cannot read seat material (skip with note if single-node spike)

## Exit

Evidence pack recorded under `.scratch/claude-spike/logs/phase4-evidence.md` (or
equivalent). Phase 5 may correlate billing.
