# workflow

Run a JavaScript workflow as a background job. Pass `script` (required), a
module exporting `module.exports = (input) => output`; `args` is available as
`input.args` (`args.handle` names the run), and `agent_budget` bounds child
starts (default 8; pass the number spawned). The call returns a receipt now;
the result arrives later as a message. End your turn after the call.

Each step receives `pin` (null initially), `args`, `agent_budget`, `children`
(slot to `{state: live|completed|failed|interrupted, title, usage}`), `ceiling`
(`targets`, `max_live`, `remaining_spawns`, `result_bytes`), `state` (last
`effects.state` or null), and, on a read round, `job_results`.

Return exactly one of:
- `{effects: {spawn: [{slot, attempt: 0, target, prompt, title}], message: [{slot, text}], state}}`:
  start or steer children. Only these effects keys; `target` must be in
  `ceiling.targets`. The host stamps progress. With live children and nothing
  new to do, return `{effects: {}}`.
- `{job_calls: {requests: [...]}}`: read text. Each request is exactly
  `{id, child_output: {slots: [...], offset: 0, limit: input.ceiling.result_bytes}}`
  for completed children, or `{id, result: {row_doc_id, offset, limit}}` for
  another job's result. Child text is not in `children`: read it at
  `input.job_results.<id>.result.slots[slot].text`. Never request failed or
  interrupted children. The host immediately calls the step with
  `job_results`; it exists only on the step after a read, so use it then or
  carry what you need in `effects.state`. That round must return `effects`, `result`, `suspend` or
  `fail`, not another read request.
- `{result: <any JSON>}` to finish.
- `{suspend: {kind: "pause", message}}` to pause.
- `{fail: {code, message}}` to fail.

Decide the phase from `input.children`, never by re-sending step 0. A step
with no live child that spawns nothing new and sends no message fails
`plugin_job_stalled`. Resending an existing slot and attempt is not a new
spawn. Three malformed outputs in a row fail `plugin_job_step_failed`. Put
`state` inside `effects`; no other top-level keys are allowed.

To promote a run, give the operator the command from its receipt, not a pack:
`gents automation job promote <address> --name <snake_case_name>`. It installs
the script as tool `<name>` wired to this agent; call it with `args` and
`agent_budget` afterwards.
