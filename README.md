# Dynamic workflows

Agent-authored workflows. The `workflow` tool takes a JavaScript step function
as `script` and runs it as a background job through gents' plugin-job host:
the host calls the function once per step with its view of the run and acts
on what it returns (spawns, messages, a read of children's text, a result).
The pack's `workflow` plugin owns the run: it pins the script's digest,
applies the default budget and words the failures. For the evaluation itself
it declares the `run_script` host import, and the host runs the call's own
`script` in its embedded JavaScript engine (Afterburner, QuickJS on Wasmtime,
sealed: no files, network or environment) under the call's fuel, memory and
time bounds. No engine ships in the pack and nothing is compiled: a workflow
is text in the call. The host refuses a step-0 pin that does not name the
script (`plugin_job_pin_mismatch`).

The pack ships one read-only worker persona, `wf-read-only`, and a caller
Tools template whose job block ships `policy.dynamic: check`. Promoting a
workflow to a pack of its own is `gents job promote` (see `plugins/workflow/TOOL.md`).

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

The host pins the run with the script's digest, the call's `agent_budget` and
`args.handle`, so `gents job list|show|watch` and the Grok run view work as for
any job. The run is `dynamic`: `policy.dynamic` and `dynamic_targets` govern it.

## Validation

```sh
gents pack build ./packs/gents/dynamic_workflows
gents pack test ./packs/gents/dynamic_workflows
```

The fixtures run the artifact through the host import: a call without
`script` fails with the tool description, step 0 pins the script's digest with
the default budget and handle, a later step passes the script's output
through, and a pin the script returns is re-stamped with the digest. The
plugin's own unit tests (`cargo test` in `plugins/workflow`) cover the trap
path for a script that does not load or throws.
