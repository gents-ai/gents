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
workflow to a pack of its own is `gents automation job promote` (see `plugins/workflow/TOOL.md`).

## Operator steps

```sh
gents config pack install gents/dynamic_workflows --home ./.gents --node-did "$NODE_DID" \
  --grant-authority --inference-slot worker=<profile>
gents config pack wire gents/workflow --agent <A> --targets-from gents/dynamic_workflows
```

`--agent` takes the agent's DID (`gents config agent list`). The install's
`--wire <agent-did>` does the same wiring in one step.

The wired job block copies `Tools/workflow-caller`: `agent_targets` and
`dynamic_targets` are `wf-read-only`, and the caller's `agents.target_ids`
lists it too, since a Tools document whose job names a target its own
`agents.target_ids` leaves out is refused when written. `agents.enabled`
stays off, so the agent cannot start the worker directly. A pack may ship `policy.dynamic` only as
`off` or `check` (gents `job_block_admissible`), and `check` refuses every
dynamic spawn and message, so no workflow runs until the agent's Tools say
`run`. Wiring never sets `run` and never changes a policy the operator set;
for the Engineer, enabling the pack sets it, with the pack's dynamic targets,
and reports each block before and after:

```sh
gents config pack starter --enable gents/dynamic_workflows --home ./.gents
```

On another agent the operator edits its Tools document (`gents config tools
show <tools-id>`, then `gents config tools set --file tools.json`).

## Authoring

See `plugins/workflow/TOOL.md`, which the model reads as the tool description.

The host pins the run with the script's digest, the call's `agent_budget` and
`args.handle`, so `gents automation job list|show|watch` and the Grok run view work as for
any job. The run is `dynamic`: `policy.dynamic` and `dynamic_targets` govern it.

## Validation

```sh
gents config pack build ./packs/gents/dynamic_workflows
gents config pack test ./packs/gents/dynamic_workflows
```

The fixtures run the artifact through the host import: a call without
`script` fails with the tool description, a run without `args.handle` fails,
step 0 pins the script's digest with the default budget and the call's
handle, a later step passes the script's output through, and a pin the script
returns is re-stamped with the digest. The plugin's own unit tests (`cargo
test` in `plugins/workflow`) cover the trap path for a script that does not
load or throws.
