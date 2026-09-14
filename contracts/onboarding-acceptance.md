# Onboarding release acceptance

Use this checklist for each integrated native build. Automated tests are a
prerequisite, not a substitute for the interactive checks below. An unchecked
item is **not yet accepted**. Record a failure as a bug with reproduction,
expected/actual results, build SHA, and non-secret runtime evidence. Rerun the
failed case after its fix; do not mark an entire area accepted from one happy path.

## Run record

- Build SHA / bridge contract:
- Date / platform / tester:
- Fresh agent home / desktop home:
- Runtime URL / agent DID:
- Tool root / process permission ceiling:
- Provider / advertised model / sampling settings:
- Automated validation evidence:
- Bugs / fixes / retest results:

Create new homes for a from-scratch run. Compatibility with homes from previous
builds is not an acceptance requirement for this development cycle; do not add
migrations or legacy paths to satisfy it. Restart tests below apply to the home
created and configured during this run. Previous demo data need not be upgraded;
leave it untouched unless cleanup is explicitly requested. Do not include
credentials, OAuth callbacks, or tokens in run records, screenshots, or bugs.

## 1. Fresh install and managed runtime

Provider regression checks for the next native run:

- [ ] Add backend fits the settings content column at desktop and narrow widths;
      it does not create a second full-window layout or clip provider controls.
- [ ] An active chat shows one compact status, not stacked streaming/waiting
      explanations. Submission errors remain visible and take precedence.
- [ ] Claude and ChatGPT callback success, cancel, and error pages use the shared
      Gents theme. Grok's device authorization page is provider-owned, not a local
      callback; verify its in-app completion state instead.
- [ ] Claude model selection exposes supported effort and correct context/output
      limits. Save/reopen preserves effort, and the Messages body contains only
      `thinking.type=adaptive` and `output_config.effort` for supported models,
      never leaked sampling or arbitrary extra parameters.

Claude defaults reference Anthropic's [model catalog](https://platform.claude.com/docs/en/api/models/list)
and [effort guidance](https://platform.claude.com/docs/en/build-with-claude/effort).
Live catalog observations on 2026-09-14 confirmed 1,000,000 context / 128,000 max
output for Fable 5.1, Opus 5, and Sonnet 5. Setup starts output at 64,000 and permits
edits up to the advertised maximum. Live OAuth-backed model discovery was checked.
A direct Sonnet 5 Messages probe with adaptive thinking and low effort returned
HTTP 200, `end_turn`, and `OK`. Full native chat acceptance after saving these
settings remains unchecked; the direct probe is not a substitute for that flow.

- [ ] Each OAuth sign-in opens exactly one browser window; the explicit fallback
      link can still reopen it. Check ChatGPT, Claude, and Grok.
- [ ] ChatGPT discovery uses client version 0.154.0 and shows the account's current
      catalog. Context defaults come from `context_window`, not the larger
      `max_context_window` override ceiling.
- [ ] Hosted providers default to 8 concurrent requests; local defaults to 1.
- [ ] Configuration → Backends → New backend uses the same provider/model flow
      as onboarding. Cancel creates no blank documents. Save creates a separate
      backend/profile without modifying existing behavior bindings or profiles.

- [ ] Start the new native binary, not only a refreshed browser bundle.
- [ ] Setup has no stale provider, identity, session, or authority selections.
- [ ] The selected Local agent card shows editable name, pre-filled user-home
      tool root with optional folder picker, and a read/write, read-only, or
      metatools-only ceiling dropdown. Root and ceiling are independent.
- [ ] One Next starts the local runtime and opens inference without separate
      naming/access/review clicks. Invalid or unavailable roots fail inline.
- [ ] Rapid directory changes cannot apply an obsolete validation result.
- [ ] Launch reaches runtime readiness and pairs in the background without a
      manual pairing/retry step. The runtime reports the selected root/ceiling.
- [ ] An occupied port is avoided; an unrelated server is not adopted.
- [ ] Cancel/back/retry retains only appropriate non-secret setup state.
- [ ] Restart preserves DID/configuration/sessions and re-establishes pairing.
- [ ] Change root/permissions after onboarding; restart shows the effective
      authority even if subsequent pairing fails, with a visible recovery path.

## 2. Provider connection and model configuration

For **each** supported provider below, test connection/login, cancel/failure,
retry, explicit model selection, supported controls/defaults, persistence after
restart, and a real response in chat. A model list alone does not prove inference.

- [ ] Fresh managed initialization leaves the placeholder backend disabled;
      an unrelated healthy service on localhost cannot complete setup.
- [ ] After background enrollment, the initialized local DID still opens setup
      if inference is unfinished, and retains managed runtime controls.
- [ ] Remote Connect expands its address field on the initial card; progress says
      Starting, not Resuming. Provider configuration stays in one expanded column.
- [ ] Selecting a model collapses search/list to the selected model and a Change
      model button. Supported temperature, top-p, and reasoning settings show
      their editable values directly, without recommendation prose. Context/output
      overrides and concurrency remain under Advanced settings.
- [ ] Codex context/effort choices, vLLM max_model_len, and OpenRouter context/output
      limits survive discovery. Unknown values are not invented; unsupported Codex
      output/sampling overrides are labelled provider-managed.
- [ ] Account readers and disconnect use the same operator endpoint as sign-in.
      Resuming setup recognizes existing agent-scoped accounts.

- [ ] Local OpenAI-compatible: endpoint and optional key; advertised model
      dropdown/search; manual entry only when discovery is unavailable.
- [ ] OpenAI/ChatGPT account sign-in: successful OAuth and real chat.
- [ ] Grok account sign-in: successful OAuth and real chat.
- [ ] Claude subscription: account login and real chat through Anthropic
      Messages HTTP; no API-key substitution or dependency on the Claude binary.
- [ ] OpenRouter: connection, advertised model selection, and real chat.
- [ ] Invalid endpoint/key, canceled login, expired authorization, unavailable
      discovery, and unavailable model produce actionable errors without losing
      the user's non-secret choices.
- [ ] Switching providers/models during discovery ignores stale responses.
- [ ] Progressive steps ask for one decision at a time; supported sampling,
      context, and thinking controls are discoverable at review time.
- [ ] Workstation fixture selects exactly `GLM-5.3-Flash-NVFP4` at
      `http://workstation-1:8000/v1`, temperature `1`, top-p `0.95` (not top-k).
- [ ] Saving adds one coherent backend/profile/default-behavior configuration
      to the serving agent. A write failure does not silently save a desktop-only
      backend or claim success.
- [ ] Saving inference waits for readiness and opens the composer directly,
      without another welcome or Start chatting confirmation screen.
- [ ] Adding local inference does not overwrite a previously connected Grok
      backend; distinct compatible endpoints retain distinct configurations.
- [ ] Credentials do not appear in snapshots, logs, errors, or persisted UI state.

## 3. Configurator and behavior creation

- [ ] Initial chat uses Setup; its prompt explains the data model and available
      configuration tools, and asks for clarification when requirements conflict.
- [ ] Ask for coding work in a chosen repository with explicit desired tools.
      The agent creates a **new** behavior and makes it the server default;
      Setup remains an available configurator behavior.
- [ ] Inspect persisted display name, description, literal system prompt,
      context, profile, tools, and default selection. Reported success matches
      effective state; starting the behavior does not repeat the setup interview.
- [ ] Starter recipes are optional editable examples. An open-ended behavior
      can be configured without selecting a recipe.
- [ ] Requested LSP/native graph grants are explicitly persisted and verified;
      prompt text alone is not treated as a grant. Configured LSP is not reported
      as healthy until a server actually activates.
- [ ] A partial multi-step configuration identifies the failed step and retries
      it without duplicating the already-created behavior.
- [ ] Editing tools cannot change protected Setup or collateral shared tools.
- [ ] A behavior cannot exceed the process permission ceiling. The agent explains
      the limit and directs the user to runtime controls when necessary.
- [ ] Test the new default behavior with a small coding task in the selected
      directory. Verify actual work and explain any unavailable capability.

## 4. Packs, graphs, and deployment

- [ ] Ask Setup to discover/inspect the bundled code-review pack before install.
- [ ] Install via the authorized pack tool on the current serving node/principal;
      verify materialized documents/assets and explicit caller admission.
- [ ] No unrelated home adoption, CLI rebuild, schema reset, or data deletion is
      used as a fallback for a missing model-facing capability.
- [ ] The coding behavior discovers the installed graph through native tools.
- [ ] Run it against an explicit repository/base/head and record the GraphRun ID.
- [ ] Status identifies queued/running/terminal state; successful execution
      exposes the actual result/triage report without endless polling.
- [ ] Cancel a graph; cancellation and terminal status are visible and durable.
- [ ] A denied caller, missing pack, bad revision, or out-of-root repository fails
      clearly rather than executing through a broader host authority.
- [ ] Restart the managed deployment: installed pack/graph configuration remains
      available, and a subsequent native graph invocation works.
- [ ] Graph-only permission does not implicitly enable pack installation or
      general self-configuration.

## 5. Chat, sessions, and background-task bug regressions

- [ ] Press Enter twice while creating a chat: one accepted submission and one
      AgentSession, not duplicate sessions/requests.
- [ ] Leave and reopen both Setup and coding sessions; transcript and behavior
      selection restore correctly, including after desktop restart.
- [ ] Session filters are visible, understandable, and resettable; filtered-out
      sessions are not mistaken for lost data.
- [ ] Streaming follows the bottom, pauses when scrolled up, and relocks when
      scrolled back to the bottom.
- [ ] Submit while another request/background operation is active; show honest
      pending/activity state and eventually process the follow-up.
- [ ] A failed send/retry presents an error and preserves the draft. A refresh
      failure after accepted submission does not resubmit/duplicate that request.
- [ ] Background completion in a desktop-created session does not create a
      second session or wedge the queue on repeated unique-constraint failures.
- [ ] Interrupt a parent with a cascade background process: reap its worker.
      Detached and unrelated work continues; normal completion allows background
      work to finish.
- [ ] Verify terminal background results and the next user message from runtime
      documents, not only optimistic UI state.

Known scope boundary to test explicitly: process read/cancel tools still enforce
their existing exact requester scope. Attaching a runtime continuation to a
desktop session does not grant it broader process-control authority. Record any
scenario needing that capability as a follow-up; do not mask it with substituted
identities. See [background continuation contract](background-session-continuations.md).

## 6. Configuration panels and paired clients

For each panel, change every editable field, save, navigate away/back, and verify
the serving runtime value. Exercise every button, invalid input, cancel, delete
confirmation, and save failure. Do not infer wiring from a control being visible.

- [ ] Agent identity/default behavior.
- [ ] Behaviors and contexts, including literal prompts/descriptions.
- [ ] Backends and profiles, including supported sampling/thinking controls.
- [ ] Native macOS titlebar drag works after the capability update/rebuild.
- [ ] Claude, Codex, and Grok show the signed-in email/provider ID (not the
      credential document ID), plus absolute and relative credential expiry.
      Claude account metadata requires a sign-in with the updated binary.
- [ ] Grok authenticated model refresh lists its catalog and agrees with runtime
      health; subscription backends do not expose the local wire-API selector.
- [ ] Profile model choices come from the selected backend catalog; reasoning
      and advanced controls are visible, descriptions collapsed, and execution
      fields show runtime defaults while remaining editable.
- Multiple OAuth accounts per provider remain unsupported: `PrincipalOAuth`
  selects a principal/provider credential, not a backend-specific account. A
  follow-up must thread an explicit selector through login, refresh, discovery,
  and inference; creating extra backend cards alone is not sufficient.
- [ ] Tools and effective authority.
- [ ] Local runtime controls and pairing status.
- [ ] Any exposed pack/graph controls; note missing UI separately from native
      model-tool support.
- [ ] Paired mobile client observes configuration and can submit/reopen sessions
      through the existing client-local request and sync owners.
- [ ] Offline/reconnect/restart recovers without manual replication workarounds.

## Release decision

- [ ] Required automated gates passed for the exact build SHA.
- [ ] Critical-path live checks above have recorded evidence.
- [ ] Every failure is fixed and retested, or explicitly documented and accepted
      as a remaining limitation; no silent credential, data-loss, or queue wedge.
- [ ] Fresh-run restart works; merged worktree handoffs archived before cleanup.
- [ ] DefraDB dependency bump remains separate from this acceptance run.
