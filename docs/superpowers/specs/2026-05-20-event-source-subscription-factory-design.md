# Subscription/Stream Factory Boundary for Event-Driven Sources

Status: design for review
Date: 2026-05-20
Tracking: [#259](https://github.com/sourcenetwork/defra-agent/issues/259) (prereq for [#252](https://github.com/sourcenetwork/defra-agent/issues/252))
Branch: `design/issue-259-subscription-factory-design`
Audit anchor: `docs/superpowers/audits/2026-05-19-conformance-audit.md` §15 EventDelivery
Predecessor: #254 (interim demotion of two `consumerCoverage` rows to `consumerWithFollowUpCoverage`)

## Goal

Introduce the narrowest possible trait boundary that lets an integration-level
conformance test drive `DefraWatcher`, `EventSource`, and `SubagentSource`
through a mockable `events::Subscription`. Without this seam, #252's
production-drive of `event_delivery_transition_cases` /
`event_delivery_convergence_traces` cannot replace the
`InMemoryEventDeliverySource` simulator at
`crates/defra-agent/tests/state_machine_conformance/event_delivery.rs:117`
without re-introducing a parallel state mirror.

This is a refactor, not a feature. The acceptance bar is:

- No behavioral change for any production caller.
- The three sources accept a `pub` subscription-source trait at construction.
- A test mock implementation lives under `crates/defra-agent/tests/support/`.
- `cargo test -p defra-agent` and `cargo test -p defra-agent --test
  state_machine_conformance` continue to pass.

Implementation of the new trait and the #252 production-drive itself are
out of scope; this spec stops at the boundary.

## Source of Truth

- Issue [#259](https://github.com/sourcenetwork/defra-agent/issues/259) (the
  three concrete blocker sites named below).
- `docs/superpowers/audits/2026-05-19-conformance-audit.md` §15 EventDelivery
  — the production-drive gap.
- Strategy A investigation comment on
  [#252](https://github.com/sourcenetwork/defra-agent/issues/252) — surfaced
  these blockers in the first place.
- `crates/defra-agent/src/event_delivery_contract.rs:14` — the existing
  `EventDeliveryRuntimeContract` trait, which is the closest template for
  what shape the new abstraction should take (one-line trait + three impls
  + a runtime registry).

## Current State

Three production loops each open a concrete DefraDB `Update` subscription
internally, with no trait abstraction in the way:

- `crates/defra-agent/src/watcher.rs:99-107` — `DefraWatcher::new`:
  ```rust
  pub fn new(node: Arc<EmbeddedNode>, agent_did: &str) -> Self {
      let subscription = node.subscribe(&[EventName::Update]);
      Self { node, agent_did: agent_did.to_string(), subscription, processed_request_ids: HashMap::new() }
  }
  ```
- `crates/defra-agent/src/trigger_engine/event_source.rs:324-332` —
  `EventSource::reconcile_subscriptions` opens the global subscription
  lazily on first reconciliation:
  ```rust
  if self.subscription.is_none() && !self.desired_collections.is_empty() {
      let subscription = self.node.subscribe(&[EventName::Update]);
      tracing::info!(/* … */);
      self.subscription = Some(subscription);
  }
  ```
- `crates/defra-agent/src/trigger_engine/subagent_source.rs:101-106` —
  `SubagentSource::ensure_subscription`:
  ```rust
  fn ensure_subscription(&mut self) {
      if self.subscription.is_none() {
          self.subscription = Some(self.node.subscribe(&[EventName::Update]));
          tracing::info!("subagent source opened global Update subscription");
      }
  }
  ```

Each loop afterwards calls `subscription.recv()` and
`subscription.check_and_reset_dropped()`:

- `crates/defra-agent/src/watcher.rs:148, :165`
- `crates/defra-agent/src/trigger_engine/event_source.rs:837, :854`
- `crates/defra-agent/src/trigger_engine/subagent_source.rs:504, :522`

Compounding the problem, `crates/defra-agent/src/lib.rs:48` makes
`trigger_engine` `pub(crate)`:

```rust
pub(crate) mod trigger_engine;
```

So no test under `crates/defra-agent/tests/` can name `EventSource` or
`SubagentSource` to construct one. Today the in-crate tests at
`crates/defra-agent/src/trigger_engine/tests/event_source.rs:88, :174, :301,
:417, :557, :687, :893, :958, :1055, :1155` reach the constructor by being
inside the same crate; integration tests in `crates/defra-agent/tests/` can
only get at these sources by booting a full `DefraAgent` and going through
`agent/runtime/startup.rs:180-190`, which constructs the three sources
inline.

Two key upstream facts that pin the design space:

- `events::Subscription::new` is `pub(crate)` to the `events` crate
  (`crates/events/src/subscription.rs:33`). Tests cannot manufacture a
  `Subscription` directly. The only way to obtain one is `bus.subscribe(&[…])`
  on something implementing `events::Bus`.
- `events::ChannelBus` and `events::Bus` are both `pub`
  (`crates/events/src/lib.rs:33, :35`). A test can construct
  `Arc::new(events::ChannelBus::new())`, call `bus.subscribe(&[EventName::Update])`
  to get a real `Subscription`, and call `bus.publish(message)` to feed it.
- `EmbeddedNode::event_bus()` (`crates/defra-node/src/lib.rs:355`) already
  returns `&Arc<dyn events::Bus>`. Production's node already routes its own
  subscription through this same trait.

These three facts mean a test mock can be a thin
`Arc<events::ChannelBus>`-backed wrapper that returns a real `Subscription`
to the source under test, and lets the test push synthetic `events::Message`s
into it.

## The Abstraction

Add one trait to a new module
`crates/defra-agent/src/trigger_engine/subscription_source.rs` (and a `pub`
re-export at the crate root):

```rust
//! Subscription factory for the three event-driven sources.
//!
//! Production wraps `Arc<EmbeddedNode>`; the integration test harness wraps
//! an `Arc<events::ChannelBus>`. Both yield a real `events::Subscription`
//! so the sources' `recv()` / `check_and_reset_dropped()` loops are
//! unchanged.

use std::sync::Arc;

use defra_node::EmbeddedNode;
use events::{EventName, Subscription};

/// Source of `events::Update` subscriptions for `DefraWatcher`,
/// `EventSource`, and `SubagentSource`.
///
/// Implementations are responsible for filtering to `EventName::Update`
/// only; callers do not pass an event mask. Keeping the surface to one
/// method matches the only call site the three sources need today and
/// avoids exposing the full `events::Bus` surface (which carries
/// publish / event_bus state the sources have no business with).
pub trait UpdateSubscriptionSource: Send + Sync {
    /// Open a fresh subscription to `EventName::Update`.
    ///
    /// Each call returns a distinct subscription; the caller owns the
    /// receiver. Mirrors `EmbeddedNode::subscribe`'s ownership semantics.
    fn subscribe_updates(&self) -> Subscription;
}

impl UpdateSubscriptionSource for EmbeddedNode {
    fn subscribe_updates(&self) -> Subscription {
        self.subscribe(&[EventName::Update])
    }
}

impl UpdateSubscriptionSource for Arc<EmbeddedNode> {
    fn subscribe_updates(&self) -> Subscription {
        EmbeddedNode::subscribe(self, &[EventName::Update])
    }
}
```

### Why this shape

- **Returns `Subscription`, not a stream of `Message`s.** The three sources
  already loop on `Subscription::recv()` + `Subscription::check_and_reset_dropped()`.
  Returning the existing `Subscription` type keeps the loop bodies
  byte-identical. The only line that changes inside each source is the call
  site that creates the subscription.
- **One method, not the full `events::Bus`.** Sources don't publish, don't
  inspect subscriber counts, and don't choose event masks dynamically; the
  surface area exposed should match what they actually consume.
- **Blanket impl on `EmbeddedNode` and `Arc<EmbeddedNode>`.** Production
  callers can pass `node.clone()` (an `Arc<EmbeddedNode>`) and the trait
  resolves without any wrapper type. The `Arc<EmbeddedNode>` impl is what
  the three sources will use via `Arc<dyn UpdateSubscriptionSource>` at
  their constructor.
- **No association with `EventDeliveryRuntimeContract`.** The existing
  `EventDeliveryRuntimeContract` carries Lean-conformance metadata; the
  subscription source is a runtime seam. Keeping them disjoint prevents
  the conformance trait from accruing unrelated obligations.

### Where the trait lives

`crates/defra-agent/src/trigger_engine/subscription_source.rs` — colocated
with the two consumers in `trigger_engine`. Even though `DefraWatcher` is
not in `trigger_engine`, it consumes the same trait; the module name
`subscription_source` (not `trigger_engine::subscription_source`'s usual
trigger-engine framing) keeps it neutral.

Re-exported at the crate root from `crates/defra-agent/src/lib.rs` alongside
the existing `EventDeliveryRuntimeContract`-adjacent surface:

```rust
pub use trigger_engine::subscription_source::UpdateSubscriptionSource;
```

## Where the Three Sources Accept the Trait

A constructor variant on each of the three, with the existing
`new(node, …)` kept as a thin wrapper that supplies `node.clone()` as
the default `UpdateSubscriptionSource`. No production caller's signature
changes.

### `DefraWatcher`

`crates/defra-agent/src/watcher.rs:98-108` becomes:

```rust
impl DefraWatcher {
    /// Construct a watcher whose subscription comes from `node` itself.
    /// Backwards-compatible entry point for production callers.
    pub fn new(node: Arc<EmbeddedNode>, agent_did: &str) -> Self {
        Self::with_subscription_source(node.clone(), node, agent_did)
    }

    /// Construct a watcher with a caller-supplied subscription source.
    /// Used by integration tests to inject a `ChannelBus`-backed mock.
    pub fn with_subscription_source(
        subs: Arc<dyn UpdateSubscriptionSource>,
        node: Arc<EmbeddedNode>,
        agent_did: &str,
    ) -> Self {
        let subscription = subs.subscribe_updates();
        Self {
            node,
            agent_did: agent_did.to_string(),
            subscription,
            processed_request_ids: HashMap::new(),
        }
    }
}
```

`node.subscribe(&[EventName::Update])` at `:100` is the only production line
that disappears. The `subscription` field stays an
`events::Subscription` — its type, dropped-counter semantics, and
`recv()` shape are unchanged.

### `EventSource`

`crates/defra-agent/src/trigger_engine/event_source.rs:204-222` becomes:

```rust
pub(crate) fn new(
    snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    node: Arc<EmbeddedNode>,
    cancel: CancellationToken,
) -> Self {
    Self::with_subscription_source(node.clone(), snapshot_rx, node, cancel)
}

pub(crate) fn with_subscription_source(
    subs: Arc<dyn UpdateSubscriptionSource>,
    snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    node: Arc<EmbeddedNode>,
    cancel: CancellationToken,
) -> Self {
    Self {
        snapshot_rx,
        node,
        subscription_source: subs,
        subscription: None,
        desired_collections: HashSet::new(),
        reconciled_generation: 0,
        reconcile_debounce: Duration::from_millis(250),
        cancel,
        source_schema_cache: SourceSchemaCache::default(),
        collection_id_to_name: HashMap::new(),
        seen_docs: HashMap::new(),
        pending_intents: Mutex::new(VecDeque::new()),
    }
}
```

Adds one field:

```rust
subscription_source: Arc<dyn UpdateSubscriptionSource>,
```

The lazy-open branch at `:324-332` switches from `node.subscribe(&[EventName::Update])`
to `self.subscription_source.subscribe_updates()`. The behavior — deferred
open until `desired_collections` is non-empty — is preserved.

### `SubagentSource`

`crates/defra-agent/src/trigger_engine/subagent_source.rs:85-106` becomes:

```rust
pub(crate) fn new(
    snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    node: Arc<EmbeddedNode>,
    cancel: CancellationToken,
) -> Self {
    Self::with_subscription_source(node.clone(), snapshot_rx, node, cancel)
}

pub(crate) fn with_subscription_source(
    subs: Arc<dyn UpdateSubscriptionSource>,
    snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    node: Arc<EmbeddedNode>,
    cancel: CancellationToken,
) -> Self {
    Self {
        snapshot_rx,
        node,
        subscription_source: subs,
        subscription: None,
        cancel,
        collection_id_to_name: HashMap::new(),
        processed_tool_calls: HashSet::new(),
    }
}

fn ensure_subscription(&mut self) {
    if self.subscription.is_none() {
        self.subscription = Some(self.subscription_source.subscribe_updates());
        tracing::info!("subagent source opened global Update subscription");
    }
}
```

Adds one field:

```rust
subscription_source: Arc<dyn UpdateSubscriptionSource>,
```

`ensure_subscription`'s body is the only line in the loop body that changes.

### Production callers — unchanged signatures

The four `new(node, …)` call sites stay byte-identical:

- `crates/defra-agent/src/watcher.rs` tests at `src/watcher/tests.rs:387, :441, :460, :478`.
- `crates/defra-agent/src/agent/runtime/router.rs:21`.
- `crates/defra-agent/src/agent/runtime/startup.rs:174-190` (the three sources
  inside `tokio::spawn`).
- `crates/defra-agent/tests/lifecycle_claim.rs:85, :616, :697`.
- `crates/defra-agent/tests/r4_subagent_completion.rs:860, :869`.
- In-crate `crates/defra-agent/src/trigger_engine/tests/event_source.rs` —
  ten `EventSource::new` call sites listed by `:88, :174, :301, :417, :557,
  :687, :893, :958, :1055, :1155`.

Each of those compiles unchanged: `new(node, …)` internally calls
`with_subscription_source(node.clone(), node, …)`.

## Visibility Lift

The narrow option: keep `pub(crate) mod trigger_engine` at
`crates/defra-agent/src/lib.rs:48` (no broad lift), but `pub`-re-export the
three test-relevant symbols at the crate root. After this change, the
`lib.rs` reexport block grows by three lines:

```rust
pub use trigger_engine::event_source::EventSource;
pub use trigger_engine::subagent_source::SubagentSource;
pub use trigger_engine::subscription_source::UpdateSubscriptionSource;
```

Inside `trigger_engine`, the relevant items become `pub` (not `pub(crate)`):

- `crates/defra-agent/src/trigger_engine/event_source.rs:50` — `EventSource`
  struct: `pub(crate) struct EventSource` → `pub struct EventSource`.
- `crates/defra-agent/src/trigger_engine/event_source.rs:204-222` —
  `EventSource::new` and the new `with_subscription_source` constructor:
  `pub(crate) fn` → `pub fn`. (The lazy-open `reconcile_subscriptions` and
  `next_fire` already have the visibility the trait+production caller need.)
- `crates/defra-agent/src/trigger_engine/subagent_source.rs:34-99` —
  `SubagentSource` struct and constructors: same `pub(crate)` → `pub` lift.
- `crates/defra-agent/src/trigger_engine/subscription_source.rs` (new
  module): `pub mod subscription_source;` inside `trigger_engine/mod.rs`,
  trait + impls all `pub`.

Everything else inside `trigger_engine` stays `pub(crate)`. The
`TriggerSource` trait, `FireIntent`, `FireResult`, `TriggerKind`, the
materializer, manual handle, etc. are unchanged. The blast radius is the
two structs and the new trait, period.

`DefraWatcher` is already `pub` at `lib.rs:119`; no change there.

Post-implementation amendment: the visibility lift also extends to
`ActiveRuntimeSnapshot` and the transitive `pub(crate)` field types needed to
name and construct it from integration tests (`ResolvedTask`,
`ResolvedSchedule`, `ResolvedEventTrigger`, `ConcurrencyMode`,
`DispatcherMap`, and `BackendAdmissionConfig`). The constructors take a
receiver over the snapshot type; if the type is not public, the constructor
cannot be called from outside the crate. A test-helper constructor was
considered but rejected as additional indirection without proportional API
hygiene benefit — the snapshot type is already documented and stable.

### Why not `pub mod trigger_engine`

Lifting the entire `trigger_engine` module to `pub` would expose
`ProductionMaterializer`, `ManualTriggerHandle`'s internals,
`ScheduleSource`, the `TriggerEngine` itself, the `runtime_snapshot` types
referenced inside, and the entire `trigger_engine::tests` surface. None of
those are needed by integration tests today; making them `pub` invites
test code to drift into using internal trigger-engine plumbing as a
testing scaffold, which is exactly the kind of coupling the conformance
audit is trying to reduce. The narrow re-export costs three `pub use`
lines and a `pub` change on two structs; the broad lift costs an
indefinite future maintenance budget on internal surface.

### Why not a `pub mod test_support` helper

A "helper constructor" pattern (`pub fn build_event_source_for_testing(…)
-> EventSource`) hides the type but exposes a function. The integration
test under #252 still needs to *name* `EventSource` to hold it in a struct
field (the conformance driver wraps all three sources and reads observable
state through each). Hiding the type behind a builder forces the test to
work through opaque handles, which is more code than the narrow `pub` lift
and doesn't actually reduce the API surface — the constructor argument
types are the same surface either way.

## Test Mock Implementation

Lives at `crates/defra-agent/tests/support/mock_subscription.rs` (new file),
exported via `crates/defra-agent/tests/support/mod.rs`. Single-file scope,
~60 LOC including doc comments. Sketch:

```rust
//! In-memory `UpdateSubscriptionSource` for conformance tests.
//!
//! Backed by a `events::ChannelBus`. Tests push `events::Message`s via
//! `publish_update(collection_id, doc_id)`; the source receives them
//! through the real `events::Subscription` returned by
//! `subscribe_updates()`. Preserves `recv()` / dropped-counter semantics
//! identically to production.

use std::sync::Arc;

use defra_agent::UpdateSubscriptionSource;
use events::{Bus, ChannelBus, EventName, Message, Subscription, Update};

#[derive(Clone)]
pub struct MockUpdateSubscriptionSource {
    bus: Arc<ChannelBus>,
}

impl MockUpdateSubscriptionSource {
    pub fn new() -> Self {
        Self { bus: Arc::new(ChannelBus::new()) }
    }

    /// Push a synthetic Update event into the in-memory bus. The single
    /// active subscription (if any) will see it on its next `recv()`.
    pub fn publish_update(&self, collection_id: impl Into<String>, doc_id: impl Into<String>) {
        let update = Update { collection_id: collection_id.into(), doc_id: doc_id.into(), is_relay: true };
        self.bus.publish(Message::update(update));
    }
}

impl UpdateSubscriptionSource for MockUpdateSubscriptionSource {
    fn subscribe_updates(&self) -> Subscription {
        self.bus.subscribe(&[EventName::Update])
    }
}
```

Open question: the exact `Message::update(…)` constructor and `Update`
field set come from `events::Update` at the upstream crate; the mock should
mirror whatever production messages a real `EmbeddedNode` would publish on
a doc-create. The implementer should cross-check field shapes against
`crates/events/src/event.rs` when implementing, and pin those field
expectations in a comment on `publish_update`.

The mock lives in `tests/support/` because:

- It pulls in `events::ChannelBus` directly — a dependency `defra-agent`'s
  production code never references except through `EmbeddedNode`. Keeping
  it test-only avoids leaking a test dependency into the production
  graph.
- The audit's #15 follow-up driver is the only consumer today.
- The pattern matches the rest of `tests/support/` (`http_mock.rs`,
  `mock_endpoint.rs`, `identity_stubs.rs`).

## What the #252 EventDelivery Production Drive Looks Like

Replacing `InMemoryEventDeliverySource` at
`crates/defra-agent/tests/state_machine_conformance/event_delivery.rs:117`
becomes: hold real `DefraWatcher`, `EventSource`, `SubagentSource` instances
plus one `MockUpdateSubscriptionSource` per source (so each source's
subscription is independent), and translate each `LeanEventDeliveryAction`
into a production state mutation.

Sketch of the post-#259 driver (illustrative, not normative for this spec):

```rust
struct ProductionEventDeliveryDriver {
    node: Arc<EmbeddedNode>,
    mock_subs: MockUpdateSubscriptionSource,
    watcher: Option<DefraWatcher>,
    event_source: Option<EventSource>,
    subagent_source: Option<SubagentSource>,
}

impl ProductionEventDeliveryDriver {
    fn new(instance_name: &str) -> Self {
        let node = /* … fresh in-memory DefraDB with conformance schemas */;
        let mock_subs = MockUpdateSubscriptionSource::new();
        let arc_subs: Arc<dyn UpdateSubscriptionSource> = Arc::new(mock_subs.clone());
        let watcher = (instance_name == "Watcher").then(|| {
            DefraWatcher::with_subscription_source(arc_subs.clone(), node.clone(), AGENT_DID)
        });
        let event_source = (instance_name == "EventSource").then(|| {
            EventSource::with_subscription_source(arc_subs.clone(), snapshot_rx_for_test(), node.clone(), CancellationToken::new())
        });
        let subagent_source = (instance_name == "SubagentSource").then(|| {
            SubagentSource::with_subscription_source(arc_subs.clone(), snapshot_rx_for_test(), node.clone(), CancellationToken::new())
        });
        Self { node, mock_subs, watcher, event_source, subagent_source }
    }

    async fn apply(&mut self, action: &LeanEventDeliveryAction) -> Result<(), String> {
        match action {
            LeanEventDeliveryAction::Persist { doc } => {
                // Write a real document via node.execute(graphql_create(doc)).
            }
            LeanEventDeliveryAction::Enqueue { doc } => {
                // Push a synthetic Update through the mock.
                self.mock_subs.publish_update(COLLECTION_ID_FOR_DOC, doc.clone());
            }
            LeanEventDeliveryAction::Handle { doc } => {
                // Poll the active source's next_fire() / next_request()
                // once; expect doc to be returned.
            }
            LeanEventDeliveryAction::RescanTick => {
                // For Watcher: tick GOSSIP_FALLBACK_POLL through tokio time
                // pause/advance. EventSource/SubagentSource refuse this
                // action (rescan_bounded_by = 0).
            }
            /* Depersist, Drop, DeliverFromQueue analogous */
        }
        Ok(())
    }
}
```

The replacement diff against
`crates/defra-agent/tests/state_machine_conformance/event_delivery.rs` is
roughly:

```diff
-#[derive(Debug)]
-struct InMemoryEventDeliverySource {
-    source: EventDeliverySourceContract,
-    world: lean_vocab_test::LeanEventDeliveryWorld,
-}
-
-impl InMemoryEventDeliverySource {
-    fn new(source: …, world: &…) -> Self { /* clone world */ }
-    fn apply(&mut self, action: &LeanEventDeliveryAction) -> Result<(), String> {
-        /* mutate self.world in memory */
-    }
-    fn unhandled_persistent_docs(&self) -> Vec<String> { /* read self.world */ }
-}
+use crate::support::mock_subscription::MockUpdateSubscriptionSource;
+use defra_agent::{DefraWatcher, EventSource, SubagentSource, UpdateSubscriptionSource};
+
+struct ProductionEventDeliveryDriver { /* fields per sketch above */ }
+
+impl ProductionEventDeliveryDriver {
+    async fn new(source: EventDeliverySourceContract, world: &LeanEventDeliveryWorld) -> Self { … }
+    async fn apply(&mut self, action: &LeanEventDeliveryAction) -> Result<(), String> { … }
+    async fn unhandled_persistent_docs(&self) -> Vec<String> {
+        // Query DefraDB for persistent docs and intersect with the set
+        // the source has actually emitted via next_fire / next_request.
+    }
+}
```

The three callers at `:11`, `:55`, and the convergence trace loop at `:51-63`
swap their `InMemoryEventDeliverySource::new(…)` / `.apply(action)` /
`.unhandled_persistent_docs()` calls onto `ProductionEventDeliveryDriver`.
`LeanEventDeliveryAction::RescanTick` against an `EventSource` /
`SubagentSource` continues to be rejected (matches
`rescan_bounded_by: 0`); the existing deviation arm at `:80-97` is
unaffected.

## Design Options Considered

**A. `trait UpdateSubscriptionSource` returning `events::Subscription`.**
*Chosen.* Smallest production-side delta: one field per source, one trait
method, no change to the loop bodies. Mock = thin `events::ChannelBus`
wrapper; uses a real `Subscription`, preserving `recv()` /
`check_and_reset_dropped()` semantics for free. Matches the issue title
("subscription/stream factory boundary"). Pros: production behavior is
provably unchanged (the same underlying type is returned by the same
subscribe call); test mock surfaces the existing public `events::ChannelBus`
type rather than reinventing a stream; conformance trait
(`EventDeliveryRuntimeContract`) is unrelated and stays unrelated. Cons:
test mock still depends on `events::ChannelBus` being a public type
upstream — if a future `defradb.rs` revision moves it behind a feature
gate, the mock breaks (low likelihood; ChannelBus is the only general-
purpose `events::Bus` impl in the crate).

**B. Per-source `EventStream` trait yielding `events::Message`s.**
Rejected. Replaces the three sources' `subscription: Option<events::Subscription>`
field with an async stream. Pros: most decoupled from the concrete
`Subscription` type. Cons: bigger production-side refactor — each source's
`select! { msg = subscription.recv() => … }` becomes `select! { msg =
stream.next() => … }`, and `check_and_reset_dropped()` either disappears
from the surface (worse — drops are a real conformance concern at
`watcher.rs:165`, `event_source.rs:854`, `subagent_source.rs:522`) or has
to be lifted into the trait. We'd be paying refactor cost for the *third*
abstraction over a channel, when one already exists and is the right
shape.

**C. Factor a one-step handler `fn handle_message(&mut self, msg:
events::Message) -> Option<FireIntent>` per source; tests skip the
subscription entirely.** Rejected. Pros: most "production-pure" test
posture — no real subscription state at all. Cons: doesn't fit
`DefraWatcher`, which has a richer poll-and-cooldown loop with
`tokio::time::timeout(GOSSIP_FALLBACK_POLL, …)` and a `prune_processed_requests`
cycle (`watcher.rs:121-154`) that aren't reducible to "handle one
message"; the conformance properties under #252 specifically test that
loop's behavior, including the deviation arms documented in
`crates/events/src/event.rs` re. message ordering. Also doesn't address
the `pub(crate) trigger_engine` visibility blocker — the handler still
lives on `EventSource`/`SubagentSource` and the test still needs to
construct them or expose the handler through a builder. Net cost is
higher than (A) with no upside for the conformance driver.

The audit's own framing — "subscription persistence stays a deviation"
(audit §15, smallest delta paragraph) — argues that the subscription
boundary is the right unit of testability, not the message boundary.
Option A formalizes the existing boundary; B/C introduce different
boundaries that don't match the audit's framing.

## Smallest Delta

Implementation (out of scope for this spec; recorded so the implementer
has a punch list):

Rust (new files):
- `crates/defra-agent/src/trigger_engine/subscription_source.rs` —
  `UpdateSubscriptionSource` trait + blanket impls for `EmbeddedNode` and
  `Arc<EmbeddedNode>`.
- `crates/defra-agent/tests/support/mock_subscription.rs` —
  `MockUpdateSubscriptionSource` + `publish_update` helper.

Rust (modified files):
- `crates/defra-agent/src/trigger_engine/mod.rs` — `pub mod subscription_source;`.
- `crates/defra-agent/src/lib.rs:48` — leave `pub(crate) mod trigger_engine`
  unchanged; add three `pub use` lines for `EventSource`,
  `SubagentSource`, `UpdateSubscriptionSource`.
- `crates/defra-agent/src/watcher.rs:98-108` — add
  `with_subscription_source`; rewrite `new` as a thin wrapper.
- `crates/defra-agent/src/trigger_engine/event_source.rs:50, :204-222,
  :324-332` — `pub(crate) struct` → `pub struct`; add
  `subscription_source` field; add `with_subscription_source` constructor;
  switch the lazy-open call site to `self.subscription_source.subscribe_updates()`.
- `crates/defra-agent/src/trigger_engine/subagent_source.rs:34, :85-99,
  :101-106` — same pattern.
- `crates/defra-agent/tests/support/mod.rs` — `pub mod mock_subscription;`.

Tests that must keep passing unchanged (i.e., callers of
`DefraWatcher::new`, `EventSource::new`, `SubagentSource::new`):

- `crates/defra-agent/src/watcher/tests.rs:387, :441, :460, :478`.
- `crates/defra-agent/src/agent/runtime/router.rs:21`.
- `crates/defra-agent/src/agent/runtime/startup.rs:174-190`.
- `crates/defra-agent/tests/lifecycle_claim.rs:85, :616, :697`.
- `crates/defra-agent/tests/r4_subagent_completion.rs:860, :869`.
- `crates/defra-agent/src/trigger_engine/tests/event_source.rs` — all ten
  `EventSource::new` sites listed above.

No Lean changes. No proof changes. No
`EventDeliveryRuntimeContract` / `EVENT_DELIVERY_CONTRACT` constant changes;
the `Watcher` / `EventSource` / `SubagentSource` rows at
`watcher.rs:111`, `event_source.rs:755`, `subagent_source.rs:475` continue
to drive `event_delivery_source_instances_match_runtime` at
`tests/state_machine_conformance/event_delivery.rs:24`.

## Conformance Consequence

Standalone, this spec is invisible to the coverage ledger. The three
ledger rows at
`crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:334`
(transition cases), `:338` (source instances), `:342` (convergence traces)
are unchanged.

The downstream effect (when #252 lands on top of this seam) is what the
audit §15 "smallest delta" paragraph and #254's interim demotion already
forecast:

- `event_delivery_transition_cases` row at `CoverageLedger.lean:334` —
  promote from `consumerWithFollowUpCoverage` (per #254) back to
  `consumerCoverage`, with a `ProductionEventDeliveryDriver` reference in
  place of the `InMemoryEventDeliverySource` reference.
- `event_delivery_convergence_traces` row at `CoverageLedger.lean:342` —
  same treatment.
- `event_delivery_source_instances` row at `CoverageLedger.lean:338` — no
  change; it was already fully bound through `EventDeliveryRuntimeContract`.
- Audit §15 footnote — strike the "in-test simulator" qualifier from the
  ⚠️ Partial verdict.

The two `deviation` rows
(`event_source_lacks_periodic_rescan` and
`subagent_source_lacks_live_rescan` at
`Proofs/Conformance/Deviations.lean:26, :37`) remain. The
`defradb_rs_p2p_subscription_state_not_durable` deviation (#240) is
orthogonal to this seam and unchanged.

## Risks and Open Questions

- **`Arc<EmbeddedNode>` blanket impl + `Arc<dyn UpdateSubscriptionSource>`
  interaction.** The three sources will hold `Arc<dyn UpdateSubscriptionSource>`;
  production passes `node.clone()` typed as `Arc<EmbeddedNode>`, which
  must coerce to `Arc<dyn UpdateSubscriptionSource>` cleanly. Rust's
  unsized coercion handles this through the
  `impl UpdateSubscriptionSource for EmbeddedNode` impl combined with
  `Arc<T>: CoerceUnsized<Arc<dyn Trait>>` (stable since 1.38). The
  blanket `impl UpdateSubscriptionSource for Arc<EmbeddedNode>` above is
  belt-and-braces — the implementer should pick whichever the compiler
  prefers and drop the other. If neither lands cleanly, fall back to a
  one-line newtype `struct NodeSubscriptionSource(Arc<EmbeddedNode>)`
  with the trait impl, and have `with_subscription_source`'s call sites
  wrap with `Arc::new(NodeSubscriptionSource(node.clone()))`.

- **Test mock and DefraDB state.** The conformance driver under #252 will
  use `MockUpdateSubscriptionSource` for the subscription, but `node`
  itself still has to be a real `EmbeddedNode` — `EventSource`,
  `SubagentSource`, and `DefraWatcher` all call `node.execute(graphql)`,
  `node.list_collections()`, `node.get_collection()`. So the driver is
  "mock subscription, real DB," not "fully in-memory." This is by design
  (production-state mutation is exactly what the audit asks for) and is
  the smallest delta that doesn't reduce the test to a simulator again.
  Implementer note: the existing `subagent_source.rs`
  `resolve_collection_name` (`:108-143`) and `event_source.rs`
  `resolve_collection_name` (`:414-449`) both query the live DB, so the
  mock can't paper over a missing schema; the test must seed the
  conformance source-collection schema before constructing the source.

- **Per-source vs. shared mock instance.** A single `Arc<MockUpdateSubscriptionSource>`
  shared across all three sources means every `publish_update` is seen by
  every active source — fine for `event_delivery_transition_cases` /
  `convergence_traces` (each case targets one source instance), but the
  driver should still document which source it targets per action and
  publish synthetic Updates whose `collection_id` matches the source's
  filter to avoid cross-talk. Open question: does the conformance driver
  need to construct multiple `MockUpdateSubscriptionSource` instances or
  just one shared bus? The simpler "one shared" choice is recommended
  pending implementer discovery; the field-level cost of swapping later
  is negligible.

- **`publish_update` payload fidelity.** Production `EmbeddedNode::subscribe`
  delivers `events::Message::Update(Update { collection_id, doc_id,
  is_relay, … })`. Tests must mirror the field set, especially
  `is_relay` — `DefraWatcher::next_request` at `watcher.rs:158` filters
  on `u.is_relay`. The mock helper should default to `is_relay: true`
  and expose a builder variant for `is_relay: false` if any Lean trace
  exercises the non-relay path. Open question: do any
  `event_delivery_convergence_traces` cases need to drive `is_relay =
  false`? If yes, extend the mock helper before the #252 driver lands; if
  no, the default is sufficient.

- **`reconcile_subscriptions` lazy-open and the mock.** `EventSource`'s
  `reconcile_subscriptions` (`:262-335`) only opens a subscription once
  `desired_collections` is non-empty. The conformance driver must publish
  a snapshot with at least one `active_event_trigger` before its first
  `LeanEventDeliveryAction::Enqueue` — otherwise the source has no
  subscription and the publish is dropped. The existing in-crate test at
  `crates/defra-agent/src/trigger_engine/tests/event_source.rs` already
  handles this pattern via `snapshot_rx_for_test()`; the conformance
  driver should reuse that helper or an equivalent.

- **Out-of-process subscription drops.** `events::ChannelBus` bounds its
  channels; if the driver publishes faster than the source `recv()`s,
  messages drop and `check_and_reset_dropped()` returns non-zero. The
  Lean `LeanEventDeliveryAction::Drop` arm models this explicitly; the
  driver should advance the source one step (read one event) between
  publishes when the Lean trace doesn't include a `Drop` action, to avoid
  spurious drops. If a Lean trace *does* include `Drop`, the driver can
  exploit the bus's bounded behavior — publish past the bound and assert
  `check_and_reset_dropped() > 0` on the next `Handle`.

## What's Not In Scope

- Implementing the trait, the three constructors, the mock, or the
  visibility lift. This is a design pass.
- Replacing `InMemoryEventDeliverySource` with the
  `ProductionEventDeliveryDriver` sketch above. That is the #252
  implementation, which this spec unblocks.
- Promoting the two ledger rows at `CoverageLedger.lean:334, :342` back to
  `consumerCoverage`. That promotion is part of the #252 PR, not this one.
- Restructuring DefraDB's `EmbeddedNode::subscribe` API. The chosen
  approach treats the upstream surface as fixed — `bus.subscribe()`
  returning a real `Subscription` is the seam we wrap, not extend.
- `defradb_rs_p2p_subscription_state_not_durable` (#240) — orthogonal
  upstream concern tracked at `Proofs/Conformance/Deviations.lean:49`.
- The two existing deviations (`event_source_lacks_periodic_rescan`,
  `subagent_source_lacks_live_rescan`) — orthogonal; this spec neither
  closes nor widens them.
- TLA+ / P2P territory.

## Self-Review

Citations: every claim above carries a `file:line` reference into the
checked-in source or a named upstream symbol. The three blocker sites,
the visibility line, the existing `EventDeliveryRuntimeContract` template,
the seven affected production callers, the ten in-crate test sites, the
two upstream `events`-crate facts (`Subscription::new` is `pub(crate)`,
`ChannelBus` is `pub`), and the post-#259 driver sketch are all named
explicitly.

No `TBD` / `TODO` / placeholder text in normative sections. The open
questions in the Risks section are implementer scoping calls (coercion
ergonomics, payload-field exactness, per-source-vs-shared mock instance),
not unfilled blanks.

An implementer picking this up cold can: read
`trigger_engine/subscription_source.rs` (new) for the trait shape; read
the three "Where the Three Sources Accept the Trait" subsections for the
constructor diffs; read `tests/support/mock_subscription.rs` (new) for
the mock; consult the "Smallest Delta" punch list to bound the change
set; and verify the conformance consequences by re-reading audit §15.
