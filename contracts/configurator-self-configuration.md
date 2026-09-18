# Configurator self-configuration contract

Issues: #1467, #1503, #1504, #1506, and #1508 inventory only. Baseline:
`main` at `1c0c34926`.

## Demo acceptance refinement (#1475)

`config behavior tools` selects optional LSP and native graph tools on an
existing sibling. Creation/clone/edit remain signed commands and reject these
flags. After creation, callers explicitly select tools using the returned
behavior ID; failures never imply recreating the behavior. This separate
operation delegates to the existing
SelfConfigCore identity-scoped patch/validation/publication transaction.
Protected Setup and shared Context/Tools references are rejected rather than
silently changing other behaviors. Omission preserves current selections and
full LSP settings; false revokes. Permission-preset edits retain their existing
replacement semantics, so callers must re-inspect/reselect afterward.
These are focused conveniences, not a second Tools model or a full arbitrary
Tools editor. Recipes remain optional.

Native graph APIs use `Tools.built_ins.enable_graph_tools`, default false,
independent of self-configuration and pack installation. Graph invocation still
uses the existing caller-admission, host ceiling, and node/principal owners.
No new runtime home, CLI executor, graph state, or identity is introduced.

Inspect returns `tool_grants.configured` from decoded canonical Tools and
distinguishes configuration from activation, LSP process readiness, installed
packs, caller admission, and successful execution. Applied outcomes compare the
requested flags against persisted selections. Tests also resolve the created
behavior's actual tool surface after restart-style reload: LSP and native graph
tools are present without configuration/installation tools.

The signed request includes an `edit_fields` mask. It distinguishes omission
from explicit clear without adding a duplicate writable configuration type.
The mask is covered by the request signature; DefraDB does not support
`@immutable` on list fields, so reconciliation rejects any mask mutation whose
signature no longer verifies.

Setup and generated working-role instructions prohibit searching/adopting another
runtime home, rebuilding the CLI to bypass missing authority, and resetting or
deleting databases for a review request. Missing authority/version/dependency
problems are reported as blockers. Native graph observations use the actual
terminal vocabulary and return control while work is running.

## Decision: keep the request boundary, retire “persona” from the model surface

`AgentBehavior` remains the only reusable runtime interface. The model-facing
tool is `config`; its `behavior` commands preview, list, inspect, and mutate the
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

## Model-facing `config` contract

The runtime exposes one self-configuration tool with one parameter:
`{"argv":[...]}`. The argv is parsed against an allowlisted grammar and is
never passed to a shell. The tool definition briefly teaches the model the
canonical graph before every call:

`Behavior -> Context -> Tools` controls instructions and capabilities;
`Behavior -> InferenceProfile -> Backend` controls model execution, with
optional sampling and execution references. Documents are exact-ID,
same-principal references. Reads never mutate. Writes are atomic sparse patches:
omission preserves, `--clear` removes an optional field, and credentials/OAuth
consent stay with their operator-owned flows.

Core examples are `config behavior list --limit 20`,
`config behavior get <id>`, `config behavior edit --id <id> --display-name ...`,
`config tools get`, `config tools edit --set host=<json>`, and
`config help <resource>`. Invalid commands name the rejected token and the
accepted vocabulary. List responses are bounded to 50 entries and report
`total`, `returned`, `truncated`, and `next_cursor` explicitly.

## Tool contract

| Tool | Canonical read/write owner | Prerequisites and inputs | Validation and recovery | Effective result and activation | Pairing/restart |
| --- | --- | --- | --- | --- | --- |
| `config` | Reads the bound canonical graph, targeted resources, or the behavior catalog. Writes reuse `SelfConfigCore`/`ConfigAccess`; behavior catalog mutations use the signed internal request owner. | One allowlisted `argv` array. `help` returns the data model and exact resource grammar. JSON-valued patch fields use repeated `--set FIELD=JSON`; clear uses repeated `--clear FIELD`. | Rejects unknown commands/options, protected fields, bad JSON values, foreign references, invalid documents, secret changes, lockout, and unavailable grants. Errors identify the bad argv/field and accepted choices. | Reads are side-effect free. Preview returns `committed:false`. Apply returns the exact document/diff/receipt and affects later work after reconciliation. | Canonical documents and signed outcomes survive restart and replicate only through authorized routes; host readiness is revalidated. |
| `list_graphs` | Reads principal-scoped `GraphDefinition` and the verified active `GraphRevision` plan through the current node’s `ConfigAccess`. | `{}` and pack-install authority on Setup. | An inactive definition is returned with no active plan and cannot run; an invalid active pointer or immutable digest fails. Activate/reinstall the exact bundled pack or ask the operator to repair it. | Returns package attribution, graph/digest, entries, input contracts, result contracts, limits, and exact `run_graph` selectors. No mutation. | Reads durable state on the managed node; never discovers a binary, home, port, DID, or credential. |
| `run_graph` | Uses the graph pipeline start transaction on the current node/principal. `code_review` additionally uses the shared host-evidence adapter and existing workspace provisioner. | Package adapter inputs (`code_review`: repository/base/head/focus; `web_deep_research`: question/options), or exact graph/digest/entry/input from `list_graphs`. | Requires installed active package attribution and digest. Code review requires effective read authority, an explicit effective root after behavior narrowing meets the process ceiling, and a canonical Git repository underneath it; revisions must resolve. Generic input is checked against the entry schema by the graph owner. | Returns the durable run receipt plus a freshly loaded `GraphRunView`. `running` is not success; poll status and require terminal result contracts. | Run, seed, workspace lineage, requests, and result refs are durable. The active runtime continues recovery after restart/reconnect. |
| `get_graph_run` | Reads the durable graph projection for the exact current principal/run. | `{run_id}` from `run_graph`. | Foreign/missing runs fail closed. Keep polling while `status=running`; inspect failure/cancellation evidence when terminal. | Returns stages, requests, counts, deadlines, result satisfaction, and terminal evidence. Read-only. | Reconstructed from durable documents after restart; no UI-local progress heuristic. |
| `get_graph_result` | Loads the terminal result view and hydrates exact result documents through the graph result owner. | `{run_id}` after terminal status. | A nonterminal or unsatisfied result is not success. Return to `get_graph_run`; do not summarize an install receipt or running state as a result. | Returns terminal status, immutable result refs, hydrated documents, and errors. | Durable and reproducible from the pinned revision/result commits. |
| `cancel_graph_run` | Persists cancellation intent through the graph-run CAS owner and reuses ordinary request interruption/reconciliation. | `{run_id,reason?}`. | Foreign/missing runs and oversized reasons fail. Terminal runs are returned unchanged; cancellation is complete only when the durable view is terminal. | Returns the observed run state after cancellation request/reconcile. | Pending cancellation and terminal outcome survive restart and can be re-driven safely. |

## `config behavior` fields

| Field | Canonical effect | Required/optional rules |
| --- | --- | --- |
| command | Chooses read, preview, or signed mutation flow. | `list`, `get`, `preview`, `create`, `edit`, `clone`, `disable`, `context`, or `tools`. |
| `behavior_id` | Exact same-principal `AgentBehavior` target. | Required for inspect/edit/disable; never inferred from display name. |
| `clone_from` | Exact enabled source behavior. Its canonical context/tools are copied into new owned documents. | Required for clone; incompatible with a preset in the signed create intent. |
| `display_name` | Writes `AgentBehavior.display_name`; the internal request maps it to its private historical wire field. | Required and 1–64 characters for create; optional sparse update for edit. |
| `description` | Writes both `AgentBehavior.description` and the isolated `AgentContext.description`, keeping interface and instruction context coherent. A clone inherits when omitted. | Optional for create/clone, explicit on edit when changing it. |
| `system_prompt` | Writes literal `AgentContext.system_prompt`; no template evaluation. | Nonblank when supplied; required for preset-based create, inherited by clone when omitted. |
| `profile_id` | Writes `AgentBehavior.inference_profile_id`; profile owns backend/model/sampling/execution selection. | Required exact published same-principal profile for create/clone; optional sparse update for edit and never clearable. |
| `preset` | Materializes canonical `Tools` host modes (`readonly` or `write`); does not create an inference path or grant self-configuration. | Required for from-scratch create; optional known replacement on edit; omitted for clone. |
| `root` | Writes `Tools.host.root` as behavior-level narrowing. It never expands the process ceiling. | Nonempty value must appear in ceiling-filtered `allowed_roots`. On edit, omission preserves narrowing and explicit JSON `null` clears it. |
| `make_default` | Atomically writes `AgentPrincipal.default_behavior_id` to the applied behavior. | Optional false; allowed for create/edit/clone, forbidden for disable. |

## Repeatable evaluation matrix

Every evaluation must parse tool JSON and query durable documents; model prose
alone is never evidence.

| Scenario | Required assertions |
| --- | --- |
| Coding behavior | Preview admits a complete prompt; create returns four materialized IDs; Behavior/Context descriptions and literal prompt match; effective root is inside the process ceiling; principal default equals the new ID; a new session is explicitly bound to it. |
| Read-only research | Preset authors `ReadOnly` files/bash, effective authority is no broader than the process ceiling, prompt is nonblank, and a new request resolves the selected profile. |
| Safe edit | Inspect first; send only changed fields. A name-only or prompt-only edit preserves profile/root/preset/tools/default; explicit clear is distinct. Old Context/Tools shared by another behavior remain unchanged; post-apply effective state is re-read. |
| Code review | Install receipt is not treated as execution; discovery returns `review` and its exact digest; run captures immutable evidence under the admitted root; status reaches a terminal state; success requires all terminal result contracts and hydrated report documents. |
| Rejected authority/root | A path outside the process ceiling or published narrowing roots produces a rejected/no-write outcome naming valid choices; Setup and the target documents are byte-equivalent before/after. |
| Restart/reconnect | Stop after durable apply/run publication, restart the runtime and reconnect an authorized client; inspect returns the same canonical IDs/effective config, pending work re-drives, and no duplicate request-owned Context/Tools appears. |

False-success gates: an empty/missing preset-create system prompt is rejected;
an applied receipt without a fully resolvable Behavior/Context/Tools/Profile
chain is an error; install does not imply run; running/cancelling does not imply
terminal success; and terminal success requires the declared result contracts.
