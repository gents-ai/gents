# Mailbox — human-attention index

**Status:** design from session 2026-08-25; awaiting review before the
implementation plan.
**Branch / worktree:** `feat/mobile-mailbox` at
`../gents-mobile-mailbox`.
**Motivating use case:** [PR #1161](https://github.com/source-inc/gents/pull/1161)
(agentic SDLC / ECSS-E-ST-40C document DAG). The mailbox is not that pack; it
is the platform-agnostic attention index that pack (and others) file into.

## Problem

A paired client — especially a phone — has no single place that answers “what
on this server wants *me*?” Today that question is scattered and incomplete:

- The session list groups some conversations as “Needs attention” from
  turn-state heuristics (`inputRequired` is reserved protocol vocabulary and
  is **not emitted**).
- Tool-approval holds exist as `AgentToolCall.lifecycle_state =
  awaitingApproval`. They are not this product’s human-wait path, and the
  mailbox does not wrap them.
- Long-running graph work (code review, SDLC V, any EventTrigger DAG) parks,
  flags, or finishes while the human is gone. The graph end then needs a
  **create**: a `Rid`, a superseding artefact, a new `AgentRequest`, or an
  ack. GitHub comments and a phone session are supposed to land the *same*
  domain document (#1161: “the transport is not the semantics”). Nothing
  currently lists those asks as replicated index rows.

The mailbox is that list: durable documents on the server, addressed to the
human who owns the work, readable by every client without hydrating
transcripts or pack artefacts.

## Decision

Introduce one branchable collection, `MailboxItem`. It is a **general
attention envelope**, not a new unblock semantics and not a second chat
surface.

- **Provenance** on the item says where it came from.
- **Handling** (`action`) is a small closed set the client switches on.
- **Follow-up** is either the human create the item asked for, or a later
  `MailboxItem` the graph files (`parent_item_id`). The mailbox is not a
  workflow engine.
- **Close** is `acted` when that expected create exists, `dismissed` when the
  owner clears the row, `expired` when a deadline passes still `open`. Status
  updates do **not** fire EventTriggers (`event_kind` remains `created`-only).
  Unblocking a parked graph is the domain create (`Rid`, superseding RB,
  `AgentRequest`).

GitHub-written and phone-written domain documents are equivalent. The mailbox
is the index so a phone can see the ask.

## Constraints established by research

- **Everything is a document.** Filing, dismissing, and listing must be
  document reads/writes. No side-channel inbox.
- **`event_kind` is `created`-only.** In-place mailbox status must not be an
  edge. Same append-only story as #1161 (supersede / create, don’t mutate a
  baseline to advance the graph).
- **Replication filter fields must be `@immutable` scalar LWW.**
  `requester_did` / `agent_did` already are, which is why session cards
  filter. `MailboxItem.requester_did` and `item_key` follow that rule.
- **BranchableSync is collection-granularity.** The `client-index`
  requester predicate is not a tenant fence (#1141). A full paired client
  may see every mailbox row on that agent. Acceptable for today’s
  single-user full client; not to be reused as a multi-tenant proof.
- **Index vs transcript.** Session cards hydrate without
  `SessionHydrationRequest`; transcripts do not. Mailbox rows are index
  documents. Opening the list never pulls Rid/RB/TS bodies or chat history.
- **`BoundedWriteTool` already rejects model-supplied `fill` fields**
  (`crates/gents/src/defra_write/mod.rs`). Today `WriteToolFieldFill` is only
  `correlation` | `source_field` from trigger context
  (`CurrentToolRuntimeContext` has `session_id` but not `requester_did`).
  Mailbox identity stamps therefore go through a dedicated stamp helper,
  not a new general fill vocabulary. The helper reads request context and
  rejects model-supplied values for stamped fields with the same rule.
- **`AgentRequest.caused_by_source_doc_id`** is the existing correlation
  slot for “this request was created because of that document.” `start_request`
  close uses it.
- **Graph ownership is explicit lineage.** EventTrigger materialization does
  not currently put `requester_did` on `AgentRequest`; the mailbox cannot infer
  the human owner from the acting agent, Task, or EventTrigger. Any graph path
  that may file must carry immutable `requester_did` on its initiating and
  stage-output domain documents. The graph entry stamps it from the
  authenticated starter/operator, never model input. Trigger source capture
  snapshots that fixed field, the production materializer validates it and
  writes it to the child `AgentRequest`, and downstream output tools preserve
  it with the existing `source_field` mechanism. An ownerless schedule/source
  can still run, but a mailbox filing from it fails closed.
- **Tool holds are out of scope.** `AgentToolApproval` stays the hold
  machinery. Mailbox does not materialize rows from `awaitingApproval`.

## Envelope

```text
MailboxItem                          whose mailbox, who filed
  ├── provenance                     where it came from
  ├── kind + action + title/summary  how to show / how to handle
  ├── target route                   where a request action goes
  ├── payload                        pack-specific JSON the generic client ignores
  ├── expected_collection            what create satisfies this item (optional)
  └── parent_item_id                 prior item, if this is a follow-up
```

### Schema (`crates/gents-schemas/schemas/agent/mailbox_item.graphql`)

```graphql
# TRUST BASIS — this row is an attention index. It grants no graph
# progress and no extra tool authority. Authority to file lives in the
# behavior's DatastoreToolSurface / runtime stamp path; authority to
# dismiss lives with requester_did. acted is server-written when the
# expected domain create exists.
#
# @branchable is REQUIRED: paired clients must receive the index over P2P.
type MailboxItem @branchable {
    item_key: String @index(unique: true) @immutable
    requester_did: String @index @immutable
    agent_did: String @index @immutable
    status: String @index

    kind: String @index @immutable
    action: String @index @immutable
    title: String @immutable
    summary: String @immutable
    payload: String @immutable

    source_kind: String @immutable
    source_id: String @index @immutable
    session_id: String @index @immutable
    request_id: String @index @immutable
    graph_run_id: String @index @immutable
    cause_doc_id: String @index @immutable

    target_agent_did: String @index @immutable
    target_behavior_id: String @index @immutable
    expected_collection: String @immutable
    parent_item_id: String @index @immutable
    deadline_at: DateTime @immutable

    created_at: DateTime @index(direction: DESC) @immutable
    updated_at: DateTime
    resolved_at: DateTime
    resolved_doc_id: String
}
```

Registration is a client/server schema-lineage change, not only an SDL include:

- Add the SDL/name to `gents-schemas` `ALL`, `ALL_COLLECTION_NAMES`, and
  `BRANCHABLE_COLLECTION_NAMES`.
- Re-export it and add it to the parallel `gents-protocol::schemas` `ALL` /
  `ALL_COLLECTION_NAMES` catalog used by runtimes and clients.
- Add a CID-pinned entry to `gents_migration::DEFAULT_BASELINE`. Because this
  is a brand-new collection, do **not** add an `AddCollection` step.
- Add `MailboxItem` to `CLIENT_AUTHORED_COLLECTIONS`. Its presence in the
  `machine` per-collection rules and client-to-runtime dismiss path puts it
  behind the #1125 fresh-apply schema-identity fence. It must not receive a
  later `PatchVersioned` / chained migration; future schema changes fold into
  the baseline and repin, as enforced by `fresh_apply_parity.rs`.

Empty lists in mutations remain `null`, never `[]`.

### Closed vocabularies

**`status`:** `open` | `acted` | `dismissed` | `expired`

**`kind`** (display): `ask` | `gate` | `finished` | `failed` | `flag`

**`action`** (client switch): `ack` | `start_request` | `write_document`

**`source_kind`:** `graph` | `session` | `agent` | `runtime` | `tool`

Unknown values fail the create (tool or runtime), they are not stored and
reinterpreted.

All fields except `status`, `updated_at`, `resolved_at`, and `resolved_doc_id`
are create-time envelope facts and are immutable. A terminal transition stamps
`updated_at = resolved_at = now`; `resolved_doc_id` is set only for `acted` and
remains `null` for `dismissed` / `expired`.

`start_request` and `write_document` require a resolvable route: either an
existing `session_id`, or both `target_agent_did` and `target_behavior_id`.
For the model-callable producer, the target defaults to the current request's
agent and behavior and is runtime-stamped rather than model-selected. A future
trusted runtime producer may supply a different operator-configured target.
`write_document` additionally requires an allowlisted `expected_collection`;
the request is only the transport that helps create that document. `ack` does
not require a target route. `start_request` / `ack` reject a non-null
`expected_collection` so the selected close engine is unambiguous.

### `item_key`

Stable idempotence token, runtime-stamped. The local unique index is the
same-node race backstop; creation authority plus source naming is the P2P
uniqueness boundary.

```text
{source_kind}:{source_id}:{kind}:{n}
```

`source_id` must be globally namespaced because `item_key` uniqueness is
collection-wide and intentionally does not include `requester_did`. Canonical
values are:

| `source_kind` | Canonical `source_id` |
| --- | --- |
| `graph` | Wait-node / expected-create document `_docID` when the item is a gate; graph-run `_docID` only for run-level `finished` / `flag` attention |
| `session` | `AgentSession._docID` |
| `agent` | Owner-scoped agent invocation/attention document `_docID`; never the shared agent principal DID |
| `runtime` | Originating runtime-event or `AgentRequest` `_docID` |
| `tool` | Owner-scoped tool/capability-use or revision document `_docID`; never a package hash shared by multiple owners |

Logical ids such as `session_id` / `graph_run_id` remain separate provenance
fields and are not used bare as `source_id`. Package content addresses such as
`sha256:<hex>` are provenance/payload, not the idempotence source. Two
same-`kind` gates in one graph run must name distinct wait/expected-create
documents; the run `_docID` would intentionally coalesce them.

`requester_did` remains outside the locked wire key shape. Owner-scoped source
documents make the key globally safe, while the lookup still includes and
verifies the requester. If a malformed producer reuses one source `_docID`
across owners, the second create reaches a key collision and fails closed; it
never returns the first owner's row.

`n` is the 1-based occurrence of that
`(requester_did, source_kind, source_id, kind)` tuple. The stamp helper queries
those exact fields (not a string-prefix filter) and assigns `n` as one plus the
greatest existing occurrence for that owner. If one **open** row already exists
for the tuple, the helper verifies `requester_did` before returning it and does
not create (`n` unchanged). That is in-flight retry idempotence. More than one
owner-matching open row is an invariant violation and fails closed. After the
open row is terminal, the next file gets `n+1`. No reopen.

On a unique-index race, the helper re-reads the exact `item_key` and returns it
only when its requester and source tuple match the attempted create; a
cross-owner or mismatched collision fails closed. DefraDB may retain
remotely-created unique twins while indexing one winner, so v1 relies on two
additional premises: every canonical source is owner-scoped, clients cannot
create mailbox rows, and one authoritative runtime owns filing for that source.
The Lean uniqueness theorem is about admitted creates under those premises,
not hostile raw P2P writes.

## Lifecycle

```text
            create (granted tool | explicit runtime producer)
                    │
                    ▼
                  open
                 /  |  \
                /   |   \
           acted  dismissed  expired
```

No admitted operation transitions out of a terminal status. No `open → open`
rewrite of envelope fields.

| Transition | Who | When |
| --- | --- | --- |
| (create) → `open` | Acting agent via granted write tool, or explicit runtime producer | Stamp path succeeds; `item_key` is new |
| `open` → `acted` | Runtime close sweep only | Expected domain create exists (see Close) |
| `open` → `dismissed` | Owner (`requester_did`) via client | Explicit dismiss. Does **not** write the domain document and does **not** unblock a parked graph |
| `open` → `expired` | Runtime close sweep | `deadline_at` is set and in the past, still `open` |

Through supported runtime/client APIs, owners cannot write `acted`, non-owners
cannot dismiss, and clients cannot create items in v1. These are admitted-write
contracts, not claims that the current local datastore rejects an arbitrary raw
GraphQL mutation: the pinned EmbeddedNode cannot yet install document ACP. A
full paired client is trusted in v1, consistent with the single-user trust
caveat below. The Lean model and Rust conformance tests fence every supported
writer; cryptographic storage enforcement remains dependent on installable
document ACP.

Transition helpers compare-and-set `status = open` on their local authoritative
view. The close sweep checks for a satisfying document before checking the
deadline, so an observed domain create wins over expiry. Concurrent terminal
writes made on disconnected replicas may still converge to any terminal under
DefraDB's LWW merge. That merge is not an admitted lifecycle transition, and
all terminal values are equivalent for graph safety: none is an edge and no
code may use a terminal-to-terminal merge to advance work.

### Lean fence

New model `Proofs/Mailbox/` (PersonaRequest-shaped: payload-abstract state,
`admits` / `applyStep`):

1. **Legal transitions** — only the three terminals from `open`; terminals
   are stuck.
2. **Tenancy** — `requester_did` (and `item_key`, `agent_did`, `source_kind`,
   `source_id`) frozen after create.
3. **Stamp soundness** — a create that does not carry the request’s
   `requester_did` / `agent_did` is not admitted. The model cannot choose
   the owner.
4. **Idempotence** — at most one **open** row per
   `(requester_did, source_kind, source_id, kind)`. `item_key` identifies at
   most one row ever. A create while an owner-matching open row exists for that
   tuple returns it; a create after that row is terminal mints the next `n`.
   These are theorems of admitted creates under the owner-scoped,
   single-authoritative-source premise; raw hostile replica writes are outside
   the machine.
5. **Close does not grant** — `acted` / `dismissed` / `expired` never imply
   a graph edge. Graph progress is a fact about other collections.
6. **Dismiss is owner-only** — `requester_did` must match the principal
   writing `dismissed`.
7. **Terminality** — under a fair successful sweep, every admitted open item
   with a deadline eventually expires unless it acts or is dismissed first;
   items without a deadline remain open until acted or dismissed (no forced GC
   in v1). Legacy/hostile rows outside the close allowlist are not liveness
   premises of the machine.

Zero `sorry`s. CoverageLedger entry, `Executable.lean` contract, Rust
conformance file, proofs README update.

This *does* start in Lean: it is a new lifecycle with tenancy and
idempotence obligations, not plumbing.

## Close

The mailbox is not the unblock document. Close correlates an **already
written** domain document back to the item.

| `action` | `acted` when |
| --- | --- |
| `start_request` | An owner-matching, route-matching interactive `AgentRequest` exists with `caused_by_source_doc_id` equal to this item’s `_docID` |
| `write_document` | A row exists in the item's allowlisted `expected_collection` whose mapped immutable correlation field equals this item's `item_key`. Creating, completing, or failing an `AgentRequest` never closes a `write_document` item |
| `ack` | Not auto-acted. The owner dismisses (or the item expires). Looking at a finished job is not a domain create |

`resolved_doc_id` records the satisfying `AgentRequest._docID` for
`start_request`, or the satisfying domain row `_docID` for `write_document`,
when status becomes `acted`.

If a pack’s GitHub path does not stamp `mailbox_item_key`, the phone row
stays `open` until dismiss. That is a pack bug, not a mailbox special case
for `Rid`.

There are exactly two close engines:

1. **Request close, `start_request` only.** A hardcoded query compares
   `AgentRequest.caused_by_source_doc_id` with `MailboxItem._docID` and also
   requires `execution_origin = interactive`, matching `requester_did`, target
   agent/behavior, and the item session when present. It does not consult
   `MAILBOX_CLOSE_COLLECTIONS`. An EventTrigger watching `MailboxItem` stamps
   the same source correlation on its scheduled child request but
   cannot auto-act the item it observed.
2. **Domain close, `write_document` only.** A static
   `MAILBOX_CLOSE_COLLECTIONS` map selects a registered collection and its
   immutable correlation field; the query compares that field with
   `MailboxItem.item_key`. In v1 mapped fields are conventionally named
   `mailbox_item_key`, but the map is authoritative.

Merely appearing in `ALL_COLLECTION_NAMES` is not enough: an eligible
collection must actually declare the mapped immutable correlation field. The
map's collection keys are validated once with
`graphql::validate_collection_identifier()` and its field values with
`graphql::validate_graphql_name()` before either is interpolated in identifier
position. Only the item key is a GraphQL string value and goes through
`graphql::escape_graphql_string()`. A model create naming an unsupported
`expected_collection` is rejected. An old or replicated row naming an
unsupported collection stays `open` and is logged rather than interpolated
into a query.

The sweep isolates row-local failures. A malformed/hostile item, ambiguous
domain correlation, or failed per-item transition is logged and counted as a
skipped error while later open items are still reconciled. Failure to load the
open set or validate the static close map remains a sweep-level error. Raw rows
are decoded individually so a nullable-field poison row cannot abort the set.

This platform slice does not add `mailbox_item_key` to `Rid` or invent future
programmable-tool approval schemas, so the production map may initially have
no direct-domain entries. Packs register an entry only in the same change that
adds the immutable correlation field to their schema. Sweep tests use an
injected/test-schema allowlist entry; no pack schema is smuggled into this
slice.

## Producers

No side door. An item appears because a `MailboxItem` is created.

### 1. Granted create tool

A `DatastoreToolSurface` (id `mailbox-writes`) exposes one create tool
`file_mailbox_item` → collection `MailboxItem`. It is a desired-state
asset, **not** auto-granted to every behavior. Operators (or packs) attach
it to the `ToolSelection` of behaviors that may ask a human, and of graph
stages that park / flag / finish. A behavior without the surface cannot
file. That is the operator ceiling.

Creates go through `mailbox::stamp_create`, not raw `BoundedWriteTool`.
The helper performs the mutation so stamped fields cannot leak into the
model-facing JSON schema.

This is enforced at the registration seam, not by convention. Any
`WriteToolDecl` whose collection is `MailboxItem` must exactly match the
canonical `file_mailbox_item` declaration. `ToolSurface::build_tools` builds a
dedicated `MailboxCreateTool` for that declaration, and rejects every other
declaration targeting `MailboxItem`; `BoundedWriteTool` must never receive that
collection. The surface remains the operator grant, but it cannot construct a
raw unstamped mailbox writer.

The model supplies: `kind`, `action`, `title`, `summary`, `payload`,
`source_kind`, `source_id`, optional `session_id` / `request_id` /
`graph_run_id` / `cause_doc_id` / `expected_collection` / `parent_item_id`
/ `deadline_at`. It does not supply the target route.

The runtime **stamps** (model-supplied values for these fields are
rejected, same as existing `fill`):

| Field | Source |
| --- | --- |
| `requester_did` | Owning requester on the current `AgentRequest`; graph stages receive it through explicit owner lineage |
| `agent_did` | Acting principal |
| `target_agent_did` | Current request's agent principal |
| `target_behavior_id` | Current request's behavior |
| `status` | always `open` |
| `item_key` | `{source_kind}:{source_id}:{kind}:{n}` as above |
| `created_at` | now |
| `updated_at` | same value as `created_at` |

`CurrentToolRuntimeContext` must carry `requester_did`, `agent_did`, and
`behavior_id` from the current `AgentRequest` (`session_id` is already in the
scope). Interactive requests already carry all three. For EventTrigger stages,
this slice must extend source capture and production materialization as
described above so graph-owned requests carry the stamped requester before the
tool runs. The stamp helper reads them and fails closed if any required value
is absent. Do not let the model pick the owner or request route. Do not add
mailbox-only variants to `WriteToolFieldFill` in this slice.

The shared bounded-write validator rejects a model-provided `requester_did`.
Domain documents may carry requester lineage only through an existing runtime
fill such as `source_field`; this keeps owner provenance transitive from the
authenticated entry instead of letting a producer forge the next stage owner.

Source ownership is optional at trigger materialization: an ownerless source
still fires its stage, but no requester is invented. If that stage invokes
`file_mailbox_item`, stamping fails closed. A present malformed owner remains a
source-capture error, including an empty or whitespace-only requester value;
an absent or GraphQL-null optional owner remains ownerless.

If an open row already exists for the prefix, the tool returns that
`_docID` and does not create (idempotent retry).

### 2. No generic runtime fallback in v1

The runtime does not currently emit a precise `terminal-with-attention` fact;
`inputRequired` is reserved protocol vocabulary, and `failed`, goal wrap-up,
and graph terminality do not by themselves say that a human action is wanted.
A generic persistence-hook fallback would therefore either spam ordinary
failures or guess an action. It is deferred.

A future runtime subsystem may file through the same stamp helper only from an
explicit, typed attention signal with a fixed mapping to `kind`, `action`,
canonical source, owner, and target route. It must use the same open-row
idempotence contract. A pack-filed `gate` / `flag` and a runtime fallback with a
different `kind` do not deduplicate, so any future broader suppression rule
must be specified independently of `item_key` before that producer ships.

### 3. Models never close

`acted` and `expired` are sweep-written. `dismissed` is owner-written.

### 4. EventTrigger

A pack *may* watch `MailboxItem` creates (the file event). It must not
watch status updates. Downstream work still keys off the domain create.

## Replication

Mailbox rows are index documents, same class as `AgentConversation` /
`AgentSession`.

- Add `MailboxItem` to `CLIENT_INDEX_COLLECTIONS` / `CLIENT_INDEX_RULES`
  (`requester_did` ← `PeerDid`) in both Rust
  (`crates/gents/src/agent/p2p_reconcile/templates.rs`) and Lean
  (`Proofs/ScopeTemplates`).
- Add `MailboxItem` to the `machine` template collections and rules the
  same way (the phone pairing already selects `machine`).
- Add `MailboxItem` to `CLIENT_COLLECTIONS`,
  `CLIENT_TO_RUNTIME_COLLECTIONS`, and both directions of the `ClientRoute`
  filter policy. `client_route_filters` in Rust and `clientRouteFilters` in
  Lean are hardcoded and both gain the same
  `(requester_did, agent_did)` predicate used for transcript rows; editing the
  collection lists alone is insufficient. Runtime-to-client carries creates
  and server terminal updates; client-to-runtime carries owner dismiss updates.
  This keeps the first-class mailbox promise true for `client` pairings as well
  as today's `machine` pairing.
- `index_collection_names()` in `gents-desktop-core` follows
  `CLIENT_INDEX_COLLECTIONS`, so the eager index pull includes mailbox
  heads after pair / reconnect, off the launch critical path. Desktop bootstrap
  currently destructures exactly two index names; replace that with iteration
  over the collection list and update its length-pinning tests.
- New items replicate as creates; `acted` / `dismissed` / `expired` replicate
  as updates. Dismiss is an owner write through the client API that replicates
  back.
- Do **not** invent a `MailboxHydrationRequest`. Session hydration stays
  for transcripts. The list renders from envelope fields only.
- Do **not** install per-item replicators (filter churn / replay, #1506).

Trust caveat: copy #1141’s wording into the template comment. Collection-
level sync is not tenancy.

The `machine` per-collection rule is also the executable
`CLIENT_AUTHORED_COLLECTIONS` coverage fence. Update that migration list and
its `fresh_apply_parity.rs` baseline pin in the same change; otherwise CI must
fail rather than permitting client/server collection-version divergence on a
dismiss update.

## Client surface

First-class on every client (iOS, desktop, CLI). Not a filter on the
session list. Sessions stay sessions.

**List row** (envelope only): title, `kind`, relative `created_at`,
provenance one-liner, `action`. Default list is `status = open`. History
filter later. Nav badge = count of open items for this `requester_did`.

**Tap** is keyed off `action`, not `kind`:

| `action` | Tap |
| --- | --- |
| `start_request` | Focus the existing session, or ensure one from `target_agent_did` + `target_behavior_id`, then open a compose surface carrying this item’s `_docID` as pending cause. Tap alone does not create an empty request and does not close the item. The next user-submitted `AgentRequest` is stamped with `caused_by_source_doc_id` = this item’s `_docID` |
| `write_document` | v1 uses the same routed compose UX as `start_request`, but request creation does **not** close the item. The item acts only when the agent/session or an external transport writes the correlated row in `expected_collection`. A native Rid form is a later shortcut, not a second unblock path |
| `ack` | Read-only detail (summary + jump to source). Primary button: dismiss |

Dismiss is always available on `open` items and writes `dismissed`.

CLI: `gents mailbox list|show|dismiss` over the same documents. `list` scopes to
the authenticated/principal DID; `dismiss` passes that DID into the same
owner-checking transition helper. A caller-supplied `--requester-did` is not an
authorization mechanism.

Bridge: list / dismiss / start-request commands generating the existing
TypeScript views. No UI-owned mailbox state.

The submit seam is explicit client data, not trigger lineage:
`SubmitRequestOptions`, the chat-send DTO, protocol
`CreateAgentRequestInput`, and `AgentRequestRow` gain optional
`caused_by_source_doc_id`. Only the compose surface opened from a mailbox item
sets it. Navigation to a different session, dismiss, or cancel clears the
pending cause. `TriggerLineage` remains reserved for runtime trigger
materialization.

The compose route remains sticky while independently replicated behavior and
session rows catch up; automatic selection repair does not discard correlation.
Only explicit navigation, dismiss, cancel, or successful submission clears it.

Before a client writes a request carrying a mailbox cause, the submission seam
reloads that item and requires it to be open, owned by the authenticated
requester, compose-capable, and routed to the submitted agent, behavior, and
session. Observer patch hydration includes `MailboxItem`, so replicated creates
and terminal status updates reach the live mailbox without a restart.

v1 UI non-goals: in-mailbox pack editors, merging with session “Needs
attention”, push notifications.

## Longer-term alignment: programmable tools

The longer-term user-authored-tool proposal adds content-addressed Afterburner
`.afb` packages as deterministic, capability-bounded graph stages. It needs
this mailbox, but it does not change the mailbox's role.

The mailbox indexes human attention around a tool/capability lifecycle; it is
not the package registry, capability manifest, approval record, execution log,
or retry edge. Those remain ordinary domain documents. Typical future filings
are:

| Programmable-tool event | Mailbox envelope | Domain action that carries semantics |
| --- | --- | --- |
| Package/capability revision needs manifest or publication review | `source_kind=tool`, `kind=gate`, `action=write_document` | Create the typed approval/publication decision document |
| Package asks for a wider `fs` / `net` / `env` / other manifold grant | `source_kind=tool`, `kind=ask`, `action=write_document` | Create a new capability-grant or denied-revision document; never mutate the reviewed baseline |
| Sandboxed execution exhausts its bounded retry policy and needs diagnosis | `source_kind=tool`, `kind=failed`, `action=start_request` | Create the correlated `AgentRequest`; any retry remains a separate domain request/document |
| A long-running tool-backed graph reaches an operator-significant result | `source_kind=graph`, `kind=finished` or `flag`, policy-selected action | Ack/dismiss, or create the graph's requested domain document |

`source_id` is the owner-scoped capability/revision document `_docID`. The
package content address (`sha256:<hex>`) remains immutable provenance in that
document and may be summarized in the envelope `payload`, but it is not the
mailbox idempotence source because the same package can require attention from
multiple owners. A generic client does not fetch or execute the package to
render the list.
Package bytes, source archives, credentials, and secret environment values
never belong in the mailbox payload.

Ordinary successful deterministic stages do not file mailbox items. Neither
does every fuel, memory, timeout, or network error: the graph/tool runtime files
only after an explicit operator policy says the bounded failure needs human
attention. That future producer must satisfy the typed-runtime-producer contract
above; raw terminal status is still not enough.

Publication/grant review is not `AgentToolApproval`, which remains the hold
machinery for an individual model tool call. Dismissing a programmable-tool
mailbox item does not approve capabilities, publish a package, retry execution,
or advance a graph. A typed domain create does. Future approval collections are
added to `MAILBOX_CLOSE_COLLECTIONS` only when their schema includes the
immutable mailbox correlation field.

## Error handling

| Failure | Behavior |
| --- | --- |
| Tool call without mailbox surface | Ordinary missing-tool; model cannot file |
| Model supplies a stamped field | `MailboxCreateTool` allowlist reject; no row |
| Missing `requester_did` / `agent_did` in request context | Fail closed; no row |
| EventTrigger source lacks stamped `requester_did` | Mailbox filing fails closed; never substitute the acting agent DID |
| Unknown `kind` / `action` / `source_kind` / `status` | Reject the supported write |
| Empty `title` or `source_id` | Reject create |
| Missing route for `start_request` / `write_document` | Reject create |
| `write_document` lacks allowlisted `expected_collection` | Reject create |
| `start_request` / `ack` supplies `expected_collection` | Reject create; close engine must be unambiguous |
| Duplicate `item_key` with matching owner/source | Return existing row; not an error |
| Duplicate `item_key` with mismatched owner/source | Fail closed; invariant error |
| More than one open row for an owner/source tuple | Fail closed; log invariant violation |
| Owner dismisses already-terminal item | No-op success (idempotent) |
| Non-owner dismiss | Reject; row unchanged |
| Client writes `acted` | Reject; row unchanged |
| Create names unsupported `expected_collection` | Reject create |
| Close sweep sees legacy/replicated unsupported `expected_collection` | Leave `open`; log; do not query it or expire for that reason |
| Close sweep GraphQL error | Retry on the ordinary reconcile cadence; do not mark expired |
| Pairing without mailbox in template (old peer) | Items exist on the server; that client shows an empty mailbox until the pairing template upgrades (one teardown/reinstall/replay, same class as #1141 machine upgrade) |

Never emit `[]` in mailbox mutations. Sweep queries filter `status: {_eq:
"open"}` and interpolate ids with `graphql::escape_graphql_string()`.

## Testing

Foundation flow: Lean → conformance → Rust.

- **Lean:** transitions, stamp/tenancy, idempotence, owner-only dismiss,
  close-does-not-grant, deadline expiry. Conformance file pins the
  exported contract JSON.
- **Schema / lineage:** unique `item_key`; `@immutable` on every envelope field
  except `status`, `updated_at`, `resolved_at`, and `resolved_doc_id`;
  `@immutable` on every replication-filter field; matching `gents-schemas` and
  `gents-protocol` catalogs; branchable registration; CID-pinned
  `DEFAULT_BASELINE`; `CLIENT_AUTHORED_COLLECTIONS`; no `AddCollection` or
  chained step; `fresh_apply_parity.rs` passes.
- **Stamp / tool:** unit tests that a granted `file_mailbox_item` cannot set
  stamped identity/target fields; context-less stamping fails; a raw
  `BoundedWriteTool` declaration for `MailboxItem` is rejected; a matching
  owner-scoped duplicate returns the first `_docID`; an open row owned by a
  different requester is never returned; mismatched and multi-open collisions
  fail closed; behavior without the surface has no tool.
- **Graph owner lineage:** an authenticated graph entry stamps immutable
  `requester_did`; EventTrigger source capture propagates it to the child
  `AgentRequest`; downstream output preserves it; a missing/malformed owner
  fails mailbox filing closed without substituting the agent DID.
- **Close sweep:** the hardcoded request engine acts `start_request` only when
  the owner/route-matching interactive caused `AgentRequest` exists; a
  scheduled EventTrigger child caused by the MailboxItem does not act it;
  creating/completing/failing that request
  does not act `write_document`; an allowlisted domain row whose mapped field
  equals `item_key` does; missing stamp leaves `open`; dismiss does not act;
  non-owner dismiss is rejected; map identifiers are validated before
  interpolation; one poisoned row does not block later items; satisfying
  create wins over deadline in one sweep.
- **Scope templates:** Lean + `scope_templates.rs` pin `MailboxItem` on
  `client-index`, `machine`, and both `client` directions with
  the specified predicates. Rust `client_route_filters`, Lean
  `clientRouteFilters`, the machine/client-authored fence, branchable catalog,
  and iterative desktop index bootstrap are pinned too.
- **No fallback:** ordinary failed/completed/wrap-up requests do not
  implicitly file mailbox rows.
- **UI / submit:** mailbox list renders open items from envelope fields; tap
  opens the routed compose surface without creating an empty request; the next
  submitted request carries `caused_by_source_doc_id` through
  `SubmitRequestOptions` / protocol input; session navigation, cancel, and
  dismiss clear it; direct submission rejects stale, foreign, or misrouted
  mailbox causes; live observer patches hydrate mailbox creates and updates;
  `write_document` remains open until its domain row exists; dismiss writes
  `dismissed`; badge counts open rows. Desktop/iOS share the same client
  package API.
- **CLI:** `mailbox list` shows open items for the caller.

Gates: `cargo test -p gents` (includes conformance), targeted desktop
package tests for the list, `cargo check --workspace --all-targets` before
push. No NASA/SDLC pack e2e in this slice.

## Non-goals (this slice)

- Wiring `packs/sdlc/` or any graph pack to the surface (they consume it
  later).
- `AgentToolApproval` / HoldsPanel integration.
- Push notifications, email, or GitHub as a mailbox transport (GitHub
  remains a *domain* transport that should stamp `mailbox_item_key`).
- Native in-mailbox document editors.
- Paginated tenant-safe index protocol (the #1141 preferred end state).
- `event_kind: updated`.
- Evicting or GC’ing terminal mailbox rows.
- Clients creating mailbox items.
- Implementing the programmable-tool registry, capability/grant schemas, WASM
  execution bridge, or its runtime producer. This spec only reserves the
  compatible attention envelope and source vocabulary.

## Files (implementation map, not the plan)

| Area | Where |
| --- | --- |
| SDL / catalogs | `crates/gents-schemas/schemas/agent/mailbox_item.graphql` + `lib.rs`; `gents-protocol/src/schemas.rs` |
| Schema lineage | `gents-migration::DEFAULT_BASELINE` CID pin + `CLIENT_AUTHORED_COLLECTIONS`; `fresh_apply_parity.rs`; no migration step |
| Lean | `crates/gents/proofs/Proofs/Mailbox/` + CoverageLedger + Machines catalog |
| Stamp / close | `crates/gents/src/mailbox/` (create helper, hardcoded request closer, allowlisted domain closer, sweep) |
| Tool surface | bundled `mailbox-writes` DatastoreToolSurface; desired-state example |
| Runtime context | `CurrentToolRuntimeContext` gains `requester_did` / `agent_did` / `behavior_id`; stamp helper reads them |
| Graph owner lineage | EventTrigger source capture + `production_materializer.rs` + `lifecycle/materialize.rs`; fixed immutable `requester_did` propagation |
| Pairing | `templates.rs`, Rust `policy.rs::client_route_filters`, Lean `Proofs/ScopeTemplates`, client-authored fence, branchable catalog, iterative desktop index bootstrap |
| Submit correlation | `SubmitRequestOptions`, chat-send DTO/mutation, protocol `CreateAgentRequestInput` + `AgentRequestRow`; never `TriggerLineage` |
| Bridge / TS | list, dismiss, routed-compose views/commands |
| UI | desktop/iOS mailbox surface in the existing shell; not inside ChatWorkspace |
| CLI | `gents mailbox list\|show\|dismiss` |

## Decisions from this session

1. **New collection, not a live projection.** Producers write `MailboxItem`
   documents; clients subscribe.
2. **Owner is the human who owns the work** (`requester_did`), not a shared
   server inbox.
3. **General envelope.** Provenance + handling + follow-up on the item.
   Pack-specific meaning lives in `payload` / domain collections, not extra
   mailbox types.
4. **Transport ≠ semantics.** Acting writes ordinary domain documents (or
   starts an `AgentRequest`). Mailbox status is not a graph edge.
5. **v1 tap:** `write_document` uses the same routed compose UX as
   `start_request`, but only its correlated domain create closes it. Forms
   later.
6. **Tool holds are a cousin, not a producer.**
7. **Follow-ups:** parent pointer plus “the graph files the next item.”
   No checklist engine on the row.
8. **Programmable tools use the same envelope.** Package/capability approval,
   permission escalation, and policy-selected terminal failures may file
   mailbox rows; typed approval/retry/publication documents remain semantics.
9. **No heuristic fallback.** Runtime filing requires a future explicit typed
   attention signal; ordinary terminal state is insufficient.
10. **Graph owner is lineage, not inference.** Graph entry/source documents
    carry requester ownership from an authenticated principal through trigger
    materialization; an agent DID is never substituted for a missing human.

## Build order

1. Lean lifecycle + conformance contract.
2. Schema + both catalogs + baseline/client-authored fence + pairing templates
   and policies (Lean and Rust).
3. Graph owner lineage + runtime-context propagation.
4. Stamp helper + create tool + owner-scoped duplicate-key idempotence.
5. Two close engines + sweep + owner dismiss.
6. Submit correlation + bridge + CLI.
7. Client list / badge / routed compose / dismiss.

Each layer is independently testable. UI does not ship before the
documents and sweep exist.
