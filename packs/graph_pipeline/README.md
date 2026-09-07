# Model-callable graph compiler evaluation

## Installation and use

`gents pack install graph_pipeline --home <home>` installs these evaluation
assets locally; it does not create agent behaviors or launch inference. The
compiler examples below describe how to use the fixtures programmatically.

This is an evaluation pack for a small adapter over the existing Gents
automation runtime. It does not introduce another graph engine.

`compile_graph` accepts topology over operator-approved capability revisions.
Each capability points at an existing Task document and declares its typed
ports. The model cannot author Task prompts, behaviors, tools, models, or
physical collections. The tool performs pure whole-graph validation first and,
only on success, writes the entry and edge EventTriggers in one transaction.

Execution remains separate. After normal runtime reconciliation, an existing
bounded write tool creates an entry document and the ordinary trigger/task
engine runs the graph. Configure `compile_graph` in
`approval_required_tools` when publication requires human approval.

```rust,ignore
let tool = CompileGraphTool::new(
    node,
    caller_identity,
    approved_existing_task_capabilities,
    CompilerPolicy::default(),
);

let agent = Agent::builder(provider).custom_tool(tool);
```

`eval_cases.json` is compiled as part of the `gents` unit suite. It covers
valid single- and multi-stage proposals plus repair cases for invented or stale
capabilities, missing inputs, schema mismatch, cycles, and structural bounds.
Expected diagnostic codes are subsets so cases remain stable when the compiler
adds another useful diagnostic.

For model evaluation, record proposal acceptance, diagnostic codes per repair
turn, number of repair turns, final digest, publication errors, reconciliation
latency, and whether the separately written entry document drives the expected
existing Tasks. Stage-output quality is a separate evaluation.
