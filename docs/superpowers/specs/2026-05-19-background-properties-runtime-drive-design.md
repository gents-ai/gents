# Background Properties: Theorem-Witness Discriminator + Runtime Drive

Status: design for review
Date: 2026-05-19
Tracking: audit item #4 (`docs/superpowers/audits/2026-05-19-conformance-audit.md` §8)
Branch: `design/background-runtime-drive-design`
Predecessor: PR #255 (`48de87e`) — ledger-row half (six `followUpCoverage` rows)

## Goal

Close the runtime-drive half of audit item #4. Two of the six theorems in
`crates/defra-agent/proofs/Proofs/Background/Properties/` — `cascade_cancels_child`
and `backgrounded_budget_bounded` — are operationally testable from current
Rust. This spec designs:

1. A new `BackgroundTheoremWitness` row shape (the `theorem_witness`
   discriminator) emitted from `Proofs/Conformance/ContractCases/R6Background.lean`.
2. Two Rust witness consumers, one per theorem, that drive production code paths
   and check the property the Lean theorem proves.
3. The conformance ledger edits that turn the two `followUpCoverage` rows from
   PR #255 into `consumerCoverage` rows.

The four remaining theorems (`foreground_blocks_parent_advance`,
`bridged_child_completion_propagates`, `inv_depth`,
`bridgedUniqueCallIds_preserved`) stay Lean-only with the rationale documented
below; their existing `followUpCoverage` rows from #255 are unchanged by this
spec.

## Source of Truth

- `docs/superpowers/audits/2026-05-19-conformance-audit.md` §8 Background
  (current state + smallest-delta direction).
- `docs/superpowers/audits/2026-05-19-conformance-audit.md` recommended
  next-impl order: item #4 — "Background Properties runtime drive."
- The six Lean theorems in `crates/defra-agent/proofs/Proofs/Background/Properties/`.
- The six `followUpCoverage` rows in
  `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:362-387` (PR #255).

## Current State

PR #255 closed the ledger-row half:

- `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:362-387` —
  six `followUpCoverage` rows under `followUpHookCoverage`, one per theorem in
  `Proofs/Background/Properties/`. The two rows for `cascade_cancels_child`
  (line 369) and `backgrounded_budget_bounded` (line 365) explicitly call out
  the follow-up text:
  `"Follow-up: emit witness row via theorem_witness discriminator in Proofs/Conformance/ContractCases/R6Background.lean."`
  The four remaining rows say `"Accepted Lean-only today because ..."`.
- `Proofs/Conformance/CoverageLedger.lean:389-393` — `followUpHookIds` /
  `followUpHooksJson` project these onto a JSON `follow_up_hooks` array, wired
  into the snapshot at `Proofs/Conformance/Contracts/Json/Snapshot.lean:113`.

Existing R6 conformance surface that this spec sits next to:

- Lean row type: `R6BackgroundingCase` at
  `crates/defra-agent/proofs/Proofs/Conformance/ContractCases/Types.lean:229`.
- Lean rows + factory: `r6Case` and `r6BackgroundingCases` at
  `crates/defra-agent/proofs/Proofs/Conformance/ContractCases/R6Background.lean:15-107`
  (seven shape-pinning rows: budget admit/reject, bridge complete/failure,
  recovery, queue source/key).
- JSON projection: `r6BackgroundingCaseJson` at
  `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/BackgroundWork.lean:166-183`.
- Snapshot key `"r6_backgrounding_cases"` at
  `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/Snapshot.lean:98-100`.
- Rust consumer: `generated_r6_backgrounding_cases_pin_tool_backgrounding_contract`
  at `crates/defra-agent/tests/state_machine_conformance/transcript_background.rs:356`,
  registered at
  `crates/defra-agent/tests/support/conformance_consumers.rs:288-296`,
  pointed at by the ledger row at
  `Proofs/Conformance/CoverageLedger.lean:301-304`.

The existing rows are a **data-shape contract** — they pin field values
(`max_backgrounded == 8`, `cancel_policy == "cascade"`, `error_code ==
"background_tool_budget_exceeded"`, terminal-state strings). They do not drive
production code paths. Audit §8's smallest-delta paragraph names this as a
separate, parallel gap; this spec does not touch it.

## The Two Runtime-Drivable Theorems

### `Subagent.BridgedState.backgrounded_budget_bounded`

Source: `crates/defra-agent/proofs/Proofs/Background/Properties/Budget.lean:31-36`.

```
theorem backgrounded_budget_bounded
    (s : BridgedState) (h_reach : Reachable s) :
    s.backgroundedLiveCount ≤ maxBackgroundedPerParent
```

`backgroundedLiveCount s` (Budget.lean:14) is `s.parent.tools.filter (await =
.background ∧ ¬ terminal state) |>.length`. `maxBackgroundedPerParent` is the
constant `8` defined in `State.lean:115` (`CancelPolicy` namespace) and pinned
across the existing R6 data-shape rows.

**Operational kind:** state invariant. Quantifies over reachable bridged
states; no trace step required to express it.

**Production target.** Two production sites enforce the bound:

- `crates/defra-agent/src/hook/persistence/mod.rs:45` — constant
  `const MAX_BACKGROUNDED_TOOLS_PER_PARENT: usize = 8;`.
- `crates/defra-agent/src/hook/persistence/background_tools.rs:86-101` — the
  admission gate inside `persist_background_tool_call`:
  ```rust
  let live_count = count_live_backgrounded_rows(&self.node, &request_id).await?;
  if live_count >= MAX_BACKGROUNDED_TOOLS_PER_PARENT {
      return self.fail_background_meta_tool_call(/* ... */).await;
  }
  ```

**Operational test (smallest surface).** Drive the production hook,
not a simulator:

1. Spawn `MAX_BACKGROUNDED_TOOLS_PER_PARENT` live backgrounded rows by
   invoking `PromptHook::on_tool_call("background_tool", ...)` against
   `DefraSessionHook` with a tool that hangs (e.g. the existing `PendingTool`
   fixture from `crates/defra-agent/tests/r6_background_tools.rs:76-100`).
2. After each spawn, read the live count from DefraDB via
   `count_live_backgrounded_rows(&node, &request_id)` and assert it equals
   `i + 1` and is `≤ MAX_BACKGROUNDED_TOOLS_PER_PARENT`.
3. After the 8th spawn, attempt one more. Assert the returned hook action
   carries `code == "background_tool_budget_exceeded"`,
   `current_backgrounded == 8`, `max_backgrounded == 8` (matching the existing
   field names already pinned at
   `crates/defra-agent/tests/r6_background_tools.rs:415-417`).
4. Read the database again and assert the live count is still
   `MAX_BACKGROUNDED_TOOLS_PER_PARENT`, never `MAX_BACKGROUNDED_TOOLS_PER_PARENT + 1`.

This is exactly the safety statement the Lean theorem proves, against the
exact production gate that enforces it. The existing test
`background_tool_rejects_when_parent_budget_is_exhausted` at
`crates/defra-agent/tests/r6_background_tools.rs:383-422` already drives steps
1 and 3; the new witness consumer adds steps 2 and 4 (the per-step invariant
queries) and is keyed off the Lean witness row name rather than hard-coded
constants.

### `Subagent.BridgedState.cascade_cancels_child`

Source: `crates/defra-agent/proofs/Proofs/Background/Properties/Cancellation.lean:22-34`.

```
theorem cascade_cancels_child
    (pre : BridgedState)
    (h_parent_term : isTerminal pre.parent.request.state)
    (h_cascade     : ∃ t ∈ pre.parent.tools,
                        t.callId = pre.bridgeCallId ∧
                        t.cancelPolicy = .cascade ∧
                        ¬ isTerminal t.state)
    (h_child_proc      : pre.child.request.state = .processing)
    (h_child_admission : pre.child.request.admission = .executing)
    (h_child_no_fg     : ¬ ∃ t ∈ pre.child.tools, t.awaitMode = .foreground ∧
                                                    ¬ isTerminal t.state)
    (h_linked          : pre.linked) :
    ∃ post, Trace pre post ∧ post.child.request.state = .interrupted
```

**Operational kind:** reachability (existential `Trace`). Names a precondition
shape (parent terminal, live cascade-policy bridge tool, processing/executing
linked child with no live foreground tool) and asserts a two-step trace exists
to a state where the child is `.interrupted`.

**Production targets.** Three production sites compose to realize the trace:

- `crates/defra-agent/src/tool_call_lifecycle/transition/bridge.rs:179-193`
  (`bridge_cancel_cascade`) — pure, returns a `CascadeIntent` for a bridge
  tool that is `.cancelled` with policy `.cascade` and has a `child_request_id`.
- `crates/defra-agent/src/tool_call_lifecycle/transition/bridge.rs:263-299`
  (`cancel_during_run_with_cascade_dispatch`) — Running → Cancelled on the
  bridge tool plus cascade dispatch. Returns
  `Some(CascadeDispatch::Local(intent))` for a locally-owned child, then the
  caller drives the child interrupt.
- `crates/defra-agent/src/interrupt.rs:38` (`interrupt_request`) — writes
  `interrupt_requested_at` on the child `AgentRequest`. This is the Lean
  `interrupt_processing` arm at the request layer.

**Operational test (smallest surface).** The existing test
`single_deployment_cancel_dispatch_still_interrupts_child` at
`crates/defra-agent/tests/r4_subagent_tools/background_cancel.rs:237-297`
already exercises the local cascade path end-to-end. It spawns a child via
`spawn_subagent`, loads the bridge `ToolCallLifecycle`, calls
`cancel_during_run_with_cascade_dispatch(AGENT_DID)`, requires the dispatch be
`CascadeDispatch::Local(intent)`, calls `interrupt_request(node,
intent.child_request_id)`, and reads back `fetch_interrupt_requested_at` on
the child. The new witness consumer is a thin re-shape of this test, keyed
off the Lean witness row name, that:

1. Builds the Lean precondition shape: spawn a child with
   `await_mode = "background"` (the default cancel policy is `cascade`,
   asserted as `tool.cancel_policy == "cascade"` at
   `tests/r4_subagent_tools/background_cancel.rs:49`); claim and advance the
   parent to a terminal state (the existing setup uses parent-cancel as the
   terminal trigger — same operational shape, since the Lean hypothesis is
   `isTerminal pre.parent.request.state`, not "parent is `.completed`").
2. Calls `cancel_during_run_with_cascade_dispatch(AGENT_DID)` on the loaded
   bridge lifecycle and asserts the dispatch is `CascadeDispatch::Local`.
3. Drives `interrupt_request(node, intent.child_request_id)`.
4. Reads back the child via `fetch_interrupt_requested_at` and asserts the
   child's `interrupt_requested_at` is `Some(_)`. (Per audit §8, the Lean
   theorem's post-state `child.request.state = .interrupted` corresponds to
   `interrupt_requested_at.isSome` once the recovery sweep / request lifecycle
   transitions the child to the `interrupted` row state; the post-state row
   transition is owned by Request lifecycle, not by the cascade dispatch
   itself. Driving the row to `.interrupted` is a follow-up — see Risks.)

## Why The Other Four Stay Lean-Only

| Theorem | File:line | Why not runtime-driven |
|---|---|---|
| `foreground_blocks_parent_advance` | `Foreground.lean:14` | Non-progress invariant. The runtime guard is structural — `request_step` carries an `h_no_block` precondition; the implementation never *advances* progressSeq while a live foreground tool exists. To "drive" the property in Rust you would have to enumerate every legal step and verify none of them increased `progress_seq` under a foreground guard, which is what the Lean proof already does case-by-case. No single production function corresponds to the property; the property *is* the absence of a code path. |
| `bridged_child_completion_propagates` / `bridged_child_failure_projects` | `Projection.lean:19` / `:89` | Already runtime-checked at data-shape level. The R6 rows `tool_kind_bridge_complete_persists_result` and `tool_kind_bridge_failure_cancelled_projects_parent_cancelled` at `R6Background.lean:57-72` pin the parent-tool terminal projection (`completed`, `cancelled`) per child terminal class. The theorem itself is the formal *trace* construction; the projection it certifies is already a data witness. Driving the trace would duplicate the existing shape-pin. |
| `inv_depth` (alias `subagent_depth_bounded`) | `Structure.lean:111`, `Foreground.lean:88` | Structural trace invariant. The runtime never exposes `subagent_depth` as a budget gate the way the backgrounded count is gated; depth is set once at spawn (`spawn_subagent` writes `subagent_depth + 1`) and never mutated. There is no production "increase depth" function to fence. The bound holds by construction at the apply path; an invariant test would inspect database state at rest, not drive a property. |
| `inv_link` (alias `bridge_link_symmetric`) | `Structure.lean:197`, `Foreground.lean:98` | Structural trace invariant lifted from per-step preservation. Same reasoning as `inv_depth`: there is no single runtime function that "preserves the link"; the link is a stored invariant of the bridge row and the child request row. The existing R6 spawn test (`spawn_subagent_background_materializes_child_and_bridge` at `tests/r4_subagent_tools/background_cancel.rs:4-89`) already asserts the link is *established* at spawn time (`tool.child_request_id == child_request_id`, `child.caused_by_parent_request_id == request_id`, `child.caused_by_parent_tool_call_id == internal_call_id`). The Lean theorem proves that no later transition breaks it; there is no operation to drive. |
| `bridgedUniqueCallIds_preserved` | `Unique.lean:198` | Structural trace invariant on `parent.tools` ordering. Uniqueness is asserted by the spawn path (the freshness precondition on `bridge_spawn` in `Proofs/Background/Transition.lean`); the database surface is a list of `AgentToolCall` rows keyed by `tool_call_id` with a unique index. Runtime "checking" uniqueness is a SELECT for duplicates after every operation, which neither tests the theorem (it tests the schema constraint) nor adds signal beyond the data-shape rows. |

All four match the "Accepted Lean-only today because ..." text already present
in `CoverageLedger.lean:373-387` from PR #255. This spec does not change those
rows.

## The `theorem_witness` Discriminator

### Lean record shape

Add to `crates/defra-agent/proofs/Proofs/Conformance/ContractCases/Types.lean`,
sibling to the existing `R6BackgroundingCase`:

```lean
/-- Witness row for an operationally-driven Background Properties theorem.
    Distinct from R6BackgroundingCase: case rows pin per-action data shape
    (pre_live_count, terminal_state, error_code) whereas theorem witnesses
    name (a) the Lean theorem, (b) its operational shape (invariant vs
    reachability trace), and (c) the Rust scenario and assertion target. -/
structure BackgroundTheoremWitness where
  -- Discriminator: the Lean theorem name. Conventionally the fully-qualified
  -- form, e.g. "Subagent.BridgedState.cascade_cancels_child".
  theoremName : String
  -- "state_invariant" | "reachability_trace". The shape the Rust consumer
  -- uses to dispatch its assertion strategy.
  witnessKind : String
  -- Human-readable scenario label. Stable across rows; the Rust consumer
  -- looks rows up by `theoremName` (or this label) and binds to specific
  -- production calls per row.
  scenario : String
  -- Bounded numeric parameter the theorem fixes, when one applies. For
  -- backgrounded_budget_bounded this is maxBackgroundedPerParent (= 8);
  -- for cascade_cancels_child this is the trace step bound (= 2). For
  -- theorems with no numeric bound, set to 0 and use kindFields below.
  numericBound : Nat
  -- Free-form key/value pairs encoding theorem-specific shape:
  --   * For backgrounded_budget_bounded: ("await_mode","background"),
  --     ("error_code_on_violation","background_tool_budget_exceeded").
  --   * For cascade_cancels_child: ("cancel_policy","cascade"),
  --     ("child_pre_state","processing"),
  --     ("child_pre_admission","executing"),
  --     ("child_post_state","interrupted").
  -- The Lean side enumerates these fields; the Rust consumer iterates and
  -- asserts each against the production observation. Keys are stable
  -- strings; values are stringified to keep the JSON projection simple.
  kindFields : List (String × String)
  deriving Repr
```

Field selection mirrors the existing case-row pattern (one struct per
domain, `deriving Repr`, explicit JSON projection). The discriminator is
`theoremName` paired with `witnessKind`. We deliberately do *not* extend
`R6BackgroundingCase` itself: that record's fields (`preLiveCount`,
`maxBackgrounded`, `terminalState`, `result`, `reason`, `errorCode`,
`queueSource`, `queueKey`) are per-action data, not per-theorem. Adding a
discriminator field plus a parallel set of Option fields would either bloat
every existing row with `none` values or produce two interleaved row
populations through one consumer — both of which the audit's "case-only data
witness vs theorem witness" framing argues against.

### JSON projection

In `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/BackgroundWork.lean`,
sibling to `r6BackgroundingCaseJson`:

```lean
def backgroundTheoremWitnessJson (witness : BackgroundTheoremWitness) : String :=
  "{"
    ++ "\"theorem_name\":" ++ jsonString witness.theoremName ++ ","
    ++ "\"witness_kind\":" ++ jsonString witness.witnessKind ++ ","
    ++ "\"scenario\":" ++ jsonString witness.scenario ++ ","
    ++ "\"numeric_bound\":" ++ toString witness.numericBound ++ ","
    ++ "\"kind_fields\":"
      ++ jsonArray (witness.kindFields.map (fun (k, v) =>
            "{\"key\":" ++ jsonString k ++ ",\"value\":" ++ jsonString v ++ "}"))
    ++ "}"
```

### Witness rows

In `crates/defra-agent/proofs/Proofs/Conformance/ContractCases/R6Background.lean`,
after `r6BackgroundingCases`:

```lean
def r6BackgroundTheoremWitnesses : List BackgroundTheoremWitness :=
  [ { theoremName := "Subagent.BridgedState.backgrounded_budget_bounded"
    , witnessKind := "state_invariant"
    , scenario := "background_tool_admission_respects_max_backgrounded_per_parent"
    , numericBound := Subagent.maxBackgroundedPerParent
    , kindFields :=
        [ ("await_mode", "background")
        , ("cancel_policy", "cascade")
        , ("error_code_on_violation", "background_tool_budget_exceeded")
        ]
    }
  , { theoremName := "Subagent.BridgedState.cascade_cancels_child"
    , witnessKind := "reachability_trace"
    , scenario := "parent_terminal_with_cascade_bridge_interrupts_processing_child"
    , numericBound := 2  -- the Lean trace is bridge_cancel_cascade ∘ child_step
    , kindFields :=
        [ ("cancel_policy", "cascade")
        , ("child_pre_state", "processing")
        , ("child_pre_admission", "executing")
        , ("child_post_state", "interrupted")
        ]
    }
  ]
```

### Snapshot key

In `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/Snapshot.lean`,
adjacent to the existing `"r6_backgrounding_cases"` line (`:98-100`):

```lean
++ "\"r6_background_theorem_witnesses\":"
  ++ jsonArray
      (r6BackgroundTheoremWitnesses.map backgroundTheoremWitnessJson) ++ ","
```

## Rust Witness Consumers

Two `#[tokio::test]` functions in
`crates/defra-agent/tests/state_machine_conformance/transcript_background.rs`,
the file that already hosts
`generated_r6_backgrounding_cases_pin_tool_backgrounding_contract`. They are
async because they touch DefraDB. Each consumer:

1. Looks up its witness row by `theorem_name` via a new helper
   `lean_r6_background_theorem_witness(name)` (modeled on the existing
   `lean_r6_backgrounding_case(name)` at
   `tests/state_machine_conformance.rs:44`).
2. Asserts the `witness_kind` matches the expected shape.
3. Reads `numeric_bound` and `kind_fields` and uses them as the source of
   truth for production assertions — no hard-coded `8`, `"cascade"`,
   `"background_tool_budget_exceeded"` strings on the Rust side; those flow
   from the witness row. If Lean changes the constant, the Rust assertion
   updates automatically; if the production code drifts, the test fails.

### Consumer 1 — budget invariant

Function name (follows the post-#239 `*_drive_*` naming convention from
`Proofs/Conformance/CoverageLedger.lean:297`):

```
generated_r6_background_theorem_witnesses_drive_admission_budget_invariant
```

Body sketch (DefraDB-backed, mirrors `r6_background_tools::background_tool_rejects_when_parent_budget_is_exhausted`):

```rust
#[tokio::test]
async fn generated_r6_background_theorem_witnesses_drive_admission_budget_invariant() {
    let witness = lean_r6_background_theorem_witness(
        "Subagent.BridgedState.backgrounded_budget_bounded",
    );
    assert_eq!(witness.witness_kind, "state_invariant");

    let max_backgrounded: usize = witness.numeric_bound as usize;
    let await_mode_expected = witness.kind_field("await_mode");
    let error_code_expected = witness.kind_field("error_code_on_violation");

    let (db, hook, session_id, request_id) = setup_hook(
        "r6-background-theorem-budget",
        registry(vec![Box::new(PendingTool)], &["slow_tool"]),
    ).await;

    // Property: every observation of live_backgrounded_count stays ≤ max_backgrounded.
    for index in 0..max_backgrounded {
        let receipt = skip_reason_json(
            PromptHook::<TestModel>::on_tool_call(
                &hook, "background_tool", None,
                &format!("meta-theorem-bg-{index}"),
                r#"{"tool_name":"slow_tool","args":{}}"#,
            ).await,
        );
        assert_eq!(receipt["status"], "running");
        assert_eq!(receipt["await_mode"], await_mode_expected);

        let live = count_live_backgrounded_rows(db.node.as_ref(), &request_id).await.unwrap();
        assert!(
            live <= max_backgrounded,
            "live count {live} exceeded {max_backgrounded} after admit #{index}; theorem violated",
        );
        assert_eq!(live, index + 1);
    }

    // Property: the (max_backgrounded + 1)-th admission is rejected with the witness-named code.
    let denied = skip_reason_json(
        PromptHook::<TestModel>::on_tool_call(
            &hook, "background_tool", None, "meta-theorem-bg-overflow",
            r#"{"tool_name":"slow_tool","args":{}}"#,
        ).await,
    );
    assert_eq!(denied["code"], error_code_expected);
    assert_eq!(denied["max_backgrounded"].as_u64().unwrap() as usize, max_backgrounded);

    // Property: after the rejection, the live count is still ≤ max_backgrounded.
    let live_after = count_live_backgrounded_rows(db.node.as_ref(), &request_id).await.unwrap();
    assert_eq!(live_after, max_backgrounded);

    // Sanity: session-wide assistant-visible row count never overshoots.
    assert_eq!(
        count_tool_calls_by_name(db.node.as_ref(), &session_id, "slow_tool").await,
        max_backgrounded,
    );
}
```

Production paths driven:
- `DefraSessionHook::persist_background_tool_call` (the assistant-facing
  `PromptHook::on_tool_call` entry).
- `crates/defra-agent/src/hook/persistence/background_tools.rs:86-101` —
  the budget gate (loop iteration `MAX_BACKGROUNDED_TOOLS_PER_PARENT`
  exercises the admit branch; the final call exercises the reject branch).
- `count_live_backgrounded_rows` — the same production query the gate uses.

### Consumer 2 — cascade reachability

Function name:

```
generated_r6_background_theorem_witnesses_drive_cascade_cancellation_trace
```

Body sketch (mirrors
`crates/defra-agent/tests/r4_subagent_tools/background_cancel.rs:237-297`):

```rust
#[tokio::test]
async fn generated_r6_background_theorem_witnesses_drive_cascade_cancellation_trace() {
    let witness = lean_r6_background_theorem_witness(
        "Subagent.BridgedState.cascade_cancels_child",
    );
    assert_eq!(witness.witness_kind, "reachability_trace");

    let cancel_policy_expected = witness.kind_field("cancel_policy");
    let child_post_state_expected = witness.kind_field("child_post_state");

    let (db, hook, session_id, _request_id, _parent_deadline) = setup_spawn_fixture(
        "background_theorem_cascade",
        vec![CHILD_BEHAVIOR_ID], 0, true,
    ).await;

    // Step 0 — establish the Lean precondition shape.
    let args = json!({
        "behavior_id": CHILD_BEHAVIOR_ID,
        "prompt": "child for cascade theorem witness",
        "await_mode": "background"
    }).to_string();
    let action = PromptHook::<TestModel>::on_tool_call(
        &hook, "spawn_subagent",
        Some("model-call-theorem-cascade".to_string()),
        "internal-theorem-cascade", &args,
    ).await;
    let receipt = skip_reason_json(action);
    let child_request_id = receipt["child_request_id"].as_str().unwrap().to_string();

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "internal-theorem-cascade").await;
    assert_eq!(tool.cancel_policy.as_deref(), Some(cancel_policy_expected));

    // Step 1 — bridge_cancel_cascade ∘ cancel_during_run_with_cascade_dispatch.
    let mut lifecycle =
        ToolCallLifecycle::load(db.node.clone(), &session_id, "internal-theorem-cascade")
            .await.unwrap()
            .expect("bridge persisted");
    let dispatch = lifecycle
        .cancel_during_run_with_cascade_dispatch(AGENT_DID)
        .await.unwrap()
        .expect("cascade dispatch");
    let CascadeDispatch::Local(intent) = dispatch else {
        panic!("local child must use local cascade dispatch");
    };
    assert_eq!(intent.child_request_id, child_request_id);

    // Step 2 — interrupt_processing on the child Request.
    interrupt_request(db.node.as_ref(), &intent.child_request_id).await.unwrap();

    // Property: the child's interrupt_requested_at is now set (the Lean
    // post-state precondition for the request-layer interrupted state).
    let child_interrupt =
        fetch_interrupt_requested_at(db.node.as_ref(), &child_request_id).await.unwrap();
    assert!(
        child_interrupt.is_some(),
        "cascade trace must leave child interrupt_requested_at set ({child_post_state_expected})",
    );
}
```

Production paths driven:
- `DefraSessionHook::persist_spawn_subagent_tool_call` (precondition setup).
- `ToolCallLifecycle::load`.
- `crates/defra-agent/src/tool_call_lifecycle/transition/bridge.rs:263-299`
  (`cancel_during_run_with_cascade_dispatch`).
- `crates/defra-agent/src/interrupt.rs:38` (`interrupt_request`).

### Allowlist registration

Add two rows to `crates/defra-agent/tests/support/conformance_consumers.rs`,
next to the existing R6 entry at `:288-296`. Pattern lifted verbatim from the
existing R6 row:

```rust
ConformanceConsumer::RustTest {
    id: "state_machine_conformance::generated_r6_background_theorem_witnesses_drive_admission_budget_invariant",
    package: "defra-agent",
    source_path: "crates/defra-agent/tests/state_machine_conformance.rs",
    module_path: "state_machine_conformance",
    function: "generated_r6_background_theorem_witnesses_drive_admission_budget_invariant",
},
ConformanceConsumer::RustTest {
    id: "state_machine_conformance::generated_r6_background_theorem_witnesses_drive_cascade_cancellation_trace",
    package: "defra-agent",
    source_path: "crates/defra-agent/tests/state_machine_conformance.rs",
    module_path: "state_machine_conformance",
    function: "generated_r6_background_theorem_witnesses_drive_cascade_cancellation_trace",
},
```

## Coverage Ledger Consequences

After implementation, the ledger at
`crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean` changes in
two places.

**1. Remove (or rewrite) the two `followUpHookCoverage` rows that are now
runtime-driven.** Lines `:363-371`:

- Line `:363-367` — `Subagent.BridgedState.backgrounded_budget_bounded`
  `followUpCoverage` row. Promoted: removed from `followUpHookCoverage`,
  replaced by an entry in `caseCoverage` (see #2 below). Alternatively, keep
  the row and rewrite the follow-up text from "Follow-up: emit witness row ..."
  to "Driven by `generated_r6_background_theorem_witnesses_drive_admission_budget_invariant`."
  The cleaner outcome is full removal — the theorem is no longer a follow-up
  hook once a consumer exists.
- Line `:367-371` — `Subagent.BridgedState.cascade_cancels_child`
  `followUpCoverage` row. Same treatment.

The four remaining `followUpHookCoverage` rows at lines `:371-387` stay
untouched (the "Accepted Lean-only today because ..." rows for
`foreground_blocks_parent_advance`, `bridged_child_completion_propagates`,
`inv_depth`, and `bridgedUniqueCallIds_preserved`).

**2. Add new `consumerCoverage` rows under `caseCoverage`,** adjacent to the
existing R6 entries at `:301-308`. Two rows, one per consumer:

```lean
, consumerCoverage
    "r6_background_theorem_witnesses"
    "BackgroundBudgetBoundedTheoremWitness"
    "state_machine_conformance::generated_r6_background_theorem_witnesses_drive_admission_budget_invariant"
, consumerCoverage
    "r6_background_theorem_witnesses"
    "CascadeCancelsChildTheoremWitness"
    "state_machine_conformance::generated_r6_background_theorem_witnesses_drive_cascade_cancellation_trace"
```

Domain strings (`BackgroundBudgetBoundedTheoremWitness`,
`CascadeCancelsChildTheoremWitness`) mirror the per-row keying used elsewhere
in the ledger when a JSON case domain contains multiple distinct row kinds
(compare `event_delivery_cases` with three domain rows at `:334-355`, and
`identity_*_cases` with three domain rows at `:313-325`).

The drift test at
`crates/defra-agent/tests/state_machine_conformance/coverage.rs:391` already
enforces ledger ↔ snapshot agreement on category strings; adding a new JSON
key (`"r6_background_theorem_witnesses"`) requires the matching `caseCoverage`
rows above and no test-side edits beyond a new entry in the snapshot
emit-coverage map.

## Design Options Considered

**A. Sibling list inside `R6Background.lean`, new struct, new JSON key.**
*Chosen.* `BackgroundTheoremWitness` is a distinct record type colocated with
`r6BackgroundingCases`. New JSON key
`"r6_background_theorem_witnesses"`. Pros: zero churn on existing R6 case
rows or consumer; the file/module that already owns R6 conformance owns the
new rows too, consistent with the follow-up text in
`CoverageLedger.lean:366` / `:370` which names `R6Background.lean` as the
target file; new domain is grep-stable. Cons: two row types in one file; the
implementer must add a parallel JSON projection and parallel Snapshot wiring.

**B. New file `Proofs/Conformance/ContractCases/BackgroundTheorems.lean`.**
Rejected. Isolation has no payoff here: the new rows reference the same Lean
constants (`maxBackgroundedPerParent`, `AwaitMode`, `CancelPolicy`) as the R6
case rows, share the same conformance category prefix
(`"r6_background_..."`), and the audit's follow-up text already names
`R6Background.lean` as the destination. Splitting the file would also force a
parallel directory-walk in the drift test for what is functionally one R6
conformance surface.

**C. Hybrid — extend `R6BackgroundingCase` with a discriminator and a parallel
optional-field set for theorem-only fields; keep one row list and one JSON
projection.** Rejected. The field overlap between data witnesses and theorem
witnesses is essentially nil: data witnesses carry `preLiveCount`,
`terminalState`, `result`, `reason`, `errorCode`, `queueSource`, `queueKey`;
theorem witnesses carry `theoremName`, `witnessKind`, `scenario`,
`numericBound`, `kindFields`. Forcing them into one struct produces a row
populated mostly with `none` regardless of which kind a row encodes, and the
Rust consumer becomes a `match` on the discriminator with two near-disjoint
post-conditions. Cleaner to use two structs (Option A).

The audit's framing — "the runtime-drive half wants its own design pass" —
argues for explicit infrastructure, which Option A provides minimally without
the over-isolation of Option B or the over-coupling of Option C.

## Risks And Open Questions

- **`cascade_cancels_child` post-state row transition.** The Lean post-state
  is `post.child.request.state = .interrupted`. The cascade dispatch +
  `interrupt_request` writes `interrupt_requested_at` on the child row; the
  child row transitions to lifecycle_state `interrupted` through the
  Request lifecycle's interrupt arms (see
  `Proofs/Request/Transition.lean` — `interrupt_before_claim`,
  `interrupt_claimed`, `interrupt_processing`) executed by the runtime
  recovery sweep or the daemon's interrupt handler. The consumer above
  asserts `interrupt_requested_at.isSome` — the necessary precondition for
  the Lean post-state, but one step short of the post-state itself. A
  stronger consumer would additionally pump the recovery sweep / daemon
  interrupt path to observe the row transition. This spec scopes the
  initial witness to the `interrupt_requested_at` observation (matching the
  existing test pattern at `tests/r4_subagent_tools/background_cancel.rs:281-295`)
  and notes the stronger form as a follow-up under audit item #4's later
  iterations.

- **Cross-deployment cascade.** The Lean theorem is single-deployment; the
  Rust runtime has a remote-cascade branch
  (`CascadeDispatch::RemoteIntentWritten`,
  `bridge.rs:198-212`) that writes a bridge intent rather than writing the
  child interrupt directly. This spec's cascade consumer fences only the
  local path, matching the Lean theorem's scope. A separate witness for the
  remote-dispatch path would require a Lean theorem about cross-deployment
  bridges, which lives in TLA+ work tracked by #155 / R5 cross-deployment
  specs and is out of scope here.

- **`witness_kind` vocabulary growth.** Today we need two values
  (`state_invariant`, `reachability_trace`). Future Background theorems
  promoted to runtime drive might introduce a third (e.g. `liveness_bounded`
  for a step-bounded eventual-reachability shape). The vocabulary lives
  inline as strings rather than a Lean inductive. If the set grows past
  three, lift it into `Proofs/Conformance/ContractTypes.lean` as a closed
  enum with `toString`/`fromString?` round-trip, the same way `AwaitMode`
  and `CancelPolicy` are modeled. Not worth doing pre-emptively for two
  values.

- **`numeric_bound = 0` sentinel.** A theorem with no numeric parameter
  (none in the initial two; potentially future ones) would carry
  `numericBound := 0`. Consumers must read `witnessKind` first and not
  interpret a literal `0`. If this becomes a footgun, switch to
  `Option Nat`.

- **Parent-terminal precondition wiring in the cascade test.** The fixture
  `single_deployment_cancel_dispatch_still_interrupts_child` does not
  *explicitly* drive the parent to a terminal state before calling
  `cancel_during_run_with_cascade_dispatch`; instead it calls the dispatch
  on a live bridge tool while the parent is still active, which fires the
  `Running → Cancelled` bridge transition (one of the legal terminal-from-
  the-bridge's perspective predecessors). The Lean theorem's `h_parent_term`
  is satisfied at the *bridge tool* level, not the parent request level —
  the cascade is triggered by the *bridge tool* being cancelled with policy
  `cascade`. The witness consumer should mirror the existing fixture (drive
  the bridge to cancelled, not the parent request), and the scenario string
  in the Lean row should clarify "bridge tool terminal under cascade policy"
  rather than "parent request terminal." Open question: rephrase the Lean
  `scenario` field below to
  `"bridge_tool_cancelled_under_cascade_interrupts_processing_child"` for
  faithful mirror to the Rust assertion target. The implementer should
  decide based on whichever phrasing is more intent-revealing in the JSON
  snapshot.

## Smallest Delta (files, not areas)

Lean (three files):
- `crates/defra-agent/proofs/Proofs/Conformance/ContractCases/Types.lean` —
  add `BackgroundTheoremWitness` structure.
- `crates/defra-agent/proofs/Proofs/Conformance/ContractCases/R6Background.lean` —
  add `r6BackgroundTheoremWitnesses : List BackgroundTheoremWitness` after the
  existing `r6BackgroundingCases` definition (after line 107).
- `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/BackgroundWork.lean` —
  add `backgroundTheoremWitnessJson` projection after
  `r6BackgroundingCaseJson` (after line 183).
- `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/Snapshot.lean` —
  add `"r6_background_theorem_witnesses"` key adjacent to
  `"r6_backgrounding_cases"` (insert after line 100).
- `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean` —
  remove (or rewrite) lines `:363-371`; add two `consumerCoverage` rows under
  `caseCoverage` (adjacent to line `:308`).

Rust (three files):
- `crates/defra-agent/tests/state_machine_conformance.rs` — add `pub use`
  for `lean_r6_background_theorem_witness` / `lean_r6_background_theorem_witnesses`
  (sibling to existing `lean_r6_backgrounding_case` at line 44); add the two
  new `#[tokio::test]` wrappers that delegate into
  `state_machine_conformance/transcript_background.rs`.
- `crates/defra-agent/tests/state_machine_conformance/transcript_background.rs` —
  add `generated_r6_background_theorem_witnesses_drive_admission_budget_invariant`
  and `generated_r6_background_theorem_witnesses_drive_cascade_cancellation_trace`
  bodies (siblings to `generated_r6_backgrounding_cases_pin_tool_backgrounding_contract`
  at line 356).
- `crates/defra-agent/tests/support/conformance_consumers.rs` — add the two
  `ConformanceConsumer::RustTest` allowlist rows adjacent to the existing R6
  entry at lines `:288-296`.

Snapshot codegen: re-run whatever script regenerates the Rust-side Lean JSON
mirror (the test infrastructure that resolves
`lean_r6_backgrounding_cases()`). No manual Rust struct mirroring is in
scope of this spec beyond the test bodies above.

## What's Not In Scope

- Implementing the discriminator and consumers. This is a design pass.
- Promoting any of the four "Accepted Lean-only" `followUpCoverage` rows
  beyond the two named here.
- Driving the post-cascade child row transition from
  `interrupt_requested_at = some` to `lifecycle_state = "interrupted"`. The
  consumer asserts the precondition for that transition; the recovery-sweep
  / interrupt-handler drive is a separate follow-up.
- Cross-deployment cascade (`CascadeDispatch::RemoteIntentWritten` branch).
  Tracked by R5 / #155 / cross-deployment TLA+ work.
- The data-shape `r6_backgrounding_cases` gap from audit §8's smallest-delta
  paragraph (extending those rows with a runtime drive that observes the
  resulting `terminal_state` against the Lean projection). Separate work
  stream; tackled in a sibling design pass.
- Changing the Lean spec, theorem statements, or the four Lean-only
  theorems' proofs.
- TLA+ / P2P territory.

## Self-Review

Citations: every claim above carries a `file:line` reference into the
checked-in source. The audit, the six Lean theorems, the six ledger rows
from #255, the two reference Rust consumer patterns, the production budget
gate, the production cascade dispatch, and the existing fixtures the new
consumers piggy-back on are all named explicitly.

No `TBD` / `TODO` / placeholder text in normative sections. The two open
questions in the Risks section are scoping calls left to the implementer,
not unfilled blanks.

An implementer picking this up cold can: read `R6Background.lean` to see
where the new rows go; read `Types.lean` to see where the new structure goes;
read `transcript_background.rs:356` to see the sibling test the new tests
mirror; read the production functions at the cited line numbers to see what
the consumers drive; and read the ledger rows at the cited line numbers to
see what to remove and add.
