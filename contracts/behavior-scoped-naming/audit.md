# Planning audit record

32 independent requests completed on `GLM-5.3-Flash-NVFP4` at
`http://workstation-2:8000/v1` through the worktree-local Gents runtime, configured
with backend/executor concurrency 32. `audit-lanes.csv` records each request and
its initial source assignment. All 32 hydrated responses were checked for
terminal `complete` status and nonempty content.

The audit used source packets from `a36055cec` plus read-only repository tools.
The planning branch subsequently rebased to pushed coordinator `72be84b87`.
Its structured command and mailbox changes were reviewed directly and added to
the plan. `coverage.csv` was regenerated at that implementation base.

Raw prompts, responses, worker reports, launcher and logs are retained under
ignored `.gents/naming-audit/`. The local identity and runtime database are also
ignored; they are not deliverables. The orchestration binary identifies itself
as `gents 0.17.0 (9bf1490c...)`, so these runs are research, not runtime validation
of the implementation base. The plan remains documentation only.

## Findings verified against source

- Behavior-only lookup: `document_config/behavior.rs::load_agent_behavior_record`
  lacks a principal filter. Common qualified names require scoped lookup changes.
- `persona_ops::apply_persona_request` relies on request-key-derived context IDs
  for repair and copies only part of the mutable closure today.
- `pack/inference.rs` directly shares selected profiles; pack provenance explicitly
  skips inference documents. Both need changes for per-behavior copies.
- `CompactionConfig.inference_profile_id` introduces another profile branch that
  must be included in closure copying.
- `createBehavior.ts` allocates a timestamp key and reuses the first context and
  profile; `ProfilesPanel.tsx` derives sampling IDs in the UI.
- Signed behavior requests cover semantic inputs with a signature domain. New
  scope inputs, if added, must be covered and versioned deliberately.
- The Lean `siblingToolsAllowed` predicate rejects shared Context/Tools. The new
  noninterference guarantee must extend beyond that existing boundary.
- Pack distribution names, origin tags, slot sentinels, generated behavior keys,
  and user display names have distinct jobs. A global textual rename is unsafe.

## Reports required adjudication

Worker output is evidence to investigate, not a ready patch specification. Some
reports confused pack inference slots with provider concurrency accounting,
proposed rejection of user display-name capitalization, assumed clone machinery
did not exist, or suggested changing DID ownership/global uniqueness. Those
suggestions were not adopted. Some searches cited generated Lean build artifacts;
the plan cites and targets source models instead. Browser DOM IDs containing
colons are not inherently invalid; actual selectors and route encoding need tests.

The 711-file lexical inventory is deliberately marked `review-pending`: it is an
implementation coverage checklist, not a claim that every file has been reviewed
or needs modification. Source packets were bounded; indirect references and pack
assets still require the full implementation sweep described in the plan.

## Operational observations

Automatic session-title requests timed out under the initial batch and used
fallback titles; audit work still completed. Raw AgentResponse content was empty
after materialization, so reports were recovered with `gents response show`, which
hydrates the canonical stored messages. Do not treat raw empty response content
as missing work. The original concurrent CLI waiters did not all exit promptly;
the canonical hydrated responses, not waiter exit status, establish completion.

For the implementation worker harness, supply explicit session titles, retain
request IDs immediately, and collect through the existing hydrated response
owner. Validate the harness with a small batch before increasing concurrency.
