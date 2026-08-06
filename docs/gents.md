# Gents

**Recursive self-improvement for organizational AI.**

*v1 — 2026-07-20. Supersedes the Agentic Governance doc series (v4); the governance material survives here as Part 3. Gents is the platform; its runtime ships as `gents`.*

---

## The Problem

Organizations are deploying AI agents into real operations — triaging alerts, watching infrastructure, drafting changes, coordinating work across teams and systems. Two walls stop them from getting what they actually want.

**The first wall: the agents don't get better.** A deployed agent today is a frontier model, a prompt, and a pile of integrations. It performs the same on day 400 as on day 4. Everything it learns about your operation — which alerts matter, which runbooks work, how your systems actually fail — evaporates at the end of every session. Improvement means a human editing a prompt. The institutional knowledge accumulates in a vendor's next model, not in your system.

**The second wall: the loop that would fix this is the loop nobody can run safely.** Everyone can sketch it: capture what the agents do, fine-tune smaller models on the traces, deploy those models back into the system, let agents adjust their own configuration as they learn what works. That is recursive self-improvement, and organizations are right to want it — a system that compounds on its own operational experience is worth more every month it runs. They are also right to fear it. On today's stacks, the loop is unaccountable at every stage: traces are partial application logs, training data provenance is a spreadsheet, deployment is a config push, and "the agent modified its own setup" is an incident report. You cannot answer the questions that make the loop tolerable: *what exactly did the agent see, what did it do, who approved the change, and can you prove it never exceeded its authority?*

Gents exists to tear down both walls at once. It is an agent platform where the self-improvement loop is a **property of the architecture** — every stage of the loop is a governed, traceable operation with a durable, attributable record — so organizations can run recursive self-improvement on their AI systems the way they run CI on their code: continuously, observably, and with human approval on everything that matters.

## The Flywheel

Gents is built on a governed data layer: DefraDB, a CRDT document store with P2P replication, backed by a trust stack providing identity-based encryption and access control (Part 3 describes it; Current State records what is wired on by default). Everything in the platform is a document — configuration, requests, responses, tool calls, goals, schedules, approvals. That single decision is what makes the loop closeable. Two terms recur in this document with distinct meanings: **document-governed** — durable, attributable, document-driven, what the shipping runtime does today — and **cryptographically enforced** — filtered by access-control policy (ACP), identity-encrypted, threshold-signed, what the fully integrated trust stack adds. Current State maps which is which; from here on, *governed* without qualification means document-governed.

```
        ┌──────────► OPERATE ──────────┐
        │      agents do real work     │
        │                              ▼
     DEPLOY                         TRACE
  fine-tuned models          every step a durable,
  behind document-           attributable document
  described backends;        written under the
  hot reload, no restart     acting identity
        ▲                              │
        │                              ▼
        └──────────  TRAIN  ◄──────────┘
             training-safe projections
             of the system's own traces

        every edge crosses the same
      governance layer: identity, audit,
      approvals — cryptographically
      enforced as the trust stack wires on
```

Each stage maps to a concrete runtime or trust-stack mechanism; Current State distinguishes the paths shipping today from the remaining integration work.

### Operate → Trace: every interaction already has the spine of a training record

A Gents agent cannot act off the record, because acting *is* writing records. Every request, streamed response, inference call, tool invocation, tool result, subagent spawn, and compaction step is a document, written under the agent's cryptographic identity — a decentralized identifier (DID) backed by real keys — and encrypted and ring-signed once the trust stack is wired on. The runtime reconstructs a request's event timeline — request, messages, tool calls, inference calls, response — from persisted state alone: no side-channel logs to collect, no instrumentation to install. Everything else that happened — compactions, approvals, goal transitions — is durable in adjacent collections under the same governance, waiting on the same projection surface.

Two details make this trace path training-ready rather than merely observability-grade:

- **The exact provider input is capturable at one verified point.** Every completion request in the system is assembled at a single, formally verified entry point, and the runtime exposes a capture surface for the rendered request — precisely what the model saw — at that point. A trace that pairs the true model input with the model's output and its downstream consequences is a training example by construction, not by archaeology. Persisting every rendered input alongside the trace by default is a deliberately narrow addition — because there is exactly one place in the system to capture it.
- **Inference calls are first-class records.** Each model call is persisted with its backend, state, queue and timing data, and token counts — the metadata a training and evaluation pipeline needs to select, weight, and cost samples.

### Trace → Train: the system's history is a governed dataset

Traces are only useful if you can get them out — and only safe if getting them out is governed. Gents treats both as projection problems over the durable record:

- **Export in shapes training and eval stacks recognize.** A request's timeline projects into documented, versioned external-framework styles — ATIF v1.7 trajectories, OpenAI/Codex-style run traces, LangGraph-style state histories, multi-agent task records — for consumption by existing fine-tuning, evaluation, and observability tooling. The ATIF projection supports native JSON output for consumers such as Harbor that require the trajectory as the top-level document.
- **Redaction lives in the export path, not the client.** Projections carry redaction modes — full, training-safe, public — applied inside the governed export path, and projection policy bindings let access control filter which rows an export may contain. Binding the redaction floor itself to policy — so a fine-tuning pipeline can be granted exactly the training-safe projection of exactly the collections it should learn from, and nothing else — is the completing step, and it is enforcement wiring on an existing surface, not new architecture.
- **Provenance travels with the sample.** Every projected trace carries its lineage: which agent, which behavior, which trigger, which parent request. When a fine-tuned model behaves unexpectedly, you can walk backward from the weights' training set to the durable operational records it came from.

Harbor expects an ATIF trajectory at `/logs/agent/trajectory.json`. Export the native document after a run while preserving the enveloped `json`, `jsonl`, and `eval-jsonl` formats for Gents-native consumers:

```sh
gents trace project \
  --projection atif \
  --format native-json \
  --request-id "$REQUEST_ID" \
  --output-file /logs/agent/trajectory.json
```

The result: fine-tune small, fast, specialized models on how *your* operation actually runs — your alert patterns, your runbooks, your tool-use conventions — using data whose filtering and release run through the same governed export path as everything else.

### Train → Deploy: models are documents too

In Gents, an inference backend is a document: endpoint, model list, concurrency and queue limits. An inference profile is a document: context window, output budget, temperature, deadlines. Deploying the model you just fine-tuned means standing it up behind an endpoint — including a fully local one, on your own hardware, beside your own data — and writing a document. Agents route to it through the same resolution chain as any other backend, with admission control and capacity invariants that are proven, not hoped.

Reconfiguration is a **hot reload, formally**. When configuration documents change, the runtime reconciles: it resolves the new configuration, applies it, and publishes a new runtime generation. In-flight work stays pinned to the generation that claimed it; the previous generation drains before it retires; generations only move forward. These are proven properties of the reconcile model. You can swap the model under a live agent fleet with no restart, no dropped work, and no ambiguity about which configuration produced which inference — the controller generation and backend fingerprint are recorded on every inference call.

### Deploy → Operate: the system can modify itself — under guardrails

The final edge of the loop is the one that makes people nervous, and it is where the architecture earns its keep. Because configuration *is* documents, an agent improving its own setup is not a new, exotic capability that needs a bespoke safety system — it is a document write, subject to exactly the machinery that governs every other write:

- **Self-configuration ships, typed and fenced.** An agent that has learned a better prompt, a better schedule, or a sharper trigger changes it through dedicated self-configuration tools — typed, per-collection operations with formally proven guardrails: writable and protected field partitions, identity immutability, transactional accept-or-reject, and an optional no-lockout guard that rejects accepted changes which would disable the behavior, its active backend, or the self-configuration gate itself. There is no privileged side door; the agent's changes flow through the same governed write path and reconcile model as an operator's.
- **Selected tool invocations go through the approval inbox.** Tool approvals are a collection: a gated invocation holds until a human reviews exactly what is being asked and approves or rejects it, and the decision is recorded with the approver's identity as a durable document. The operator ceiling on tool authority means a behavior cannot grant itself more than the operator allowed — a misbehaving or compromised agent proposing its own escalation hits enforcement, not a policy PDF. Extending the inbox from held actions to capability grants, with each decision bound to the approver's hardware-backed signature through the trust layer, is where this surface is headed.
- **Long-horizon objectives are durable and budgeted.** Goals are documents with explicit token and time budgets, continuation lineage, and wrap-up state. A self-improvement objective — "reduce false-positive alert pages" — persists across sessions, restarts, and model swaps, with its spend visible and its progress auditable.
- **The loop feeds itself.** Every self-modification produces durable records for the invocation, the applied patch, and the resulting generation; approvals join that lineage when required. The system's improvement of itself becomes training data for the next turn of the flywheel. Recursion, with receipts.

This is the difference between recursive self-improvement as a risk and recursive self-improvement as a product: on this platform, the loop's every edge is observable, attributable, reversible, and gated by human approval where it matters — signature-bound in the fully integrated deployment. You don't trust the system to improve itself responsibly. You **verify** it.

### The loop in practice: infrastructure operations

Infrastructure monitoring and automation is the primary use Gents is demonstrated on today. In a fully integrated deployment, the complete flywheel looks like this:

1. **Operate.** An event trigger watches the alerts collection; each new alert fires a triage agent. A schedule runs nightly health sweeps across the fleet. An operator asks a general agent to investigate a latency regression; it delegates the code-level investigation to a coding-specialist subagent on another node.
2. **Trace.** Every triage decision, every health-sweep finding, every tool call the investigation made — down to the model inputs — lands in the durable record, attributed to the acting identity.
3. **Train.** Six months of triage decisions, exported through the training-safe projection, fine-tune a small model that knows *this* infrastructure's alert taxonomy cold — which pages matter at 3 a.m. and which are the flapping load balancer again.
4. **Deploy.** The triage behavior's backend document is updated to point at the fine-tuned model running on a local inference box. New generation, no restart, in-flight triage unaffected. Frontier models stay in the loop for the hard, novel cases — the specialist handles the volume.
5. **Improve.** The triage agent, observing its own miss rate through the same query surface operators use, sharpens its own event-trigger filter through the self-configuration tools — a typed, guardrailed write it is allowed to make — and routes the change it isn't allowed to make through an operator's approval inbox. One decision later, the system is better — and the record shows exactly how, why, and on whose authority.

Each pass around the loop makes the system faster, cheaper, and more specifically *yours* — with an audit trail that gets richer, not thinner, as autonomy grows.

---

## Part 2 — The Platform

The flywheel runs on a general-purpose agent platform. This part describes it: a document-driven runtime whose lifecycle is formally verified, with first-class delegation, automation, and interoperability.

### The Stack

In a fully integrated deployment, the stack composes as follows:

```
┌─────────────────────────────────────────────────────────┐
│                     Gents Runtime                       │
│   Formally verified lifecycle, document-driven control  │
└────────────────────────┬────────────────────────────────┘
                         │ Authenticated reads and writes
┌────────────────────────▼────────────────────────────────┐
│                      DefraDB                            │
│  CRDT document store with iroh P2P replication          │
│  All data encrypted to authorized identities            │
│  Access control enforcement on every operation          │
│  Every document is a governed, auditable record         │
└──┬──────────────────────────────────────┬───────────────┘
   │         Hosted trust services        │
   ▼                                      ▼
┌──────────────┐                    ┌──────────────┐
│    Hub.rs    │                    │    Orbis     │
│  Replicated trust service:       │  DKG ring:    │
│  policy / bulletin / identity,   │  proxy re-    │
│  Zanzibar engine, verifiable     │  encryption,  │
│  snapshots                       │  threshold    │
│                                  │  signing      │
└──────────────┘                    └──────────────┘
```

The trust services are optional integrations of the data layer, wired on per deployment; Current State records today's defaults. All of the runtime's durable control-plane and audit state lives in DefraDB — transient process internals exist, but nothing decision-bearing survives outside the document store. You configure the runtime by writing documents, trigger work by writing documents, and debug by reading documents. The durable audit surface *is* the document store.

### The Document-Driven Control Plane

Every question the runtime can ask — what identity is this, what tools may it use, what backend does it call, what is it doing right now, what did it do an hour ago — is answered by reading documents.

**Configuration** — desired state:

| Collection | Purpose |
| --- | --- |
| AgentPrincipal | DID-backed identity; the permission and audit boundary |
| AgentBehavior | Prompt, model, tool selection, backend, inference profile, skills |
| ToolSelection | Which local tools, remote services, and delegation targets a behavior can use |
| Skill | A named capability bundling instructions with an explicit tool grant |
| InferenceBackend | LLM endpoint, model list, concurrency and queue limits |
| InferenceProfile | Context window, output budget, temperature, deadlines |
| ToolServiceRegistry | Discoverable MCP-style remote tool services |

**Automation** — work and lineage:

| Collection | Purpose |
| --- | --- |
| Task | A named unit of work: prompt template bound to a behavior |
| Schedule | Runs a task on an interval or cron, with missed-run and concurrency policy |
| EventTrigger | Runs a task when documents in a watched collection change, with filtering |
| Goal | A durable objective with token/time budgets, continuation lineage, wrap-up state |

**Interaction history** — the operational record, encrypted to session participants in a fully integrated deployment: AgentRequest, AgentResponse, InferenceCall, AgentSession, AgentConversation, AgentMessage, AgentToolCall, AgentToolResult, CompactionEntry, AgentMemory.

**Oversight and observability** — AgentToolApproval (the approval inbox as a collection) and AgentRuntime (process state, reconcile phase, active generation).

When the runtime reconciles, it resolves a runnable configuration by following one chain — principal → behavior → backend, tool selection (intersected with the operator ceiling), skills (privilege-bounded), profile — and publishes the result. If the backend is missing, disabled, or unhealthy, the behavior is unrunnable, and the runtime publishes that fact. Configuration collections are current desired state; operational collections are branchable, preserving observable history. The apply path owns desired-state fields; the runtime owns live-state fields; neither clobbers the other.

### Identity: Principal / Behavior / Deployment

A *principal* is a DID-backed identity — the permission and audit boundary, what the trust service recognizes, what signs documents. A *behavior* is a reusable interface on a principal: prompt, tools, model, backend policy; one principal can have many. A *deployment* places a principal's behavior on hardware; the deployment contract assigns each (principal, behavior) pair to exactly one deployment, so there is no ambiguity about which machine is acting for an identity.

The separation matters for least privilege. A maintenance assistant and an operations assistant can be two behaviors of one principal, sharing permissions and audit trail. A background analysis task that must not see sensitive data is a separate principal with narrower permissions — not a flag on the same one. You do not mint a new identity every time you add a prompt, and you do not stretch one identity across work that needs different authority.

### Delegation: Subagents Across Boundaries

Agents delegate to agents, and delegation is structural, not conversational. A child is a first-class AgentRequest, stamped with its parent request and the tool call that spawned it, with delegation depth tracked. Which principals a behavior may delegate to is an allowlisted configuration choice, not an open capability. Cancelling a parent cancels its delegated children — a proven property, not a best effort.

Because requests replicate over the P2P mesh, delegation crosses machine boundaries with no special plumbing: a general assistant on one node spawns a coding specialist on another by writing a request document that replicates to it, and the child's execution returns into the same durable lineage. Cross-deployment spawning ships behind an explicit opt-in gate — deliberately default-off until the access-control integration that governs cross-organization trust lands — and delegation carries full lineage either way, so a subagent three hops away is as attributable as a local tool call.

### Automation with Lineage

Agents in production do not just answer prompts — they run on schedules, react to data changes, and pursue goals. In most stacks this is where the audit trail frays: a cron fires, a webhook triggers, and cause-and-effect lives in application logs, if anywhere. Here, automation is documents, and lineage is structural. Every request fired by automation carries which trigger caused it, what kind it was, and which parent delegated it. The trigger dispatch logic is formally modeled: disabled triggers never fire, serial tasks never run concurrently, superseded requests link to their successors, and lineage stamping is complete — there is no path through the system that produces an unattributed request.

### Layered Tool Authority

An agent's tool surface is resolved from documents, in layers that are independent by design:

- **Local tools** — file and command execution, bounded by an operator-owned ceiling that caps what any behavior can use regardless of what it requests. The final surface is the intersection of the behavior's selection and the operator's ceiling. Command execution is policy-filtered — argument, network, and environment constraints are formally modeled.
- **Remote MCP services** — discovered through registry documents, gated by per-behavior allowlists, with connection health and service identity tracked.
- **Skills** — capability bundles pairing instructions with explicit tool grants. The privilege algebra is formally specified: skills narrow or organize authority, never escalate it.
- **Delegation targets** — the principals a behavior may spawn subagents on, allowlisted per behavior.

### The Formally Verified Lifecycle

The runtime's lifecycle is a Lean 4 model with proven theorems, and the Rust implementation is tested for refinement against it — conformance tests run against persisted DefraDB state and assert the implementation only produces traces the model allows. The verified core spans fifteen domains; the ones that matter most here:

| Domain | What is proven |
| --- | --- |
| Request / process / persistence lifecycles | Terminal requests stay terminal; progress is monotonic; completion implies durable persistence; recovery blocks new claims until stuck state is repaired |
| Tool-call and delegation lifecycles | Every tool invocation moves through a legal lifecycle; cancelling a parent cancels its children |
| Scheduler and admission | Slots are never leaked; terminal work releases capacity; unavailable backends cannot admit work |
| Recovery convergence | After a crash, stuck requests are driven to terminal outcomes in finite steps — no work stranded, no audit trail dangling |
| Trigger dispatch | Disabled triggers never fire; serial execution is exclusive; lineage stamping is complete |
| Transcript reduction and compaction | Summarizing long histories preserves the integrity of the durable record |
| Provider-input assembly | Everything sent to a model passes one proven entry point — sound, idempotent, stable under splitting |
| Command execution policy | Argument, network, sandbox, and environment filtering; execution deadlines and cancellation have proven liveness |
| Skill privilege algebra | Skills compose tool authority without escalation |
| Agent self-configuration | Writable/protected field partitions, identity immutability, transactional accept-or-reject, opt-in no-lockout preservation for accepted writes |

The models are explicit about their boundary: they prove state-machine invariants over runtime-visible state — not storage-engine, network, provider, or UI behavior. That candor is what makes the guarantees they do state trustworthy.

Why this matters for the flywheel: traces are only as good as the lifecycle that produces them. A runtime that can crash mid-request and leave an ambiguous record, double-count work, or let a cancelled delegation keep running produces training data and audit trails with holes. The proofs close those holes by construction, and the conformance suite keeps the implementation honest — a code change that introduces an illegal transition fails the tests.

### Interoperability: Meeting Existing Stacks Where They Are

Gents is not a replacement for MCP, A2A, LangGraph, CrewAI, the OpenAI Agents SDK, or Microsoft Agent Framework. It is the durable, governed substrate underneath those surfaces, and it treats interoperability as a projection problem over its records:

- **MCP is the native tool boundary.** Gents keeps MCP as the interface to external tools and adds what frameworks leave to glue code: per-agent tool policy, service identity and health, DID propagation, and persisted, attributable tool calls.
- **Protocols fit as adapters over the same truth.** Agent-to-agent protocols (A2A and its lineage) map onto documents Gents already has — agent cards project from principals, behaviors, and tasks; protocol task lifecycles map to the request lifecycle; sessions map to sessions. The permission decision always resolves to the data layer, never to adapter-local auth.
- **Framework patterns map to document topologies.** Sequential, concurrent, handoff, group-chat, and manager-led orchestration; Flows and Crews; graph checkpoints and replays — these are topology patterns over requests, sessions, subagent lineage, and shared documents, expressible without surrendering governance to a framework runtime.
- **Traces flow both directions.** Export projects governed records into styles existing observability, eval, and compliance tooling consume. Import runs the other way — captures from LangGraph-, CrewAI-, and Microsoft-style frameworks already ingest into the same timeline model through the interop harness — so an existing stack's agent activity comes under the same audit surface during migration. Adoption is incremental, not rip-and-replace.

The trade-off is stated plainly: Gents is more opinionated about state, identity, access control, and persistence than a code-first library. For a one-off local script, that is more substrate than you need. For an organization running many agents against real systems — and intending to let that system improve itself — the document-native model is the product.

### Surfaces

Operators reach the platform through a CLI and a desktop client that pairs with a runtime over iroh P2P and then replicates the entire control plane — configuration and operational documents alike — under the same governance machinery that protects cross-organization deployments. Backends can be cloud providers or fully local inference endpoints; for environments where prompts and data must never leave the premises, the model runs beside the data. The whole system tolerates disconnection: a site with intermittent connectivity keeps operating against its local replica, CRDT replication reconciles when the link returns, and governance does not degrade with the network — when ACP policies are configured, permissions are enforced on every access regardless of which replica serves it, and with encryption enabled, documents are ciphertext wherever they sit.

---

## Part 3 — The Foundation: Governance

Everything above assumes a data layer that can actually enforce what it promises. This part describes that layer — the Source trust stack Gents is built against. It is what makes the flywheel safe to run: rather than trusting agents to follow rules, the system makes unauthorized access cryptographically impossible. Unless a paragraph explicitly says otherwise, this part describes the fully integrated deployment; Current State records which pieces are wired on by default today.

### Three Primitives

**Cryptographic identity** — Every agent, human, and service gets a decentralized identifier (DID) backed by real keys, not API tokens: key pairs rooted in hardware (secure enclaves, HSMs, YubiKeys) or distributed across an Orbis DKG ring where no single machine ever holds the complete private key. An agent's identity has three layers: a persistent Orbis identity tying it to the real principal that authorized it, a hardware-rooted node identity proving which machine acted, and a service identity authenticating every operation it performs. Provisioning follows the same administrative path as for human operators; agent capability requests add one step — the approval inbox — and there is no backdoor.

**Access control on a replicated trust service** — Permissions are not database rows. Every grant, revocation, and policy change is recorded in a tamper-evident, quorum-validated transparency log replicated across independent validator nodes, with a Google Zanzibar relation engine deciding access. No single administrator — and no single compromised machine — can forge an authorization or quietly rewrite history; any party can independently verify what was authorized, by whom, and when. Policy authoring is headed toward assistance: a purpose-trained policy model that translates operational intent into valid Zanzibar policies for human review. The model proposes; the human signs; the trust service records.

**Identity-based encryption** — Data is encrypted to identities, not servers or locations. Ciphertext can be replicated, cached, and stored anywhere — across sites, networks, and organizational boundaries — and remains ciphertext to everyone except identities with verified permissions. The Orbis ring performs proxy re-encryption, transforming ciphertext for an authorized reader without ever reconstructing the plaintext. There is no master key.

### Three Enforcement Surfaces

**Reads** — With ACP policies configured on a collection, DefraDB enforces access control on every read, including peer-to-peer block requests between nodes, not just client queries. Unauthorized reads return nothing; the cryptographic path to the plaintext does not exist.

**Writes** — When an agent writes a document, DefraDB requests the Orbis threshold ring to sign the operation. Each ring node independently checks the agent's permissions against the trust service; only a threshold of agreement produces a signature, and unsigned operations are rejected by every other node on merge. An agent cannot produce valid operations without authorization — and the check is performed by independent infrastructure, not the agent's own runtime.

**Placement** — the target of the third surface: restricting which machines may hold a collection's data at all — not just who can decrypt it, but where the ciphertext is allowed to exist. Today's node access control governs node administration; extending it into collection-placement policy is design work in the data layer. For data legally confined to a site or jurisdiction, it is the difference between "encrypted wherever it lands" and "never lands anywhere it shouldn't."

### The Trust Chain

```
Agent Identity (hardware-rooted or DKG-distributed)
  → Authenticates to DefraDB via signed JWT
    → DefraDB requests Orbis ring to sign the operation
      → Each ring node independently checks ACP on Hub.rs
        → Hub.rs validators reach quorum on authorization
          → Ring threshold-signs the operation (BLS12-381)
            → Any node can independently verify the signature
              → Valid signature = ACP was enforced, provenance is proven
```

An attacker who compromises a single node cannot forge a threshold signature; other nodes reject unsigned or mis-signed operations on merge. This is the system's core security invariant, and it holds identically for a human's write, an agent's tool call, and an agent's proposal to modify its own configuration.

### Key Properties

- **No single point of compromise** — signing authority is DKG-distributed; compromising one node does not confer signing authority.
- **Hardware roots of trust** — node and validator keys live in enclaves, HSMs, and YubiKeys; even root access cannot extract them.
- **Real-time revocation** — revocation is designed to propagate through the trust service and invalidate caches within seconds. The agent's cryptographic path to the data is severed. This is not a "please stop" — it is a key that stops working.
- **Verifiable audit, end to end** — every action produces a record backed by the quorum-validated log and threshold cryptography: requests signed by client identities, tool executions and responses signed by agent identity plus the ring, reads backed by ACP proofs, grants and revocations by validator quorum, delegation approvals by a human's device key. An auditor can verify the entire chain — which agent executed what, with access to which data, authorized by whom, revoked when — without trusting any single component. And because the flywheel's training exports, model deployments, and self-configuration changes flow through this same machinery, the audit trail covers not just what the system *did* but how the system *changed*.

---

## Current State

**The runtime ships today.** The Gents runtime runs with a formally verified lifecycle across fifteen proven, conformance-fenced domains; a document-driven control plane spanning configuration, automation, goals, memory, and interaction history; native MCP integration with service registry and health tracking; first-class subagents, with cross-node delegation behind explicit opt-in gates; typed, guardrail-proven self-configuration tools; generation-based hot reload; timeline reconstruction and adapter projections with full / training-safe / public redaction modes; and a CLI plus a desktop client that pairs over iroh P2P, against cloud or fully local inference backends.

**The trust stack runs as components.** Hub.rs runs as a replicated trust service — policy, bulletin, and identity modules over a quorum-validated log with verifiable snapshots, exercising the full Zanzibar engine. Orbis runs as a DKG ring with proxy re-encryption and threshold signing, open-sourced. DefraDB ships optional integrations for trust-service-backed access control (exercised in multi-node replication tests), at-rest encryption, and node access control.

**The integration milestone is bounded work, not research.** Turning the full trust chain on by default underneath the runtime — policies on the agent collections, at-rest encryption on, the identity-encryption/PRE path, ring-signed writes — plus durable rendered-input capture, policy-bound redaction floors, signature-bound approvals, and the A2A adapter: each is bounded integration work on existing surfaces. Not all of it is switch-flipping — durable capture needs storage and projection work, signed approvals need verification semantics, A2A needs an adapter — but none requires new foundational research. The flywheel's core trace spine, deploy stage, and self-configuration stage run today; the train stage runs against exported traces on external infrastructure; the loop tightens as each piece lands.

## Architecture Summary

Roles and properties below describe the fully integrated architecture; Current State records which integrations ship wired on by default today.

| Component | Role | Fully Integrated Property |
| --- | --- | --- |
| Gents runtime | Formally verified agent runtime; document-driven control plane | Every durable control-plane and audit record is a document; lifecycle is a Lean-proved state machine |
| DefraDB | CRDT document store, iroh P2P replication, agent operation backend | Every operation ACP-gated; data encrypted to identities; node-level placement control |
| Hub.rs | Replicated trust service: policy, bulletin, identity over a quorum-validated log; Zanzibar engine | Immutable permission records, no single authority, independently verifiable state |
| Orbis | DKG ring keys, proxy re-encryption, threshold signing | No single key holder, ring survives node loss, hardware-rooted |
