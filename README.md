# Dynamic workflows

Agent-authored workflows. The `workflow` tool takes a JavaScript step function
as `script` and runs it as a background job through gents' plugin-job host:
the host calls the function once per step with its view of the run and acts
on what it returns (spawns, messages, a read of children's text, a result).
The function runs in the host's embedded JavaScript engine (Afterburner,
QuickJS on Wasmtime, sealed: no files, network or environment) under the
call's fuel, memory and time bounds. There is no engine pack and nothing to
compile: a workflow is text in the call.

The pack ships one read-only worker persona, `wf-read-only`, and a caller
Tools template whose job block lets the tool spawn only that persona under
`policy.dynamic: run`. A workflow that outgrows this becomes a pack of its own:
the same step function, compiled by `gents pack build` (see `code_review`).

## Operator steps

```sh
gents pack install gents/dynamic_workflows --home ./.gents --node-did "$NODE_DID" \
  --grant-authority --inference-slot worker=<profile>
gents pack wire gents/workflow --agent <A> --targets-from gents/dynamic_workflows
```

The wired job block copies `Tools/workflow-caller`: `agent_targets` and
`dynamic_targets` are `wf-read-only`. A pack may ship `policy.dynamic` only as
`off` or `check` (gents `job_block_admissible`), and `check` refuses every
dynamic spawn, so the operator sets `run` on the agent's Tools after wiring:

```sh
gents config tools --agent <A> set integrations.plugins[gents/workflow].job.policy.dynamic=run
```

(or edit the Tools document; the exact `gents config` spelling is the host's).

## Authoring

See `plugins/workflow/TOOL.md`, which the model reads as the tool description.
A one-phase workflow:

```js
module.exports = (input) => {
  if (!input.pin) {
    return { effects: { spawn: [{ slot: "ask", attempt: 0, target: "wf-read-only",
      prompt: "Count the Rust files under src and answer with the number only.", title: "ask" }] } };
  }
  const ask = input.children["ask"];
  if (!ask || ask.state === "live") return { effects: {} };
  if (ask.state !== "completed") return { result: { count: null, limitation: "the worker did not answer" } };
  const text = input.job_results?.text?.result?.slots?.ask?.text;
  if (text === undefined) {
    return { job_calls: { requests: [{ id: "text", child_output: { slots: ["ask"], offset: 0, limit: input.ceiling.result_bytes } }] } };
  }
  return { result: { count: parseInt(text, 10) } };
};
```

The host pins the run with the script's digest, the call's `agent_budget` and
`args.handle`, so `gents job list|show|watch` and the Grok run view work as for
any job. The run is `dynamic`: `policy.dynamic` and `dynamic_targets` govern it.

## Validation

```sh
gents pack build ./packs/gents/dynamic_workflows
gents pack test ./packs/gents/dynamic_workflows
```

The artifact is a stub that answers a call without `script`; the fixture pins
that reply. The step function itself is exercised by gents' own tests
(`plugin::source_runner`, `hook::tests::plugin_job_dispatch`).
