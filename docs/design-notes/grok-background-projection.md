# Grok background-task projection

The shim projects runtime state, not another process lifecycle. The server
passes its existing BackgroundExecutionRegistry to each connection. Task
cancellation uses the runtime process-control API after checking registered
sessions or canonical, controllable child edges.

Running processes expose the full retained runtime window (currently 256 KiB),
not merely the database's 4 KiB observation tail. Eviction is reported as
truncation. Terminal output uses persisted completion evidence and its capture
limits. Output already discarded is not recoverable; model page budgets are
unchanged.

Native task registration precedes cumulative tool_call_update output. Delivery
fingerprints commit only after successful sends. During overlapping turns,
background activity and child streams continue; parent text and its cursors
stay pending. Background parent updates omit prompt/timing metadata so the
pager cannot adopt an old turn or reset the active timer. Only the canonical
request lifecycle can terminalize a child, not an interrupt marker alone.

## Stock UI audit

In the local xai-org/grok-build checkout, xai-grok-pager's
app/acp_handler/mod.rs handles SessionMatch::Child by passing ordinary tool
calls/updates to the child session tracker and scrollback. Bash tools are not
intentionally hidden.

app/acp_handler/background.rs handles task start/completion via
resolve_target_view, selecting the child's background-task store. Root
ToolCallUpdate handling calls route_bg_task_stdout to replace cumulative task
output. The child branch bypasses that helper and calls session.handle_update
directly. This is a separate client-side gap for live child task-card output.
Correct wire delivery alone does not prove that those cards render it.

Do not conceal that gap with synthetic completion or root-session rerouting.
The client fix belongs in its shared tool-update handling.

## Live wire check

With spawn_process and bash_unrestricted enabled on the served behavior:

```sh
python3 demo/grok-tui-port/scripts/grok_background_probe.py \
  --socket /path/to/grok.sock --cwd "$PWD" \
  --model GLM-5.3-Flash-NVFP4
```

The probe launches one bounded process, verifies a >100 KB output snapshot
after task registration, and cancels only that process through the native
task RPC. This complements the runtime and shim regression tests; it is not
a stock-pager rendering test.

Add `--child` when a `control-worker` subagent target is configured to exercise
the same large-output and native cancellation checks in a child session.

## Rebased live verification

The updated production binary was served from a fresh database, with the
configured GLM-5.3-Flash-NVFP4 backend, 524,288-token context and high reasoning:

- Root and child background processes both delivered the >100 KB retained
  snapshot after registration, accepted native task cancellation, returned
  `already_exited` on repeat cancellation, and emitted cancelled task completion.
- Cross-turn model-facing list/read/steer and native subagent get/list/cancel
  passed. The cancelled child became durably interrupted and its bash call
  cancelled; the queued steering request completed with the expected marker.
- Child tool/text delivery followed pane creation and preceded its finish
  event. The child reply appeared once; successful bash completion included
  its output, while automatic wake echoes carried hidden-scrollback metadata.

These are live framed-ACP results, not a claim that the stock client's child
background-output routing gap described above has been fixed.
