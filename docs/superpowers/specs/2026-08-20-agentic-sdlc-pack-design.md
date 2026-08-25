# Agentic SDLC pack — design

**Status:** design; open questions resolved in review on PR #1161 (2026-08-21).
**Prototype target:** gents itself — GitHub issues labelled `sdlc:<A|B|C|D>`, driven against
`workstation-1:8000` (GLM-5.2). One V, small-app to feature-integration scope;
recursive V-of-Vs is out of bounds.

## The bet

Building software is the same craft it was twenty years ago: decompose a need, agree
interfaces, implement, verify, validate. Heavyweight process — V-model, ASPICE, ECSS,
ISO/IEC/IEEE 12207 — did not lose to agile because it was wrong. It lost because its
overhead (traceability matrices, keeping contracts in sync, artefact generation) is too
expensive for projects without clear requirements and strong up-front design.

Agents are cheap and fast at exactly that mechanical overhead. Collapse its cost and the
trade-off flips: rigour once affordable only for spacecraft becomes affordable for normal
work.

So the job is not to design a methodology. It is to **compile an existing one into agents** —
lift ECSS-E-ST-40C and 12207 clause by clause and re-cast each activity as a bounded agent
with defined inputs and outputs.

The V, re-read as a value gradient: human judgement is worth most at the two top corners,
ideation and validation, which stay hard human gates. The trough — architecture, component
design, implementation, verification — is delegated, but **reclaimable**: a human can reach
into any node and override it, and the machinery re-derives whatever that change
invalidated downstream.

Gents is an unusually good substrate for this because the control plane is already a
document database. Every ECSS artefact is a document, every activity is a trigger edge, and
DID-signed provenance means the traceability matrix — the single most expensive artefact in
the standard — falls out of the persistence layer instead of being maintained by hand.

## Constraints established by research

Verified against this tree and the two standards, not assumed:

- **`event_kind` is `created`-only.** `crates/gents-cli/src/desired_state/validate.rs:727`
  hard-rejects anything else: *"v1 supports only \"created\""*. This does **not** block the
  reclaimable-node property — see [Revision, not mutation](#revision-not-mutation).
- **GitHub is already reachable via shell.** Packs call `gh pr create`, `gh pr view`,
  `gh pr checks` directly from task prompts
  (`demo/repo-maintenance/tasks/maintenance-publish-task/prompt.md:17`,
  `demo/code-review/tasks/review-recon-task/prompt.md:9`). Issue-comment ingest needs a task,
  not a new tool or integration.
- **`GLM-5.2` is already the pack default** (`demo/repo-maintenance/agent-behaviors/
  maintenance-publish/object.json`), so retargeting to `workstation-1:8000` is an endpoint
  variable, nothing more.
- **Clients render replicated documents, not bespoke views.** The desktop app "consumes the
  replicated document surface through `gents-desktop-core`", and `apps/fixture-host` already
  runs `BootstrapPolicy::PairedRemoteOnly`. RB/TS/DDF documents appear in a paired client
  because they replicate. Mobile/web review is a pairing problem, not an integration.
- **`demo/repo-maintenance/` is the structural template.** Six nodes, fan-out scanners, an
  adversarial verification barrier, count-balanced work-package accounting, worktree
  isolation, a real PR, and a bounded CI repair loop. This pack is that graph with a
  different seed and a longer artefact chain.
- **The standard is pre-annotated for this.** Every ECSS clause tags its outputs
  `[file, DRD; review]` — e.g. `Software architectural design [DDF, SDD; PDR]`. Output
  collection and gating review are given, not inferred.

## Scope note: this does not touch Lean

A pack is configuration applied to an already-proven runtime. Trigger dispatch, the request
lifecycle, and fan-in accounting are Lean-fenced today; this work composes those existing
transitions rather than adding new ones. The V's state machine lives in the schema graph —
which collection a node may write, and which edge fires next — not in runtime semantics.

If the prototype turns up a required runtime change (a new event kind, a new trigger
predicate), that change follows the normal foundation flow and starts in
`crates/gents/proofs/`. The design below is deliberately built to avoid needing one.

## The V as a document DAG

The pack primitives map onto the standard with almost no impedance:

| ECSS concept | Gents primitive |
| --- | --- |
| Process activity (e.g. §5.4.2) | Task + behavior |
| Expected output `[DDF, SDD; PDR]` | A document in the `DDF` collection |
| Activity precedence | `EventTrigger`, `event_kind: created` |
| What an activity may produce | `DatastoreToolSurface` on its `ToolSelection` |
| Joint review (§5.3.3) | A review behavior + its RID ledger |
| Review item discrepancy | A `Rid` document with a disposition and change class |
| Configuration management baseline | `supersedes` chain, append-only |
| Traceability matrix | `derived_from` edges, DID-signed |
| Annex R tailoring | A `TailoringRule` collection, queried at dispatch |
| Change classification (§4.3.3.4) | `Rid.change_class` under job eagerness |

### Revision, not mutation

The obvious reading of "a human overrides a node" is an in-place edit, which would need an
`updated` trigger we do not have. That reading is wrong on the standard's own terms.

ECSS §5.3 and 12207 §6.3.5 both put artefacts under configuration management: you do not
edit a requirements baseline, you issue a **superseding revision** under change control. So
an override is:

```
create RequirementsBaseline { supersedes: <prior_doc_id>, rationale: …, author: <human DID> }
```

which fires a `created` edge, re-derives downstream, and leaves the prior revision in the DAG
as signed history. The append-only constraint of the trigger engine and the change-control
discipline of the standard are the same constraint. No runtime gap.

Downstream re-derivation is scoped by `derived_from`: a node re-runs when any document it
derived from has been superseded. That is a query, not a subscription. Whether it re-runs
*now* or waits is a change-class decision — see [Change classification](#change-classification).

## Collections

ECSS's own file taxonomy, one collection each:

| Collection | ECSS file | Holds |
| --- | --- | --- |
| `RequirementsBaseline` | RB | One document **per requirement**, individually supersedable |
| `TechnicalSpecification` | TS | Software requirements, derived from RB |
| `InterfaceControl` | ICD | External interfaces |
| `DesignDefinition` | DDF | Architecture and per-item design (SDD), build config (SCF) |
| `DesignJustification` | DJF | Verification/validation specs and reports — the *why* |
| `ManagementFile` | MGT | Plans, schedule, tailoring decisions, eagerness |
| `Rid` | — | Review item discrepancies, with disposition and change class |
| `SdlcJob` | — | Seed; carries issue number, criticality, eagerness, head SHA |

Every artefact document carries:

- `derived_from: [doc_id]` — upstream provenance; the traceability matrix is `SELECT` over
  this, never a maintained artefact
- `supersedes: doc_id | null` — revision chain
- `ecss_clause: string` — e.g. `5.4.3.1a`, the stable requirement id from the standard
- `criticality: A | B | C | D`

Granularity matters: **one document per requirement**, not one document per baseline. A human
who disagrees with requirement 7 supersedes requirement 7, and only what derived from
requirement 7 re-derives. Coarse documents would make every override re-run the whole V.

## Nodes

Lifted from ECSS-E-ST-40C clause 5. Each is a behavior whose write surface is exactly its
expected outputs — the requirements node *physically cannot* write a `DesignDefinition`.

| # | Clause | Node | Reads | Writes |
| --- | --- | --- | --- | --- |
| 0 | — | `sdlc-intake` | GitHub issue | `SdlcJob`, `ManagementFile` (tailoring + eagerness), `Rid` when arguing |
| 1 | §5.2.2 | `sdlc-needs` | issue body | `RequirementsBaseline`, one doc **per need** |
| 2 | §5.8.3.1 | `sdlc-verify-rb` | RB | `DesignJustification` (SVR) → feeds **SRR** |
| — | §5.3.3 | **SRR — hard human gate** | RB, DJF, MGT | `Rid` dispositions |
| 3 | §5.4.2 | `sdlc-requirements` | RB | `TechnicalSpecification`, `InterfaceControl` |
| 4 | §5.8.3.2 | `sdlc-verify-ts` | TS, RB | `DesignJustification` (SVR) |
| 5 | §5.4.3 | `sdlc-architecture` | TS | `DesignDefinition` (SDD) — **declares items** |
| 6 | §5.5.2 | `sdlc-item-design` | DDF, TS | `DesignDefinition` per declared item — **fan-out** |
| 7 | §5.6.3 | `sdlc-validation-spec` | TS | `DesignJustification` (SVS vs TS) |
| 8 | §5.5.3 | `sdlc-code` | DDF | commits in an isolated worktree, `DesignJustification` (unit test reports) |
| 9 | §5.8 | `sdlc-verification` | code, TS | `DesignJustification` (SVR) — adversarial |
| 10 | §5.6.4 | `sdlc-validation` | code, **RB** | `DesignJustification` (SVS vs RB) |
| 11 | — | `sdlc-publish` | everything | one PR, CI repair loop |
| — | §5.3.3 | **AR — hard human gate** | MGT, DDF, DJF, MF | merge, or `Rid` dispositions |

The RB/TS split is load-bearing and is the standard's own. The **requirements baseline is the
customer's** (§5.2) — what the human actually needs, derived from the issue and approved at
SRR. The **technical specification is the supplier's** (§5.4.2) — how the software will
satisfy it. Collapsing them would leave the SRR gate with nothing to approve and would give
node 10 nothing independent to validate against.

Node 3 splits ECSS §5.4.2.1's ten expected outputs (functional/performance, operational and
reliability, quality, security, human factors, data definition, validation requirements,
external interfaces, reuse, security risk treatment) into individually-addressable TS
documents.

Nodes 6 and 8 fan out per software item and fan back in on a count-balanced barrier — the
work-package pattern `repo-maintenance` already proves, including the rule that document
arrival order is not an execution callback. The architecture node **declares** the items;
fan-out width is a property of that `DesignDefinition`, not a pack constant. For v1 the
declaration is bounded to **1–6** items. Annex K.4 treats over- and under-selection as
pathological modes and supplies the criteria the architecture behavior uses to decide
whether something is its own area.

### Argument, cascade, alignment

**Intake argues.** If an issue is too vague to produce a requirements baseline,
`sdlc-intake` does not fail the run. It posts clarifying questions through the GitHub
bridge, writes an inbound `Rid`, and parks until a human replies. That loop is the
highest-value exercise on the V — ideation refinement — and it may take several turns
before node 1 runs.

The same pattern repeats at every left-side node. A child that finds a gap writes a
change proposal to its **adjacent parent only**. It never addresses a non-adjacent node.
The parent answers from its own artefacts, supersedes its own artefact, or cascades one
hop further up. Distance is unbounded; skip-level talk is not. This is ECSS §4.3.3.4: a
change proposal is processed through the levels of the organisation, and disposition sits
at the lowest level whose effects have no repercussions on commitments made to the
customer.

Write-surface fences already enforce adjacency: `sdlc-item-design` cannot write a
`RequirementsBaseline`. The cascade is a `Rid` (or a superseding parent document), not a
skip-level mutation.

**Alignment.** Agents on the **left of the V** are aligned with implementors against the
specs they inherited — they argue when an input artefact is incomplete, or when a more
efficient realisation of the same goal needs the spec updated. Agents on the **right of
the V** are aligned with the specs against the delivered artefacts — they argue with the
implementors.

### Why verification and validation are separate nodes

They look redundant and are not. ECSS separates them precisely:

- **§5.6.3** — validation *with respect to the technical specification* → `[DJF, SVS; CDR]`
- **§5.6.4** — validation *with respect to the requirements baseline* → `[DJF, SVS; QR, AR]`

Verification asks *did we build what the spec says*. Validation asks *does the spec answer
the need in the issue*. CI only ever does the first. The second is exactly where an agent
loop goes wrong — a perfectly-implemented misreading of the issue — and the standards have
kept them apart for forty years for that reason. Node 10 reads the RB and the *issue*, never
the TS.

## Review gates

### The loadout is specified

ECSS Figure 4-2 gives each review its inputs. This is the context each review behavior
loads — read off the table, not invented:

| Review | Inputs |
| --- | --- |
| SRR | RB, DJF, MGT |
| SWRR | TS, DDF, DJF, MGT |
| PDR / DDR | TS, (ICD), DDF, DJF |
| CDR | DJF |
| QR / AR | MGT, DDF, DJF, MF |

### The exit criteria are specified

§5.3.3.1a is a six-clause checklist that reads almost verbatim as a system prompt. Joint
reviews evaluate progress and provide evidence that:

1. the outputs are complete;
2. the outputs conform to applicable standards and specifications;
3. any changes are properly implemented and impact only those areas identified by the
   configuration management process;
4. the outputs conform to applicable schedules;
5. **the outputs are in such a status that the next activity can start**;
6. the activity is being conducted according to plans, schedules, standards and guidelines.

Clause 5 is literally the edge condition. A review's job is to decide whether the downstream
trigger fires.

### RIDs are the output

ECSS reviews emit review item discrepancies on a form (§ Annex, per ECSS-M-ST-10-01), each
one *dispositioned*. So a review behavior's write surface is the `Rid` collection, and
"review passed" means the RID ledger is closed — the same count-balanced barrier
`repo-maintenance` uses for work packages.

### Two hard gates, five soft

All seven reviews run as nodes. Only two are **hard human gates**, per the value gradient:

- **SRR** — the requirements baseline. Ideation. Nothing proceeds without a human.
- **AR** — the PR. Validation. Nothing merges without a human.

The other five (SWRR, PDR, DDR, CDR, QR) run as agent reviews that emit RIDs. A human may
reach in and re-disposition any RID at any time; that creates a superseding document, fires
a `created` edge, and re-derives downstream. Reclaimable without being blocking.

For `sdlc:D`, Annex R may switch SRR off. The value gradient still keeps the ideation tip
as a hard human gate; that is an **explicit tailoring override** recorded in the job's
`ManagementFile`, not an accident of the Table R-1 query.

## Change classification

ECSS §4.3.3.4 classifies a change by blast radius: whether it goes back to the customer
or is handled internally. For the pack the originator is the GitHub issue. Pre-approval
(descending the V, artefacts not yet baselined) and post-approval use the **same classes**
in v1; the distinction is not load-bearing for us.

Three classes, recorded on the `Rid`. The class is selected under an **eagerness** policy
on the `SdlcJob` / `ManagementFile` — it configures how eagerly the stack approaches work,
not a per-node special case.

| Class | What it means | Re-derivation |
| --- | --- | --- |
| 1 | Hits the originator artefact. Escalate to a human; work on that branch waits. | Parked |
| 2 | Intermediate artefact. Log the decision **and** flag it for human review; work continues on the agent's call. A later human `Rid` can invalidate it. | Eager, optimistic |
| 3 | Answered entirely from existing artefacts. Note the decision; do not elevate. | Eager |

This replaces the binary "re-derive eagerly vs. mark stale." Class 1 is the wait path.
Class 2 and 3 re-derive. Waiting for a human and doing the wrong thing can run in parallel
under class 2: the decision is visible, work continues, the human can still pull it back.

## Human surfaces

**The transport is not the semantics.** A GitHub comment and a paired mobile session both
land the same document — a `Rid` with a disposition, or a superseding revision. Neither is
the privileged channel.

### GitHub bridge (prototype)

Outbound: a node posts its artefact summary as an issue comment. Inbound: a polling task runs
`gh issue view --comments --json` and converts human replies into `Rid` documents.

Two sharp edges to design rather than discover:

- **Watermark on comment id.** Triggers are created/first-seen; a re-poll that re-ingests an
  old comment re-fires the graph. The bridge persists a high-water mark per issue.
- **Filter by comment author.** The agent posts comments to the same thread it polls. Without
  an author filter it ingests its own output and loops.

### Paired session (next)

A behavior with `request_context_template` scoped to a single artefact document, so a human
can open a session against *this TS* from desktop or mobile, talk it through, and have the
agent write the superseding revision. Requires no new plumbing: the documents are already
replicating to any paired client, and `PairedRemoteOnly` already exists.

## Tailoring

Annex R (normative) is a pre-tailoring of the whole standard by software criticality category
(A–D, per ECSS-Q-ST-80 Annex D.1). Table R-1 is keyed by stable requirement id, one row per
expected output:

```
Requirement id    Expected Output                                   A   B   C   D
5.2.2.1a eo a     Functions and performance system requirements…    Y   Y   Y   Y
```

- `Y` — activity and expected output are required
- `N` — neither is required
- `Ytba` — some DRD information may be omitted if justified

So the GitHub label is not an on/off switch for the workflow; it is a **criticality
category**, and the category selects the subgraph. `sdlc:D` on a bug fix runs requirements →
code → verify. `sdlc:A` runs the full V with architecture, design justification, and every
review. SRR is the exception: even when Table R-1 would omit it at category D, the pack
keeps it as a hard human gate and records that override in MGT.

Table R-1 becomes a `TailoringRule` collection (`ecss_clause`, `expected_output`, `category`,
`applicability`). Node activation is a query against it, and the tailoring decision for a
given job is recorded in its `ManagementFile` — which is what ECSS requires anyway.

## Prototype scope

Current target is **one V** for a small-app or feature-integration job — roughly 2–4 weeks
of human work against an established codebase, issue in and PR out. 12207 notes that its
life-cycle processes can be applied "concurrently, iteratively and recursively"; recursive
**V of Vs** (sub-Vs under a parent V, with today's human gates falling to the parent) is a
bigger bet and is explicitly out of bounds for this pack.

On `agent/space-dev`, as `demo/sdlc/`, following the existing pack layout (`schemas/`,
`agent-behaviors/`, `datastore-tool-surfaces/`, `tool-selections/`, `event_triggers/`,
`tasks/`, `experiment.json`).

Build order, because each layer fences the next:

1. **`schemas/`** — the collections *are* the state machine. Get `derived_from`, `supersedes`,
   `ecss_clause`, `criticality`, `change_class`, and job `eagerness` right; a node is then
   just a behavior reading one collection and writing another. Remember: never emit `[]` in
   a DefraDB mutation, emit `null`.
2. **`datastore-tool-surfaces/`** — the real safety fence, one per node.
3. **`agent-behaviors/`** — system prompts, several lifted near-verbatim from clause text.
4. **`event_triggers/`** — the edges, `created`-only.
5. **`tasks/`** + the GitHub bridge.
6. **`TailoringRule` rows** for Table R-1.

Then run it end to end on one real labelled gents issue against `workstation-1:8000`.

**Deferred to follow-up work:**

- Paired-session review from desktop/mobile (documents already replicate; needs the
  artefact-scoped behavior)
- Packaging the pack as a `make` target consumable by *other* repos — the distribution story.
  It is a separate concern, but building this pack first is what discovers its requirements.
- 12207 organizational and agreement processes (§6.1, §6.2) — out of scope; this pack covers
  the technical processes (§6.4) only.
- Recursive V of Vs / jobs larger than a 2–4 week feature. 12207 permits it; this pack does
  not.

## Decisions from review

Resolved on PR #1161. The original questions are kept here so the answers stay attached
to the forks they closed.

1. **Fan-out width for `sdlc-item-design`.** Declared by the architecture node, not
   hardcoded. v1 bound is 1–6. Selection criteria from Annex K.4.
2. **Does the intake node get to argue?** Yes, it must. Vague issues park on an inbound
   `Rid` rather than failing the run. Cascade is adjacent-only, any distance up the left
   side. Left-of-V agents argue the spec; right-of-V agents argue the implementation.
   See [Argument, cascade, alignment](#argument-cascade-alignment).
3. **Re-derivation blast radius.** Not a binary. ECSS §4.3.3.4 change classes, folded
   across pre/post approval for v1, recorded as three classes on the `Rid` under a
   job-level eagerness policy. Class 1 parks; class 2 re-derives and flags; class 3
   re-derives quietly. See [Change classification](#change-classification).
4. **SRR for `sdlc:D`.** Value gradient wins: SRR stays a hard human gate, as an explicit
   MGT tailoring override of Annex R. Recursion / V-of-Vs is out of bounds; current target
   is one V for a 2–4 week feature.

## References

- ECSS-E-ST-40C Rev.1 (30 April 2025) — Space engineering: Software. Clause 5 (processes),
  §4.3.3.4 (change classification), Figure 4-2 (review/artefact map), Annex K.4 (item
  selection), Annex R (criticality tailoring).
- ISO/IEC/IEEE 12207 — Software life cycle processes. §6.4 technical processes.
- *Something old, something new: can established software-engineering process serve as a
  template for agentic development loops?*
- `demo/repo-maintenance/` — structural template.
- `demo/code-review/` — fan-out/fan-in with correlation propagation.
