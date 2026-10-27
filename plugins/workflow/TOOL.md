# workflow

Run a multi-step, multi-agent workflow you write yourself, as a background job.
Pass the workflow as `script`: a JavaScript module whose `module.exports` is a
step function `(input) => output`. The host calls it once per step with its
view of the run and acts on what it returns. The call returns a receipt at
once; the result arrives later as a message. End your turn after the call.

## Input

- `script`: the JavaScript source (required).
- `args`: an object your step function reads as `input.args`; `args.handle`
  names the run.
- `agent_budget`: how many child agents the run may start (default 128).

## The step function

`input` has `pin` (null on the first step), `args`, `agent_budget`, `children`
(slot -> `{state: live|completed|failed|interrupted, title, usage}`),
`ceiling` (`targets`, `max_live`, `remaining_spawns`, `result_bytes`), `state`
(what your last step returned as `state`, or null) and, on a round, `job_results`.

`output` is exactly one of:
- `{effects: {spawn: [{slot, attempt: 0, target, prompt, title}], message: [{slot, text}], state, progress}}`
  to start or steer children (`target` is one of `ceiling.targets`);
- `{job_calls: {requests: [{id: "text", child_output: {slots: [...], offset: 0, limit: input.ceiling.result_bytes}}]}}`
  to read completed children's text, answered in `input.job_results.text.result.slots[slot].text`;
- `{result: <any JSON>}` to finish; `{suspend: {kind: "pause", message}}` to pause;
  `{fail: {code, message}}` to fail.

A step with live children and nothing new to do returns `{effects: {}}`.
