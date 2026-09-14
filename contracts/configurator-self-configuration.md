# Configurator self-configuration contract

Issue: #1467. Baseline: `main` at `1aebca07`.

## Decision: keep the request boundary, retire “persona” from the model surface

`AgentBehavior` remains the only reusable runtime interface. The model-facing
tool is `configure_behaviors`; it previews, lists, inspects, and mutates the
canonical `AgentBehavior -> AgentContext -> Tools` plus `InferenceProfile`
chain. It does not expose a Persona configuration model.

`PersonaConfigRequest` is retained as an internal, immutable behavior-change
intent because its current owner provides guarantees that direct operator
document writes do not collectively replace:

- an exact local-self signature or fresh enrollment-generation authorization;
- catalog admission before any configuration write;
- one desired-state transaction for Behavior, isolated Context/Tools, and
  optional principal-default publication;
- request-key idempotency and repair after materialization succeeds but the
  terminal receipt write is interrupted; and
- a durable applied/rejected outcome that can replicate back to an authorized
  offline/paired client.

Setup, CLI, desktop, and mobile therefore continue through the same admission
and materialization core. `PersonaConfigRequest` is a transport command DTO,
not runtime configuration. Renaming the replicated collection would add a
schema transition without improving the boundary, so only model-facing
vocabulary changes here.

## Tool contract

| Tool | Canonical read/write owner | Prerequisites and inputs | Validation and recovery | Effective result and activation | Pairing/restart |
| --- | --- | --- | --- | --- | --- |
| `get_my_config` | Reads the bound `AgentBehavior`, `AgentContext`, `Tools`, `InferenceProfile`, backend/sampling/execution/retry/compaction references, skills, and bound automation through `SelfConfigCore` + `ConfigAccess`. Optional patch preview uses the same desired-state validator and aborts. | Exact running principal and behavior; `{}` to inspect. Optional `{preview:{category,kind,id,patch}}` only when dry-run is enabled. | Rejects protected/unknown fields, foreign references, invalid canonical documents, secret changes, lockout, and unavailable preview categories. Retry with the published writable fields. | Returns authored documents, redacted backend auth, process ceiling, behavior narrowing, resolver-derived effective modes/root, and generation timing. A preview has `committed:false`; a write affects later dispatch after generation swap, never the current turn. | Canonical documents replicate only through authorized config routes. Runtime observation is recomputed on restart; it is not portable identity/config data. |
| `configure_behavior` | Patches only Setup’s bound `AgentBehavior` or `AgentContext` through the common desired-state transaction. | `{target:"behavior"|"context",patch}`. Inspect first. | Identity/owner fields and Setup enablement are protected; full candidate reference validation is atomic. Use `get_my_config` preview and correct the named field/reference. | Returns document ID, exact diff, commit flag, and timing. Applies after generation swap to later work; existing sessions/turns remain bound. | Durable canonical document behavior; paired observation follows authorized replication. |
| `configure_tools` | Patches Setup’s bound `Tools`; nested host/built-in/integration/self-config groups remain canonical. | `{patch}`; inspect and preview first. | Cannot self-grant pack installation, cannot escape the process ceiling, cannot shadow built-ins, and may enforce no-lockout. Use narrower modes/root or operator settings. | Returns committed canonical diff. The read contract separately reports requested vs process-ceiling-effective authority. | Persisted Tools reload after restart; physical root availability is revalidated by the runtime host. |
| `configure_profile` | Patches the bound `InferenceProfile`, sampling, execution, retry, or compaction document. | `{target,patch}` with an exact target from inspection. | Same-principal references and complete typed validation; no implicit backend/model fallback. Correct the exact profile/reference or use operator configuration for missing documents. | Returns committed canonical diff. Shared references observe the change after reconciliation; in-flight work is unchanged. | Authorized canonical configuration replicates; credentials and health observations do not travel with ordinary client/session sync. |
| `configure_behaviors` | Lists/inspects canonical chains directly. Mutations author a signed internal `PersonaConfigRequest`; the reconciler admits it and atomically materializes `AgentBehavior`, isolated `AgentContext`/`Tools`, and optional `AgentPrincipal.default_behavior_id`. | `list`; `inspect + behavior_id`; `preview + operation`; or create/edit/clone/disable fields below. Exact published profile/root/preset values are required. | Rejects unknown/disabled sources, protected Setup edits/disables, foreign/phantom agents, stale enrollment authority, bad signatures, unavailable roots/profiles/presets, blank required instructions, and disable+default. Rejection returns the offending value and published choices. | List/inspect return complete semantic documents and effective authority. Preview writes nothing. Applied mutations are re-read through the runtime resolver and return behavior/context/tools/profile IDs, effective values, default status, and timing. Use a new session selecting the new behavior to test it. | The intent’s terminal outcome and canonical documents survive restart and replicate over authorized pairing routes. Repair re-drives a pending intent without duplicating configuration. |
| `install_pack` | Resolves only a bundled pack, stages canonical package documents through the graph package installer, and activates its immutable `GraphRevision` through the graph pipeline owner. | `{package,variables?}`; current principal is implicit. Model/endpoint variables default from Setup’s selected profile/backend. | Rejects arbitrary paths/URLs, protected variables, malformed names/values, owner/reference/schema mismatch, incomplete materialization, and activation CAS mismatch. Inspect config or correct exact declared variables. | Returns install receipt, graph ID, exact active digest/generation, dependencies, and explicitly says no run started. Activation is durable immediately; execution is separate. | Installed canonical documents and active observation reload on the same principal. Host dependencies are revalidated and are not portable guarantees. |
| `list_graphs` | Reads principal-scoped `GraphDefinition` and the verified active `GraphRevision` plan through the current node’s `ConfigAccess`. | `{}` and pack-install authority on Setup. | An inactive definition is returned with no active plan and cannot run; an invalid active pointer or immutable digest fails. Activate/reinstall the exact bundled pack or ask the operator to repair it. | Returns package attribution, graph/digest, entries, input contracts, result contracts, limits, and exact `run_graph` selectors. No mutation. | Reads durable state on the managed node; never discovers a binary, home, port, DID, or credential. |
| `run_graph` | Uses the graph pipeline start transaction on the current node/principal. `code_review` additionally uses the shared host-evidence adapter and existing workspace provisioner. | Package adapter inputs (`code_review`: repository/base/head/focus; `web_deep_research`: question/options), or exact graph/digest/entry/input from `list_graphs`. | Requires installed active package attribution and digest. Code review requires effective read authority, an explicit effective root after behavior narrowing meets the process ceiling, and a canonical Git repository underneath it; revisions must resolve. Generic input is checked against the entry schema by the graph owner. | Returns the durable run receipt plus a freshly loaded `GraphRunView`. `running` is not success; poll status and require terminal result contracts. | Run, seed, workspace lineage, requests, and result refs are durable. The active runtime continues recovery after restart/reconnect. |
| `get_graph_run` | Reads the durable graph projection for the exact current principal/run. | `{run_id}` from `run_graph`. | Foreign/missing runs fail closed. Keep polling while `status=running`; inspect failure/cancellation evidence when terminal. | Returns stages, requests, counts, deadlines, result satisfaction, and terminal evidence. Read-only. | Reconstructed from durable documents after restart; no UI-local progress heuristic. |
| `get_graph_result` | Loads the terminal result view and hydrates exact result documents through the graph result owner. | `{run_id}` after terminal status. | A nonterminal or unsatisfied result is not success. Return to `get_graph_run`; do not summarize an install receipt or running state as a result. | Returns terminal status, immutable result refs, hydrated documents, and errors. | Durable and reproducible from the pinned revision/result commits. |
| `cancel_graph_run` | Persists cancellation intent through the graph-run CAS owner and reuses ordinary request interruption/reconciliation. | `{run_id,reason?}`. | Foreign/missing runs and oversized reasons fail. Terminal runs are returned unchanged; cancellation is complete only when the durable view is terminal. | Returns the observed run state after cancellation request/reconcile. | Pending cancellation and terminal outcome survive restart and can be re-driven safely. |

## `configure_behaviors` fields

| Field | Canonical effect | Required/optional rules |
| --- | --- | --- |
| `action` | Chooses read, preview, or signed mutation flow. | Required: `list`, `inspect`, `preview`, `create`, `edit`, `clone`, or `disable`. |
| `operation` | Selects the mutation validated without persistence. | Required only for `preview`: `create`, `edit`, `clone`, or `disable`. |
| `behavior_id` | Exact same-principal `AgentBehavior` target. | Required for inspect/edit/disable; never inferred from display name. |
| `clone_from` | Exact enabled source behavior. Its canonical context/tools are copied into new owned documents. | Required for clone; incompatible with a preset in the signed create intent. |
| `display_name` | Writes `AgentBehavior.display_name`; the internal request maps it to its historical `persona_name` wire field. | Required and 1–64 characters for create/edit. |
| `description` | Writes both `AgentBehavior.description` and the isolated `AgentContext.description`, keeping interface and instruction context coherent. A clone inherits when omitted. | Optional for create/clone, explicit on edit when changing it. |
| `system_prompt` | Writes literal `AgentContext.system_prompt`; no template evaluation. | Nonblank when supplied; required for preset-based create, inherited by clone when omitted. |
| `profile_id` | Writes `AgentBehavior.inference_profile_id`; profile owns backend/model/sampling/execution selection. | Required exact published same-principal profile for create/edit/clone. |
| `preset` | Materializes canonical `Tools` host modes (`readonly` or `write`); does not create an inference path or grant self-configuration. | Required for from-scratch create; optional known replacement on edit; omitted for clone. |
| `root` | Writes `Tools.host.root` as behavior-level narrowing. It never expands the process ceiling. | Nonempty value must appear in ceiling-filtered `allowed_roots`. On edit, omission clears narrowing, so callers must inspect and resend unless widening to the process ceiling is intended. |
| `make_default` | Atomically writes `AgentPrincipal.default_behavior_id` to the applied behavior. | Optional false; allowed for create/edit/clone, forbidden for disable. |

## Repeatable evaluation matrix

Every evaluation must parse tool JSON and query durable documents; model prose
alone is never evidence.

| Scenario | Required assertions |
| --- | --- |
| Coding behavior | Preview admits a complete prompt; create returns four materialized IDs; Behavior/Context descriptions and literal prompt match; effective root is inside the process ceiling; principal default equals the new ID; a new session is explicitly bound to it. |
| Read-only research | Preset authors `ReadOnly` files/bash, effective authority is no broader than the process ceiling, prompt is nonblank, and a new request resolves the selected profile. |
| Safe edit | Inspect first; resend the existing root/preset/profile while changing instructions; old Context/Tools shared by another behavior remain unchanged; post-apply effective state is re-read. |
| Code review | Install receipt is not treated as execution; discovery returns `review` and its exact digest; run captures immutable evidence under the admitted root; status reaches a terminal state; success requires all terminal result contracts and hydrated report documents. |
| Rejected authority/root | A path outside the process ceiling or published narrowing roots produces a rejected/no-write outcome naming valid choices; Setup and the target documents are byte-equivalent before/after. |
| Restart/reconnect | Stop after durable apply/run publication, restart the runtime and reconnect an authorized client; inspect returns the same canonical IDs/effective config, pending work re-drives, and no duplicate request-owned Context/Tools appears. |

False-success gates: an empty/missing preset-create system prompt is rejected;
an applied receipt without a fully resolvable Behavior/Context/Tools/Profile
chain is an error; install does not imply run; running/cancelling does not imply
terminal success; and terminal success requires the declared result contracts.
