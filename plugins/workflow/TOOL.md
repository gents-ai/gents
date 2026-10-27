# workflow

Run a multi-step, multi-agent workflow you write yourself, as a background job.
Pass the workflow as `script`: a JavaScript module whose `module.exports` is a
step function `(input) => output`. The host calls it once per step with its
view of the run and acts on what it returns. The call returns a receipt at
once; the result arrives later as a message. End your turn after the call.

## Input

- `script`: the JavaScript source (required).
- `args`: an object your step function reads as `input.args`; `args.handle`
  names the run, so use the name the user gave it.
- `agent_budget`: how many child agents the run may start. Pass the number of
  children your script spawns; the default is 8.

## The step function

`input` has `pin` (null on the first step), `args`, `agent_budget`, `children`
(slot -> `{state: live|completed|failed|interrupted, title, usage}`),
`ceiling` (`targets`, `max_live`, `remaining_spawns`, `result_bytes`), `state`
(what your last step returned as `effects.state`, or null) and, on a round,
`job_results`.

`output` is exactly one of:
- `{effects: {spawn: [{slot, attempt: 0, target, prompt, title}], message: [{slot, text}], state}}`
  to start or steer children (`target` is one of `ceiling.targets`). Those three
  keys only: the host stamps `progress` itself; do not send it.
- `{job_calls: {requests: [...]}}` to read stored text. A request is exactly
  `{id, child_output: {slots: [...], offset: 0, limit: input.ceiling.result_bytes}}`
  (completed children's text, answered in `input.job_results.<id>.result.slots[slot].text`)
  or `{id, result: {row_doc_id, offset, limit}}` (another job's result); no other
  shape exists. The host re-invokes your function at once with `input.job_results`
  filled in, and that invocation must return `effects`, `result`, `suspend` or
  `fail`, never the same request again.
- `{result: <any JSON>}` to finish; `{suspend: {kind: "pause", message}}` to pause;
  `{fail: {code, message}}` to fail.

A step with live children and nothing new to do returns `{effects: {}}`.
Three malformed outputs in a row fail the run `plugin_job_step_failed`; the
failure names the last error.

## Example: fan out, read, judge

Three workers each take one side; a fourth judges from their texts. Every
phase is read off `input.children`, so the function needs no `state`.

```js
module.exports = (input) => {
  const T = input.ceiling.targets[0];
  const sides = (input.args && input.args.sides) || ["economic", "military", "cultural"];
  const slots = sides.map((_, i) => "side" + i);
  const c = input.children;
  const done = (slot) => c[slot] && c[slot].state !== "live";
  const completed = (slot) => c[slot] && c[slot].state === "completed";
  const read = (want) => ({ job_calls: { requests: [{ id: "text",
    child_output: { slots: want, offset: 0, limit: input.ceiling.result_bytes } }] } });
  const texts = () => input.job_results && input.job_results.text.result.slots;

  // Step 0: nothing has run yet.
  if (!input.pin) {
    return { effects: { spawn: sides.map((side, i) => ({
      slot: slots[i], attempt: 0, target: T, title: "Argue " + side,
      prompt: "In 200 words, argue that the " + side + " cause was the main cause of the fall of Rome. Answer with the argument only.",
    })) } };
  }
  // Phase 1: the sides are running.
  if (!slots.every(done)) return { effects: {} };
  // Phase 2: the sides are done and the judge has not started: read the completed sides' texts
  // (the host refuses child_output for a failed child), then spawn the judge.
  if (!c.judge) {
    const ok = slots.filter(completed);
    const t = ok.length ? texts() : {};
    if (!t) return read(ok);
    const brief = slots.map((s, i) => "## " + sides[i] + "\n" + (t[s] ? t[s].text : "(no answer)")).join("\n\n");
    return { effects: { spawn: [{ slot: "judge", attempt: 0, target: T, title: "Judge",
      prompt: "Three arguments follow. Say which is strongest and why, in 150 words.\n\n" + brief }] } };
  }
  // Phase 3: the judge is running, or done.
  if (!done("judge")) return { effects: {} };
  if (!completed("judge")) return { result: { verdict: "(no answer)", sides } };
  const t = texts();
  if (!t || !t.judge) return read(["judge"]);
  return { result: { verdict: t.judge.text, sides } };
};
```

## What the host refuses

- Any key beside `effects` other than the ones listed; `state` goes inside
  `effects`, not beside it.
- A spawn without `attempt`, `target`, `prompt` or `title`, or with a `target`
  not in `ceiling.targets`.
- A step that spawns nothing new and sends no message while no child is live:
  the run fails `plugin_job_stalled`. Every child terminal plus an empty step is
  a stall, so the step after your last child completes must read, spawn or
  finish. Spawning a slot and attempt that already ran is not new, so decide the
  phase from `input.children`, never by re-sending step 0.
- Reading a child's text from `children[slot]`: it is not there. Ask with
  `job_calls` and read `job_results` on the next call, as above.
- A worker whose prompt needs files or tools it lacks answers that it cannot;
  prompts that ask for reasoning or prose are fine.

## Promoting a workflow

When asked to promote a workflow you ran, do not build a pack yourself.
Answer with the command the operator runs, filled in from the run's receipt:
`gents job promote <address> --name <snake_case_name>`.
It installs the run's script as the tool `<name>`, wired to this agent;
afterwards call `<name>` with `args` and `agent_budget`.
