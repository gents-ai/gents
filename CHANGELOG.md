# Changelog

All desktop crates and npm packages release together at `workspace.package.version`
(lockstep train). The bundled frontend and Rust bridge use generated types and
source consistency checks, not a separate runtime compatibility version.

## Unreleased

### Breaking

- Tools document timeouts now take effect, and the ones that could not are
  gone (#1768). `host.bash` `timeout_secs` and `max_timeout_secs` set the
  foreground default and maximum, clamped to the host's
  `--command-timeout-secs` / `--command-timeout-max-secs`.
  `host.cli[].timeout_secs` replaces the CLI registration's timeout, clamped
  the same way. `background_timeout_secs` on `host.bash` and each remote
  service sets a `spawn_process` lifetime (at most 36,000s).
  `wait_timeout_secs` and `max_wait_timeout_secs` there set `wait_process`
  waits on that kind of handle (at most 600s). `integrations.lsp`
  `timeout_secs` and `max_timeout_secs` set LSP action timeouts (at most
  300s). Values above a ceiling are clamped, not rejected. Removed, and now
  rejected: `host.files.timeout_secs`, `built_ins.timeout_secs`,
  `datastore.timeout_secs`, `self_config.timeout_secs`,
  `subagents.wait_timeout_secs`, `subagents.max_wait_timeout_secs` and
  `integrations.lsp.rpc_timeout_secs`. Delete them from stored Tools
  documents before upgrading.
- `gents subagent list` JSON: `state` replaced by `edge_state` (null on
  root/forest rows) and `request_lifecycle_state`; table column `STATE` →
  `EDGE_STATE`/`REQUEST_STATE` (#1783).
- Reasoning retention, reasoning replay and detached title requests require a
  fresh home. Output segments keep typed reasoning, signatures and encrypted
  reasoning; `AgentRequest` (`purpose`) and `ProviderContextReduction`
  (`replay_associations_json`) have new collection baselines, and a desktop or
  runtime on the previous collections is refused as schema skew. Update desktop
  and paired runtimes together. Existing stores are not migrated (#1603).

### Added

- Tools documents can set file tool limits: `host.files.max_read_chars`
  (default 32,000 bytes, allowed 1 to 1,000,000) for `read_file`, and
  `host.files.max_list_entries` and `host.files.max_matches` (default 200,
  allowed 1 to 5,000) for `list_files`, `glob` and `grep`. Each is both the
  per-call default and the most a call can request. The desktop Tools editor
  shows them (#1764).
- `steer_subagent` can steer a background child that already finished
  (completed, failed or timed out). The steer queues a new request in the
  child's existing session, so the child continues with its transcript and
  tool results; the finished request and its bridge keep their outcome.
  `cancel_subagent` on the same child stops that work. A parent interrupt does
  not end a child's steerability. Cancelled children and children fenced by
  an unclaimed-spawn expiry still refuse steering (#1538).

### Changed

- Interrupting a thread stops only its foreground turn and in-flight
  foreground calls. Background processes and subagents, including one the
  thread was waiting on, keep running and stay attached; an awaited subagent
  becomes background work whose completion is delivered to the session.
  Subagents and background processes stop only when explicitly cancelled:
  `cancel_subagent`, and `cancel_process` or the UI's background-task stop.
  Parent interrupts, failures, completion and restart recovery no longer
  interrupt or fail child requests, including queued ones; a subagent a failed
  or restarted parent was waiting on becomes background work whose completion
  is delivered to the session. Steering a subagent with `interrupt` no longer
  cancels that subagent's own subagents (#1624).
- The model gets its own reasoning back across turns, requests and restarts.
  Each provider request replays the longest run of completed, accepted turns
  whose Claude thinking or Responses encrypted reasoning still matches the
  accepted request that produced it (same issuer and route, same system, tools
  and earlier messages), decided from the durable request captures. Compaction,
  repaired input, a tool or system change and a provider switch end the run
  instead of sending reasoning the provider would reject; an interrupted turn
  replays none. A signature or decryption rejection strips all reasoning and
  retries once (#1603, #1693).
- Received reasoning, signatures and auxiliary provider output are kept for
  audit even when a turn fails, is interrupted or hits its request deadline,
  and conversation titles run as detached title requests with their own audit
  (#1603).
- Plain `gents init` enables the Engineer's self-config tools and graph tools,
  as the desktop first run does. The tool ceiling set at init still bounds what
  they can change. `--setup-steward` now only seeds the Engineer identity, the
  write package default and deferred inference (#1874).
- `write_file` no longer replaces an existing file blindly: pass the
  `content_hash` from your latest read (rejected if the file changed since) or
  `overwrite: true`. Creating new files is unchanged (#1605).
- A configured `max_output_chars` now also bounds what an interrupted
  command's diagnostic shows and how much a background command's completion
  notice summarizes (at most 4,000 bytes) (#1770). The value is read from the
  behavior's current Tools document when the output is presented, including
  after a restart; a behavior that no longer resolves uses the default.
- `list_files`, `glob` and `grep` return a truncated result when their output
  would exceed the filesystem runner's response budget (1.5 MiB), instead of
  failing the call. Glob and grep patterns longer than 4,096 bytes are refused
  with an error instead of crashing the runner (#1764).
- The subagent tree view shows three levels below its root when no depth is
  requested, the delegation depth limit, instead of eight. Explicit depths
  and the desktop cascade-cancel preview use the descendant walk's 32-level
  bound, matching what cancellation reaches (#1764).
- The default agent turn limit is 1,000, up from 250, so long-running work is
  not cut short by the built-in default. A max-turns failure now also names
  where the limit came from — the built-in default, an `InferenceExecution`
  document, or a programmatic `BehaviorBuilder::max_turns` — so the operator
  knows which knob to turn (#1539).

- GitHub Releases attach the gents CLI archives again, with per-OS checksum
  files: Linux x86_64 and aarch64, and a signed, notarized macOS arm64 build.
- The desktop transcript shows tool commands, arguments and output as they
  ran. It no longer hides text that mentions a credential-like word (such as
  `password` or `Authorization`) behind "Command hidden" or "Hidden because it
  looks like it contains a credential"; the stored transcript was never
  redacted (#1620).
- Publishing a datastore tool surface now refuses an output obligation whose
  `expected_count_field` names a field whose type cannot carry a number or a
  numeric string at all — a boolean, a list, a relation, or a field the target
  collection does not have — instead of accepting it and leaving the request to
  fail at run time. A configuration that names such a field stops applying. The
  check runs in the shared desired-state publication owner, so it covers
  `gents config apply`, the self-config tool and pack installs. An accepted type
  is not a promise that the count arrives: a `DateTime` field takes the string
  but rejects every all-digit value (#1735).

### Fixed

- Title audits dispatch only through the runtime watcher, preventing duplicate
  claim attempts while retaining received reasoning (#1916).

- Stateless (`store:false`) Responses requests to xAI/Grok and ChatGPT Codex
  now always request `include: ["reasoning.encrypted_content"]`, even with no
  reasoning effort configured, so replayed reasoning resolves without
  server-side storage (#1862).
- Desktop onboarding explains what Gents keeps for you (a record of every step,
  so work can be reviewed and resumed) instead of saying every step is a
  document. After a subscription sign-in the next button reads "Find models"
  rather than asking to connect again, and the empty mailbox says what arrives
  there before offering to start a session (#1619).
- A running command's live output in the desktop transcript stays on its
  newest line unless you scroll up, and multi-line command output says how many
  lines it holds (#1620).
- A `spawn_process` process no longer outlives a runtime crash unnoticed, and
  `cancel_process` no longer reports `cancelled` for a process it did not
  stop. The runtime records each spawned process (pid, process group and OS
  start time) in its store directory; after a restart it stops a surviving
  process it can prove it owns, and settles the row as `failed` with reason
  `process_lost` when it cannot, instead of leaving it `running`.
  `cancel_process` returns `cancelled` only once the process is observed
  gone, `lost` when this runtime could not prove it owned the process, and
  `unverified` when the process is still running after the signal. Deleting
  the task or trigger that started a run stops that run's background
  processes (reason `task_deleted`) (#1858).
- Claude, Codex and Grok subscription usage limits now fail the turn at once
  with the provider's reset time (`provider usage limit reached (resets at
  …)`) instead of retrying and reporting an exhausted retry budget. Short
  rate limits still retry, after the provider's `Retry-After` rather than a
  fixed 60 seconds, and Goals pause as usage-limited on the same
  classification (#1422).
- A durable Goal no longer spends its infrastructure retries while its behavior
  is unavailable. Continuation, including a claimed continuation recovered
  after a restart, waits for the runtime's behavior readiness and records why
  it is waiting; a continuation rejected before execution because the behavior
  was unavailable is re-issued without a charge once readiness is republished;
  and a behavior that stays unavailable after reconciliation settles pauses the
  Goal with that reason instead of waiting forever (#1345).
- CLI integration tests recover from a port taken between allocation and the
  server's bind, instead of failing the run (#1641).
- A runtime with one permanently invalid behavior it is not using now settles
  instead of waiting forever. Reference visibility is decided by whether the
  referenced documents exist, not by whether every behavior's inference
  selection is valid; runnability is still decided separately and an invalid
  behavior stays unavailable (#1756).
- Collection introspection now names a non-nillable field `Int!` instead of
  reporting the bare `NON_NULL` wrapper kind, so `defra_query` discovery and the
  new obligation check both read the field's actual type (#1735).
- Task `prompt_template` and `goal_objective_template` are refused at configure
  time when they name a filter, test or function the template engine does not
  provide, instead of being accepted and failing on every trigger fire with
  `template render error: unknown filter`. Every name the compiled template
  uses is checked, so a conditional branch does not hide one. A method call on
  a value (`{{ doc.name.upper() }}`) resolves against that value, so it is
  still reported when the task fires (#1744).
- Task templates can use `tojson`. The template engine's `json` feature is
  enabled, so the filter the configurator already emits resolves instead of
  being rejected. It escapes `<`, `>`, `&` and `'` as `\uXXXX` sequences, which
  reach the model as written (#1744).
- A pack scenario sidecar reference can no longer resolve outside its pack
  directory: the CLI holds sidecar paths to the same canonical asset-path rule
  the pack loader uses (#1642).
- `gents subagent list --root` works while a fan-out is still running. Each row
  reports the parent bridge's `edge_state` (`running`,
  `awaiting_child_materialization`, `pending_child_authorization`, ...) apart
  from the child request's own `request_lifecycle_state`, instead of failing
  to decode the edge as a request (#1783).
- `last_progress_age_ms` no longer reports a working request as stalled
  between tool batches. Progress is the newest of the claim, any tool call's
  start or completion, and any inference call's start or end for that request,
  so the age no longer jumps back to `claimed_at` when a tool call finishes
  (#1782).
- The agent loop stops re-running a tool call that failed three times in a row
  with the same arguments and error. The next identical call returns a notice
  instead of running, and repeating it again ends the request with a
  `repeated_tool_failure` reason instead of spinning to the turn cap (#1734).
- Truncated tool output no longer drops an oversized line that follows a short
  one: the model sees as much of that line as fits (its start, or its end for
  shell output), and the notice says how many of its bytes are shown (#1726).
- A stale `list_subagents` cursor no longer ends the parent request. A cursor
  that names no subagent in scope restarts the listing and reports
  `stale_cursor`, and settling a failed spawn no longer rewrites its start
  time, so cursors handed out earlier keep resolving (#1808).
- A spawned subagent is created and claimed within about a second on an
  idle runtime, however long its parent's run has been. To find the spawn
  arguments, creating a child reloaded the parent's whole run, including
  every earlier child's transcript, one spawn at a time, and claiming it
  reloaded the parent's whole session. A long orchestrator's children waited
  minutes, and background spawns hit their 60 s unclaimed deadline (#1807).
- When the request deadline stops a configured CLI tool, its result shows the
  output captured so far, followed by the deadline, instead of only the
  deadline. A command stopped by its own timeout keeps the last part of its
  output, marked `[Showing last N of M bytes]`, instead of the first part (#1669).
- The isolated-workspace spawn tests now pass on Linux hosts. They check
  whether the host can enforce the WorkspaceWrite sandbox. Where it can't
  (Linux, until #1601 adds one), they assert that the runtime refuses a
  ReadWrite-bound request with the explicit "requires an enforceable
  WorkspaceWrite sandbox on this host" failure before any provider turn,
  tool call or child workspace (#1846).
- `read_transcript_terminal_flag_tracks_child_lifecycle` no longer
  intermittently sees a `pending` child. It now waits until the child's claim
  (`processing`) is durable before reading the transcript (#1847).
- A command whose CRLF output runs past the output limit no longer fails to
  record its result. The shown part keeps each line's `\r`, so it is exactly
  the start of the captured output (#1867).
- A bash call rejected by its command policy no longer becomes `running`
  first. The policy is checked before dispatch, and the call goes straight
  from `pending` to `failed` (`policyDenied`). The same check runs before
  `spawn_process` admits a background command. Tool calls settled before
  dispatch (policy rejection, pre-dispatch failure or cancellation) no longer
  record a made-up `started_at` or `latency_ms` (#1801).
- A background subagent spawned on this runtime's own principal no longer
  fails after 60 seconds while it waits to be claimed; it stays queued and
  attached to its spawn. A cross-principal spawn that no host claims in time
  fails with the new `spawnUnclaimed` failure class instead of
  `serviceUnavailable`. Whichever deadline gives up on an unconfirmed child
  records a cancel intent in the same write, so a child that materializes or
  is claimed later is refused or interrupted instead of running unsupervised,
  and `list_subagents` reports such a spawn as `stopping` until its child has
  stopped (#1807).
- A spawn's cancel intent now refuses the claim only of the child its bridge
  receipt resolves through parent lineage and target principal, as the cancel
  mirror and acknowledgement already did. A pending request that names the
  bridge with another parent or principal is no longer interrupted by it
  (#1922).
- Desktop: starting or restarting the local agent from Local server settings,
  Add agent, or the menu bar no longer reports a runtime that is still
- Desktop: starting or restarting the local agent from Local server settings
  or the menu bar no longer reports a runtime that is still
  updating its data as a failure. It shows "Updating data…" and waits, and the
  menu bar's Restart Agent waits for the update instead of interrupting it
  (#1762).
- Desktop: Add agent no longer claims to create a second local agent under a
  new name while reusing the existing one. It reconnects this computer's
  local agent by its real name, reports success only once the agent is listed,
  and keeps the dialog open with the reason when that fails. First-run setup
  shows an existing home's agent name instead of an editable name it would
  ignore, and fails clearly rather than continuing under another name (#1615).
- Desktop: in a narrow window the session side panel and the fork notice take
  their turn with the shell's popovers, so opening one after the sync or
  context popover no longer stacks two dialogs (#1778).
- Desktop: stopping a request that has children no longer shows an
  "Interrupt requested" notification. Like a direct Stop, it shows
  "Stopping…" until the request is terminal and then the stopped notice;
  only a failure is announced, in plain language (#1616).
- Desktop: finished subagents keep their details in the session transcript,
  and workers spawned by earlier requests stay visible after a new message is
  sent. Rendered tool calls now name the request that issued them (#1784).
- Desktop: a model profile's context window can be raised again. The editor
  no longer sends the profile's own saved limits as the model's advertised
  facts, which made the saved value the ceiling; limits a backend does not
  advertise stay editable, bounded by an advertised maximum. The context meter
  shows the configured window as the runtime resolves it, and reports a window
  the runtime rejects instead of presenting it as in use (#1618).
- Desktop onboarding shows the recommended reasoning effort as a default that
  can be changed later; the selector sits with the other adjustable settings
  and says it applies to new requests (#1618).
- Desktop: a local agent's subagent sessions open with their messages and
  tool calls. The transcript was read under the desktop's own requester scope,
  but a subagent the agent spawns for itself is requested by the agent, so the
  read matched nothing; the agent's operator now reads such a session under
  its own scope (#1537).
- Desktop: a subagent session shows the parent work that spawned it, also
  after the parent moves on or completes. Its lineage is rooted at the exact
  request document its provenance names, through the agent-scoped lineage
  owner, instead of comparing that document id with a logical request id
  (#1834).
- A foreground `spawn_subagent` whose child is never confirmed no longer
  blocks its parent until the parent's own deadline. It carries the same
  unclaimed-spawn bound as a cross-principal spawn on every route; when the
  bound passes the parent's turn receives a non-retryable `spawn_unclaimed`
  result and the child is fenced (#1830).

## 0.19.0 - 2026-09-24

This release changes how conversations are stored. Earlier stores are not
carried forward: start from a fresh `~/.gents`, and update the desktop app and
every runtime you pair with to 0.19.0 together.

### Breaking

- Conversations use one canonical transcript: messages and output segments
  are immutable, append-only records with exact provider input, and tool calls
  keep their lifecycle state separately (#1571). Data from earlier versions is
  not migrated.
- The desktop compares the collection versions it replicates with an agent
  runtime's. It refuses to enroll with a runtime whose replicated collections
  differ, and its sync status shows "Update required", naming the differing
  collections, when the managed runtime it starts differs, instead of
  accepting messages the runtime can never receive. Versions whose replicated
  collections match stay compatible (#1122, #1729). Checks on remote-peer
  reconnect follow in a later release.
- A provider that goes silent no longer holds a request until its deadline.
  The new InferenceExecution `provider_idle_timeout_secs` (default 300s)
  bounds how long an attempt's provider connection may deliver no bytes,
  including the wait for response headers; expiry fails the attempt and
  follows the configured completion retry policy. Keepalives and thinking
  output count as activity. Queueing for a backend slot, tool execution and
  retry backoff are not bounded by it, and `stream_liveness_timeout_secs`
  now only sets the execution lease. The profile editor shows both fields
  (#1365, #1733).
- Live tests and evals choose their model endpoint from inference target files
  (`GENTS_EVAL_TARGET`). The old `GENTS_D4F_*` and provider environment
  variables are gone.
- A behavior set as default must be enabled. A stored config with a disabled
  default fails its next apply until you enable that behavior or choose
  another default (#1718).

### Added

- The desktop window can be as narrow as half of a 1440pt display. Navigation
  and side panels become menus and sheets at narrow widths (#1717).
- "Make default" enables the behavior and sets it as the default in one step.
- When the desktop finds a home an earlier version created, it says so and
  offers to back it up and start fresh (the default: the old home moves to a
  dated folder beside it), delete it and start fresh, or keep it and quit.
  Only the files a Gents runtime writes are moved or deleted, and the panel
  lists them first; other agents' homes, backups and your own files in
  `~/.gents` stay in place. `gents server` refuses such a store before
  writing its schema and exits with status 65 (67 for a store another,
  possibly newer, version extended), which systemd does not restart.
- `gents eval` measures a behavior against an eval definition pack. Each
  trial runs in a fresh embedded home against an inference target, grading is
  deterministic, and runs can be resumed. `gents eval init` interviews a model
  to draft, validate and optionally pilot a new definition pack.
- `gents optimization` runs prompt optimization jobs over eval runs. The
  promotion gates are modeled in Lean; the paired sign-flip permutation test
  they consult runs in Rust. A job is promotable only when its baseline pack
  matches the live configuration. Promotion publishes only if the frozen
  configuration is unchanged, and revert only if the promoted prompt is.
- Tools documents can set how much command output a completed call returns:
  `host.bash.max_output_chars` and `host.cli[].max_output_chars` bound stdout
  and stderr, each (UTF-8 bytes; default 16,000, allowed 1 to 1,000,000).
  Runtimes older than 0.19.0 reject documents that set them.

### Changed

- The README covers installing the desktop app. Build-from-source steps are in
  DEVELOPMENT.md.
- A backend rewrite no longer interrupts admission. Calls already running
  finish on the connection they started with, and new calls use the new one.
  Concurrency stays within `max_concurrent` across rewrites and outages
  (#1366, #897). Rotating only an API key now rebuilds the behavior's client.
- GitHub Releases attach the desktop installers and their checksums. CLI
  archives, install notes, debug symbols, build metrics, and desktop npm
  packages stay off the release page. The container image takes the Linux
  CLI from the release workflow's artifacts.
- The execution lease defaults to two minutes instead of 30, so crash recovery
  takes over sooner (#1626).
- The ChatGPT subscription advertises Codex client 0.157.0, which unlocks the
  GPT-6 models. New setups default to `gpt-6-astra` for the ChatGPT
  subscription, `claude-opus-5-5` for the Claude subscription, and `grok-4.7`
  for the Grok subscription.

### Fixed

- A tool that returns one line larger than the output limit shows a UTF-8-safe
  prefix or suffix instead of nothing.
- Desktop startup waits for a background agent that is still booting instead
  of failing, and shows how long it has waited. It fails when the service
  stops, when it keeps exiting (with the exit reason), or after five minutes,
  and then offers Try again, Restart agent, or continuing without it. Stop and
  Restart work while a start is waiting (#1607, #1609).
- On macOS, desktop startup detects that Gents still needs approval under
  Login Items & Extensions, explains what to allow, offers a button that opens
  that settings pane, and continues once Gents is allowed (#1608).
- Desktop status reports a background agent that exits a few seconds into
  boot as failed, with its exit reason, instead of waiting five minutes. On
  Linux, a unit systemd gave up on is reported with its exit cause, and Start
  clears its failed state (#1745).
- Two runtimes can no longer open one store: `gents server` and `gents init`
  hold an exclusive lock beside the data directory (`data.lock`). When another agent serves port 9191,
  setup, startup, Stop and Restart name its DID and home and say how to stop
  or move it; Stop and Restart of the desktop's own agent are never blocked by
  it. `gents server` for a non-default home warns when it takes port 9191
  without `--http-port` (#1746).
- On macOS, a start that launchd refuses before Gents is approved waits for
  approval and retries instead of failing. Approval revoked while the agent
  runs is shown on the agent screen, and after approval at launch the desktop
  starts an agent that is enabled at login with its reviewed access (#1748).
- Claude sign-in no longer discards a completed login when the runtime is not
  serving (#1614).
- "New Behaviour" is no longer stored before you save it (#1610). The code
  diff no longer marks every change as removed (#1617).
- `gents chat` shows when the agent is working, and keeps tool activity apart
  from answers (#1622).
- An admission rejection ends the request, so Codex clients no longer wait
  forever (#1637). Requests waiting in InputRequired are recovered (#1628).
  Steering input durability is proven again (#1627).
- First-run setup keeps each finished step visible with a one-line result and
  pauses on the completed list before moving on.
- Several test flakes and cancellation contracts are fixed (#1613, #1638,
  #1723, #1728).

### Security

- If you stored inline API keys under an earlier version, rotate them (#1394).

### Known issues

- A desktop build that isn't signed with the release identity can sit on
  "Starting the secure client…" instead of reporting that it can't read its
  keychain identity (#1739). Install the published release.

## 0.18.5 - 2026-09-22

### Added

- `gents cloud login --cloud <host>` signs this machine in to a gents cloud
  with a device code and stores the workspace token as an `OAuthCredential`.
- Self-config can preview a native graph proposal through the existing graph
  permission gate without publishing it.

### Fixed

- An AppImage desktop install copies its runtime to
  `~/.local/share/gents/desktop/runtime/gents` and runs the user service from
  that copy. Opening a newer AppImage refreshes the copy, and start or
  restart rewrites a stopped service definition so it does not keep a
  temporary `/tmp/.mount_*` path.
- Desktop startup treats a managed service as ready once it publishes its
  identity, instead of waiting out GraphQL probes that are still starting.
  On macOS the background item is attributed to Gents rather than the
  code-signing name.
- Admit a tool root that is a real descendant of the reviewed root, and reject
  sibling prefixes, traversal, and paths that escape the anchor.

## 0.18.4 - 2026-09-21

### Fixed

- Advertise live `tool_ceiling` and `tool_root` on `GET /status` so desktop
  start can match the initialized identity and reviewed host authority. 0.18.3
  omitted those fields and failed first-run and existing-home start with
  "native runtime readiness did not match".
- Continue an existing `~/.gents` home in desktop setup instead of presenting
  it as a brand-new agent.

### Changed

- GitHub Releases now ship only user-facing installers and CLI archives: macOS
  DMG, Linux `.deb`/AppImage, platform `gents-*.tar.gz` plus checksums and
  install notes. dSYM, build-metrics, and desktop npm tarballs remain workflow
  artifacts.

## 0.18.3 - 2026-09-18

### Fixed

- Verify Linux desktop sidecars through the canonical `gents version` command.

## 0.18.2 - 2026-09-18

### Changed

- Losslessly delta-encode witnessed request captures while preserving existing
  capture readability, reducing repeated transcript storage without a history
  rewrite.
- Reduce streaming progress and reasoning-preview volume, quiet idle runtime
  logging, and delegate managed runtime lifetime and logs to native user
  services.

### Fixed

- Restore signed macOS desktop packaging on release runners using Python 3.14.
- Include the vendored DefraDB Explorer assets in the Linux runtime image.

## 0.18.1 - 2026-09-18

### Added

- Embed the DefraDB Explorer: `gents serve` hosts the vendored embedded build
  at `/explorer/` on its HTTP listener (same-origin with the DefraDB API), and
  the desktop settings menu gains a Developer → DB Explorer option that opens
  it for the managed runtime in a dedicated window (bridge contract 7.10,
  `desktop_open_db_explorer`).

### Fixed

- Return the runtime control watcher to idle after a successful visible
  reconcile instead of polling the full configuration graph every second.
  Failed reloads and transient resolution errors continue to retry, and local
  operator configuration writes still hot-reload.

## 0.17.0 - 2026-09-11

### Changed

- Resolve runtime, CLI, desktop, and bundled-pack configuration through one
  canonical document model. `AgentSession` is the durable session, behaviors
  select context and inference profiles, tools own their nested settings, and
  tasks, triggers, schedules, and event sources share one desired-state owner.
- Remove the duplicate conversation, deployment, approval-hold, graph-config,
  validator, migration, and desktop configuration machinery superseded by the
  canonical owners. The full refactor removes more than 24,000 net lines.
- Keep the application/database boundary explicit: Gents selects authorized
  sessions and projects locally available documents; DefraDB owns delivery,
  reconnect catch-up, retries, and backpressure.

### Fixed

- Bound embedded transaction phases and route storage timeouts through the
  standard retry classifier without claiming ambiguous transactions committed.
- Converge hydration after failed or ambiguous delivery and terminal writes,
  with idempotent repeat delivery and durable periodic recovery instead of
  readiness heartbeats.
- Coalesce native document-change bursts, remove per-lease global scans, and
  keep sticky observer resync without event-drop amplification.
- Batch durable collection subscription changes and startup restoration while
  preserving Go-compatible DefraDB semantics. Failed live topic installation
  and removal now retry in-process from durable desired state.
- Scope pairing reconciliation wakes and skip redundant migration baselines,
  eliminating broad application replay and repeated startup work.

### Performance and validation

- Cover fresh and 2,500-revision enrollment, streaming, offline reopen, local
  transcript pagination, and independent database-to-observer projection in
  the canonical mobile integration suite.
- Verify all writes persist under a deterministic 32-writer hot-document load
  and that the next canonical write remains live.
- Measure clean client startup at 51-116 ms, collection subscription setup at
  3-16 ms, steady request delivery at 95-243 ms, and offline reply recovery at
  0.78-1.05 seconds, with no event-drop recovery in deterministic runs.
- Complete a fresh server, client, enrollment, real P2P, live inference, and
  client-visible conversation in 10.86 seconds against
  `GLM-5.3-Flash-NVFP4`.

### Upgrade

- Upgrade phone/desktop clients and agent runtimes together. Preserve stores,
  DIDs, identities, and enrollment state; do not wipe or re-pair installations.
  Mixed pre-0.17 canonical configuration models are not supported.

## 0.16.4 - 2026-09-09

### Fixed

- Deliver local chat commits through DefraDB's native replication path without
  a redundant application gossip rebroadcast.
- Flush streaming responses on a 100 ms deadline, persist the first update
  immediately, and keep retry wakeups out of document-change metrics.
- Drive observer refreshes from database changes and project the foreground
  session directly, avoiding periodic full-session polling and reloads.
- Track the live selected session across route changes and remove redundant
  startup schema, migration, and subscription work.
- Use the merged DefraDB and Regolith releases that provide native catch-up,
  backpressure, and current Iroh transport behavior.
- Route discovered-model catalog updates through the canonical committed-write
  owner instead of the read-only GraphQL helper.

### Validation

- Cover clean and aged pairing, foreground delivery, offline recovery,
  transcript convergence, and local pagination in the canonical enrollment
  integration suite, including hard local latency budgets.

### Upgrade

- Upgrade phone/desktop clients and runtimes together while preserving stores,
  identities, and enrollment state. Mixed historical wire protocols remain
  unsupported.

## 0.16.2 - 2026-09-08

### Fixed

- Let DefraDB own durable reconnect recovery and immediate replication wakeups;
  remove redundant application-wide replay and unchanged readiness heartbeats.
- Batch deep-history merge writes and remove the extra pre-send delay.
- Preserve HTTP transaction-conflict retries using DefraDB's own classifier.
- Verify fresh and aged enrollment, completed conversations, offline reply
  recovery, transcript continuity, and local pagination with canonical tests.

### Upgrade

- Upgrade phone/desktop clients and runtimes together. The updated DefraDB
  dependency uses multiplexed Iroh; mixed old/new protocol peers are not
  supported. Preserve existing stores, identities, and enrollment state.

## 0.16.1 - 2026-09-07

### Fixed

- Enrollment data-plane routes honor the `client-to-runtime` /
  `runtime-to-client` suffix so phone and desktop chat requests reach the
  runtime. Unsuffixed enrollment base routes still default to
  runtime-to-client.
- Consumption clients keep last-known-good behavior readiness when the
  local replica of `AgentBehaviorReadiness` is older than 45s, instead of
  treating lease lag as "the agent is gone." Local host runtimes still
  fail closed.
- Pending enrollment copy is "Waiting for pairing request acceptance",
  and Add Agent stays available while a request is outstanding. Enrolled
  peers use the advertised agent name instead of "Enrolled Agent."
- Default behavior for chat no longer requires a gossiped
  `AgentPrincipal`; the principal stays on the node.
- iOS builds compile the grok shim (`O_NOFOLLOW`) and auto-start a fresh
  mobile client so enrollment can run.

## 0.16.0 - 2026-09-07

### Added

- Claude Max / Claude.ai subscription backend: native Anthropic Messages HTTP
  wire, first-party `gents claude-login` PKCE, credential-expiry health probe,
  and `discover-models --write` (#1398, #1399, #1400).
- Desktop bridge 6.2: `MCPServiceHealthView.displayState` (`healthy | stale |
  unreachable`) is the only MCP health classification; the desktop's
  synthetic `stuck` state is removed.

### Breaking changes

- `AgentRequest.status` is removed; `lifecycle_state` is the only request
  state column and `gents_protocol::request_lifecycle::RequestLifecycleState`
  is its only owner (#1330). The pre-claim `workspace_binding_pending` status
  is now the lifecycle state `workspaceBindingPending`. `AgentRequest` is a
  client-authored collection that evolves only by baseline re-pin, so
  existing stores fail `ensure_migrations` with `UnknownLineage` for
  `AgentRequest` after upgrading and must be reset or export/imported; there
  is deliberately no migration step.
- Desktop bridge contract 5.2 -> 6.0: `SubagentNodeView`, `TaskRunResult`,
  and `TaskRunSummaryView` lose their request `status` field. Clients on an
  older contract are rejected.
- CLI JSON output drops the duplicate request `status` fields
  (`SubagentTreeNode.status`, `SessionHistoryRow.latest_request_status`,
  `RequestShowHeader.status`, `ChildRequestView.status`,
  `GraphRunRequestView.status`); read `lifecycle_state` instead.
- External projections expose request state once in their native vocabulary:
  OpenAI-Codex request items drop `lifecycle_state` (keep `status`), ATIF
  request extras drop `status` (keep `lifecycle_state`), and Amy trace records
  drop `request_lifecycle_state` (keep `request_status`). Session-history rows
  rename session `status` to `session_status` and keep request state separately
  as `latest_request_lifecycle_state`.

### Fixed

- Config documents are validated by one owner regardless of write path;
  `gents config behavior set` now rejects unknown backends, models,
  tool selections and profiles.
- `/healthz` and `gents fleet-slots` now report backends the local prober has
  vetoed as degraded/not accepting, matching admission.
- `Goal.tokens_used` now reports the charged total (input incl. cached +
  output), matching the request ledger; `/self` utilization is measured
  against the effective input budget.
- A backend whose API-key environment variable is unset now fails loudly on
  every path (previously the probers silently attempted unauthenticated calls).

## 0.15.0 - 2026-09-01

### Breaking changes

- Advance the Rust and npm desktop package train together to 0.15.0 and the
  desktop bridge contract from 1.5 to 4.0.
- Replace the retired Lark/RocksDB storage backends with Regolith through the
  DefraDB v0.19.0 cutover. Existing legacy data directories are rejected at
  startup; reset runtime state, or use Gents v0.14.0 to export data first.
- Remove legacy bearer pairing, local mobile-runtime request creation, and
  unsigned authority paths. Enrollment, routing, readiness, and mobile
  requests now require authenticated runtime-owned state (#1310-#1313).

### Mobile authority and hydration

- Bind enrollment signatures, leases, revocation, replay protection, and
  generation changes to the final authority boundary, and prevent offline or
  accumulated authorizations from starving an active enrollment (#1312).
- Make the runtime the sole readiness authority and require requester-bound,
  terminally complete two-node hydration with explicit ordering and counts
  before the desktop projects success (#1310, #1311).
- Tag and authenticate every request source, remove the mobile local-runtime
  path, and replace compatibility pairing UI with status-based enrollment
  controls and actionable hold diagnostics (#1313).

### Runtime, research, and reliability

- Add the web deep-research graph pack, external dependency projection, live
  qualification harness, and the open-source research gateway integration
  (#1277, #1294, #1296).
- Harden Ethereum wallet submission and mobile first-run, idle hydration, and
  viewport ownership behavior (#1282, #1283, #1297, #1309).
- Evaluate readiness freshness against an injected observation clock so stale
  state still fails closed without making deterministic CLI tests wall-clock
  dependent (#1317).

### Dependencies

- Upgrade every DefraDB crate to the v0.19.0 tag (`03e1035f`), adopt Regolith
  as the sole durable backend, require transport-routable gossip origins, and
  enable verified post-merge rebroadcast for Gents' multi-hop deployments.

## 0.14.0 - 2026-08-28

### Bridge contract

- Advance the Rust and npm desktop package train together to 0.14.0 and the
  additive desktop bridge contract from 1.3 to 1.5.
- Add owner-scoped mailbox commands plus truthful session hydration and
  global sync-health projections, including explicit progress, stalled, and
  schema-skew evidence for mobile clients (#1205, #1248-#1252).

### Mobile and desktop

- Move transcript projection into bounded database queries and preserve
  request-owned session hydration across reconnects and app backgrounding
  (#1212, #1248-#1252).
- Bind iOS bearer readiness and issuer records to stable endpoint identity,
  and harden the native readiness acceptance harness (#1235-#1237).
- Publish a development-signed arm64 iOS IPA for registered-device testing
  alongside the CLI and desktop package artifacts.

### Runtime, graphs, and CLI

- Add the Lean-fenced graph execution contract, immutable graph runs, bundled
  code-review package, and graph quickstart (#1225-#1230).
- Share desired-state application through the runtime, lengthen long-running
  agent deadlines, and reject liveness windows that cannot expire before the
  request deadline (#1219, #1240, #1264).
- Hydrate materialized `response show` output consistently with wait/chat and
  scope Codex shim skill toggles to the bound agent (#1242, #1260).

### Reliability and maintainability

- Fail closed on occupied or ambiguous server ports, synchronize runtime and
  bridge readiness on durable events, and route flaky fixture writes through
  bounded transaction-conflict retry (#1243, #1253, #1254, #1257-#1259).
- Split the Codex shim, owned completion loop, desired-state validator, bearer
  pairing, and P2P reconciliation into domain-focused production and test
  modules while preserving their public and observability contracts.
- Retire stale Operations-drawer journeys and replace blind background-tool
  polling with event-backed terminal-state diagnostics (#1241, #1268).

### Dependencies

- Advance DefraDB to `81ff3cee`, including upstream schema relation/default,
  query ordering/join, P2P replay-marker, filtered truncate, and embedded HTTP
  schema-operation fixes.

## 0.13.0 - 2026-08-25

### Bridge contract

- Advance the Rust and npm desktop package train together to 0.13.0 and the
  desktop bridge contract from 0.9 to 1.3.
- Make client-authored requests the sole session creation path, removing the
  `desktop_session_fork` projection, and add revisioned live-session deltas,
  bounded transcript-page evidence, exact-total markers, and observer merge
  counters (#1160, #1154, #1203).

### Mobile and desktop

- Add request-owned remote session hydration with explicit pending, stalled,
  schema-skew, and terminal states; keep recovery truthful across mobile
  backgrounding and reconnects (#1154).
- Bound long-session work at the database query, bridge payload, and React
  rendering seams: query transcript pages at the tip and backward cursor,
  preserve tool-call boundaries, coalesce live response updates, and avoid
  remounting previously rendered rows (#1184, #1185, #1203).
- Centralize reliable multi-server pairing and replace repeated full-store
  replication scans with explicit document-set replay (#1186).

### Runtime and formal foundation

- Add Lean-fenced isolated workspaces, callback planning and execution,
  request-scoped tool roots, frozen instruction provenance, and explicit
  cleanup receipts for agent-owned workspace lifecycles (#1164-#1174).
- Publish the graph pipeline foundation, pure intent compiler, task-backed
  graph routes, and bounded graph tool surface (#1190-#1193).
- Discover live `AGENTS.md` instructions for unbound requests and tighten the
  graph-native defending-code review pack (#1173).

### CLI, testing, and reliability

- Keep explicit transport log filters intact and expand live pairing,
  hydration, pagination, and inference acceptance coverage (#1188, #1154,
  #1203).
- Add repeatable mobile interaction artifacts and structural budgets while
  keeping noisy wall-clock measurements report-only (#1203).

### Dependencies

- Advance DefraDB to `54b629b1`, including explicit document-set P2P replay;
  this revision is the direct child of the current DefraDB `main` head.

## 0.12.0 - 2026-08-20

### Bridge contract

- Keep the additive desktop bridge contract at 0.9 and advance the Rust and
  npm desktop package train together to 0.12.0.

### P2P and mobile

- Make mobile pairing converge across reconnects, layered filters, reverse
  pairings, and repeated reconcile passes; close fleet convergence
  amplification and pin DefraDB's merged replication-ownership fix (#1145,
  #1156, #1157).
- Eagerly replicate the requester-scoped session index to paired mobile peers
  and retry index synchronization from the P2P supervisor (#1141, #1148).
- Clarify mobile configuration back-navigation and skip disabled P2P metrics
  polling (#1146, #1130).

### Runtime and formal foundation

- Add correlated event-trigger fan-in, reliable background subagent
  continuations, and an optional native LSP tool for coding behaviors (#1113,
  #1117, #1115).
- Unify durable descendant authorization and projections, bound terminal
  GraphQL persistence retries, and centralize GraphQL validation and run
  provenance (#1124, #1132, #1136).
- Add bounded datastore query surfaces and keep client-authored conversation
  collections compatible with fresh stores (#1125, #1151, #1155).

### Agents, demos, and tooling

- Add the repository review harness plus executable maintenance, security-scan,
  and live code-review demo surfaces (#1121, #1135, #1151, #1155).
- Replace cluster triage labels with program milestones and enforce roadmap
  horizons through the issue-hygiene automation (#1107).

### Build and reliability

- Replace RocksDB with Lark, narrow DefraDB feature consumption, and improve
  release build attribution and artifact measurement (#1101, #1109, #1119).
- Consolidate integration-test binaries, make live gates explicit, and repair
  the post-merge conformance and migration fences (#1140, #1149, #1150).

### Dependencies

- Advance DefraDB to the ownership-corrected `f928b300` revision.

## 0.11.0 - 2026-08-11

### Bridge contract

- Advance the additive desktop bridge contract from 0.5 to 0.9: Grok OAuth
  login, managed local-server lifecycle and tray events, and provider-account
  inventory/disconnect controls (#973, #1013, #1089).
- Advance the Rust and npm desktop package train together to 0.11.0.

### Runtime and formal foundation

- Make schema provenance and signing DefraDB-native, and persist the exact
  rendered provider request before send (#1087, #1059).
- Add deterministic inference seeds and request-wide token budgets (#1062).
- Extend the Lean-fenced runtime foundation across prompt assembly, compaction,
  admission slot accounting, tool-call CAS transitions, recovery, and
  background/subagent lifecycle convergence (#999, #998, #1007, #1006).
- Align durable tool outcomes across timeline projections and add
  Harbor-compatible ATIF trace export (#1095, #1098, #988).

### Agents, configuration, and automation

- Add persona request flows, reusable directory persona and inference-profile
  catalogs, and safer self-configuration ergonomics (#1028, #1014, #1050,
  #1052, #1056, #1057).
- Add document-driven EventTrigger graph experiments and capture consumers
  for persisted runtime facts (#1081, #1080).

### Providers, CLI, and desktop

- Add Grok/xAI subscription OAuth across the runtime, CLI, and desktop (#973,
  #974), plus provider-account settings and disconnect controls (#1089).
- Make local desktop agent onboarding optional and add managed-server controls
  (#1013).
- Align CLI initialization and lineage behavior with signed provenance, while
  retaining compatibility with older provenance JSON (#1094, #1092).

### Build and reliability

- Split and cache Rust CI workloads, slim the CLI dependency graph, and add
  release dependency/binary metrics (#1011, #1024, #1097).
- Favor faster local and release builds with non-LTO profiles and parallel code
  generation; harden runtime cancellation, bridge reconciliation, and
  resource-contended conformance startup (#1097).

### Dependencies

- Advance DefraDB to v0.18.0 (`61e429fc`).

## 0.10.1 - 2026-07-30

### Dependencies

- Advance DefraDB to v0.17.4 (`f9e21c68`), following the v0.17.3 pin from #972.

### Runtime and reliability

- Stabilize mobile chat and stop synthetic agent turns (#930).
- Complete native backgrounding for subagents and tools with real GLM E2E (#937, #945).
- Fix machine pairing replicator install: `source_did` must be `@immutable` (#939).
- Hard-fail materialize + restore post-migration CI gates; complete and pin the
  runtime migration baseline catalog (#947, #949).

### Cleanup

- Integration cleanup pass for runtime, desktop, CLI, and tests (#970, #971).
- Clippy and harness hygiene for trigger tests (#961).

## 0.10.0 - 2026-07-29

### Bridge contract

- 0.5 (additive): merged #871 inference onboarding —
  `desktop_probe_inference_endpoint`, `desktop_codex_login`, and
  `desktop_codex_login_cancel` under `config-write`; new one-shot
  `desktop://codex-login-url` event.
- Inference onboarding request, response, and login-URL event payloads now come
  from generated Rust bindings instead of handwritten TypeScript mirrors.
- Local runtime initialization/reset is serialized with client start/shutdown
  and rejects storage mutation while a client is live.

### Runtime reliability

- Request interrupt latches use the standard bounded DefraDB
  transaction-conflict retry, eliminating an observed cascade-conformance flake.

### Bridge contract 0.4

- Additive: `Pairing` error code; fingerprint permission inventory aligned with
  grantable `[[set]]` entries (`core`, `client-lifecycle`, bundles).
- `BridgeConfig::default().snapshot_grants` is **fail-closed** `core_only()`.
- Documented v1 process-wide snapshot grant model (not per-caller ACL).

### Bridge contract 0.3

- Additive: structured `BridgeError` on command error paths; `SnapshotGrants`
  projection at the snapshot builder seam; `native-e2e` cargo feature.
- Command failures now serialize as `{ code, message, retryable }`; the client
  accepts both structured errors and legacy bare strings during migration.
- `RenderedTimelineItem` variant fields now serialize in camelCase, repairing
  the latent Rust/frontend mismatch (`itemKey`, not `item_key`).
- Breaking for pre-package bridge consumers: `desktop_peer_status_fetch`
  accepts a saved `peerId`, not an arbitrary `serverAddress`; arbitrary-address
  probing is restricted to the fleet-admin command.
- See prior entries for 0.2 (`desktop_bridge_contract`, address probe, and the
  saved-peer status lookup).

### Packages

- New npm workspace packages, distributed as GitHub Release tarballs:
  - `@source-inc/gents-desktop-client` — typed transport, shared store, errors, testing
  - `@source-inc/gents-desktop-ui` — accessible shared primitives
  - `@source-inc/gents-desktop-chat` — chat projection, components, and styles
  - `@source-inc/gents-desktop-fleet` — discovery, pairing, health, and peer UI
  - `@source-inc/gents-desktop-operations` — rail, holds, health, lineage, traces
  - `@source-inc/gents-desktop-tokens` — semantic CSS tokens
- Fixture host `apps/fixture-host` consumes all packages, renders bridge session
  snapshots, and registers a file-backed domain operations tab without
  `runtime-admin`. It proves package/plugin composition; the automated two-node
  Amygdala journey remains downstream evidence.
- Tag releases attach clean-install-verified npm tarballs to the GitHub Release;
  downstreams pin those assets exactly.

### Downstream update workflow

1. Bump the git tag pin for `gents-desktop-bridge` and npm pins to the same `vX.Y.Z`.
2. Read the **Bridge contract** section for additive vs breaking diffs.
3. Run contract + e2e + visual gates (fixture host is the template).
4. Merge.

## Compatibility matrix

| Tag        | Bridge crate | npm packages | contract_version | Notes                                         |
| ---------- | ------------ | ------------ | ---------------- | --------------------------------------------- |
| v0.16.0    | 0.16.0       | 0.16.0       | 6.3              | Claude subscription backend; packs catalog; write owner |
| v0.15.0    | 0.15.0       | 0.15.0       | 4.0              | Authenticated mobile authority; Regolith; DefraDB v0.19.0 |
| v0.14.0    | 0.14.0       | 0.14.0       | 1.5              | Mobile sync health, graph review, clarity refactors; DefraDB `81ff3cee` |
| v0.13.0    | 0.13.0       | 0.13.0       | 1.3              | Hydration, bounded mobile transcripts, graph pipeline; DefraDB `54b629b1` |
| v0.12.0    | 0.12.0       | 0.12.0       | 0.9              | Mobile pairing convergence; eager session index; DefraDB `f928b300` |
| v0.11.0    | 0.11.0       | 0.11.0       | 0.9              | DefraDB v0.18.0; signed provenance and build metrics |
| v0.10.1    | 0.10.1       | 0.10.1       | 0.5              | DefraDB v0.17.4; mobile/subagent/migration fixes |
| v0.10.0    | 0.10.0       | 0.10.0       | 0.5              | Reusable desktop packages implemented in #878 |

## 0.8.0

Baseline before reusable package extraction (pre-#877 implementation).
