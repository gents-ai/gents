# Configurator tool coverage audit

Sources: canonical `document_config/tools.rs`, runtime `tool_surface/build.rs`
and `tool_surface/behavior_config.rs`, desired-state `config_client/desired_state.rs`,
model command dispatch in `self_config/command.rs`, and CLI/desktop Setup grants.

Configuration selection, document authoring, and successful execution are separate
coverage requirements. The table records the current implementation, not promised
commands. The `tools` resource uses targeted sparse patches, with typed complete
values for nested groups and runtime ceilings enforced by existing owners.

| Capability | Model configuration path | Remaining execution/authoring coverage |
| --- | --- | --- |
| Host files and commands | `behavior create --preset write/readonly --root`; `tools edit --behavior` host group | Builder readiness executes a shell test. The host-steward suite checks real isolated service, disk and backup effects; historical artwork results are retained in HISTORICAL_COHORTS.md, not part of the current baseline. |
| Background commands | Host bash background flags and timeout settings | Existing runtime tests; add consumer process start/observe/cancel case |
| Named CLI tools | Host cli selection | Requires installed executable and a runtime execution check |
| MCP | Remote service/tool allowlists and presentation; `mcp-service get/preview/edit` | Existing services only; missing service creation and catalog discovery in config |
| Subagents | Subagent target IDs, spawn/steering/background grants | No model config authoring command for SubagentTarget; existing target can be selected |
| Graph execution | Built-in graph flag; native list/run/observe/result/cancel | Pack graph installation supported; caller admission still independently required |
| Goals | Separate goal tools and creation flags | Goal declarations and terminal states have existing owners; consumer case pending |
| Memory/history/context budget | Independent built-in flags | Selection available; live exercise cases pending |
| Datastore queries | Datastore query flag and exact collection allowlist | Bind to working behavior and execute bounded query |
| Datastore create/query surfaces | `datastore get/preview create/preview edit/create/edit`; bind through Tools.datastore | Transactional preview/edit/Setup-protection tests pass; bounded discovery absent. Surface syntax is checked on publication; live schema and tool collisions are checked by runtime binding/execution owners |
| Schemas | `schema get/preview install/install`; shared additive schema contract/publication owner with packs and CLI | Node-wide contracts, not document ACP grants; model command regressions pass, exercised in 10/10 successful model-authored automation trials |
| LSP | Integrations.lsp settings | Needs installed/indexed server; presence is not readiness |
| Ethereum | Integrations.eth_tool_ids selects owned EthTool documents | No model config authoring for EthTool; signing keys/credentials stay operator-owned |
| Skills | `skill get/preview import/import`; Context skill_ids attaches owned Skill documents | Shared CLI/model SKILL.md loader, root-bound reads, create-only publication; canonical source_directory is supplied by load_skill for supporting references without widening authority. Fresh-session source-relative execution passed 10/10 `b96b69918` trials (see README). Bounded inventory absent |
| Automation | `automation get/preview/edit` task, schedule, event-source, trigger with target behavior | Model-authored schema/surface/task/trigger with two real correlated document submissions passed 10/10 `b96b69918` trials |
| Inference | Profile inventory/create/edit; backend inventory/edit | Backend creation absent; credentials use operator-managed auth |
| Self-configuration | Explicit categories, preview, no-lockout, separate pack install grant | CLI/desktop initial grants now include advertised backend/MCP/automation categories |

The corrected-task cohorts completed 25 upstream attempts, 14 reviews and ten
improvements, without counting prerequisite skips. README records every cohort,
raw verdict, grader change and measurement limitation.
Template authoring now compiles through the execution parser after a live
diagnostic exposed accepted invalid syntax. Subsequent automation cohorts passed
9/10 and 8/10; syntax validation does not establish correct output framing or
appropriately bounded datastore access.
Audit gaps for other
referenced documents must remain visible until implemented and exercised. Do not
grant all operational capabilities to Setup merely to test a working behavior.

The progressive eval retains artifacts and distinguishes prerequisite failures,
model request failures, infrastructure errors and independent acceptance failures.
At least ten GLM attempts per case are complete. GLM onboarding plus fresh-session Builder execution
passed 10/10 on 2026-09-15 (160.86s, concurrency 2); this predates skill import
and does not establish acceptance of later stages. The earlier Qwen matrix is
not evidence for GLM trials.
