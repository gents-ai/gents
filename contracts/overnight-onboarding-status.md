# Onboarding acceptance handoff

## Durable acceptance

- Stream finalization is independently polled and still becomes visible only
  after terminal persistence. The regression exercises a consumer waiting while
  the canonical write gate is shared; it does not promise storage preemption.
- Polling availability versus elapsed timeout is modeled in Lean and consumed by
  generated conformance tests.
- Configuration panels use canonical documents and mutation owners. Guided
  controls, validation, and save/reload coverage are catalogued in
  `tools-context-ui-coverage.md`; fields listed there as JSON-only remain so.
- Setup remains a protected configurator. Working behaviors, profiles, tools,
  network narrowing, graph packs, and background processes use their existing
  runtime owners rather than parallel configuration paths.
- Desktop composer admission, selection, draft ownership, async callbacks, and
  snapshot reads have generated or deferred regression coverage. The detailed
  ownership contract is `client-composer-conformance.md`.
- Browser first-run journeys cover the supported viewports. Native tool, graph,
  background-process, context activation, interrupt, and same-session follow-up
  paths have also been exercised against isolated test homes.

These checks do not authorize changes to preserved user homes and do not imply
graph, mobile-device, account-consent, or cross-process guarantees beyond the
named tests.

## Known limits and unresolved work

- Account sign-in requires real user credentials and consent. Multiple
  simultaneous accounts per provider remain outside this scope.
- Interactive managed onboarding, native window presentation, and restart
  acceptance require an unlocked desktop session.
- A previously observed delayed composer after interruption has not been
  reproduced or root-caused. It remains tracked in
  [#1493](https://github.com/gents-ai/gents/issues/1493); passing interruption
  runs must not be reported as resolving it.
- The broad legacy live config-flow driver still requires migration. Focused
  canonical Context save/reload plus subsequent inference is accepted, but that
  does not validate every legacy route or selector.
- Durable Save does not itself prove runtime activation; acceptance must wait for
  the active/router generation and idle reconciliation.
- Desktop and browser tests do not establish physical iOS or Android acceptance.
- Proofs do not establish notification delivery, scheduler fairness, wall-clock
  recovery bounds, or ordering across independent native process lifetimes.

## Hands-on acceptance checklist

Use fresh desktop and managed-agent homes. Preserve existing homes and mark
observed results rather than the presence of controls.

- Local setup: default root, folder selection, authority presets, editable name,
  launch errors, and transition into inference.
- Remote setup: connection details, failed connection/retry, enrollment, and
  observed secure-route readiness.
- Providers: browser launch, callback branding, visible account identity,
  credential expiry, discovery, model choice, and reasoning settings for each
  supported provider; exercise local unauthenticated inference separately.
- Persistence: backend/profile/model/sampling survive navigation and restart;
  placeholder configuration never masks the Setup-needed state.
- Chat: first response, follow-up, double-submit rejection, stop, navigation
  during streaming, session reopen, draft restoration, and restart/reopen.
- Configurator: create a separate working behavior, retain Setup, choose an exact
  profile, inspect effective permissions, and run a task.
- Graph/background: install and run through native tools, continue chatting,
  inspect durable results, and cancel safely.
- Contexts/Tools: linked document selectors, subagent targets, exact remote-tool
  grants, invalid-input feedback, Save/Cancel/reload, and no implicit grants.
- Failure states: actionable error, bounded retry/recovery, and no indefinitely
  busy composer after canonical terminal observation.
