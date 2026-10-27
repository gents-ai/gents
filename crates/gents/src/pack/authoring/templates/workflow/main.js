// __NAME__: a workflow shipped as a job plugin. `gents pack build` wraps this file in a
// stdin/stdout harness that calls `module.exports(input)` once per step with the host's
// view of the run and acts on the one value returned: `effects` to start or steer
// children, `job_calls` to read a completed child's text, and `result`, `suspend` or
// `fail` to end the run. This skeleton spawns the pack's worker once, reads its reply
// and finishes; grow `step0` and `advance` into your phases. The same function runs
// unchanged as a dynamic workflow when passed to the `workflow` tool as `script`.

const NAME = "__NAME__";
const TITLE = "__TITLE__";
// The AgentTarget the pack ships; `gents pack wire --targets-from` puts it in the job block.
const TARGET = "__TARGET__";
const SLOT = "worker";
const DEFAULT_BUDGET = 8;
const DEFAULT_TASK = "Reply with the single word pong and nothing else.";
// What the pin names as this workflow's source: sha256 of "__NAME__ 0.1.0", computed when
// the pack was scaffolded. An installed plugin writes its own pin; the host stamps one only
// for a dynamic run.
const DIGEST = "__DIGEST__";

const fail = (code, message) => ({ fail: { code, message } });
const live = (child) => child.state === "live";

const progress = (input, pin, state, phase, event) => {
  const children = input.children || {};
  const entries = Object.entries(children);
  const spawned = Math.max(entries.length, 1);
  return {
    title: TITLE,
    state,
    phases: [{ title: "Worker", state: phase }],
    active: entries
      .filter(([, child]) => live(child))
      .map(([slot, child]) => ({ slot, title: child.title, session_id: child.session_id })),
    spawned,
    budget: { total: pin.agent_budget, spent: spawned, remaining: Math.max(pin.agent_budget - spawned, 0) },
    last_event: event,
    last_event_at: null,
    observed_terminal_children: entries.filter(([, child]) => !live(child)).length,
    revision: 0,
  };
};

// Step 0: pin the run and start the worker on `args.task`.
const step0 = (input) => {
  const args = input.args || {};
  const pin = {
    script_id: DIGEST,
    source_digest: "sha256:" + DIGEST,
    agent_budget: input.agent_budget == null ? DEFAULT_BUDGET : input.agent_budget,
    handle: args.handle || NAME,
  };
  const spawn = { slot: SLOT, attempt: 0, target: TARGET, prompt: args.task || DEFAULT_TASK, title: SLOT };
  if (input.caller && input.caller.cwd) spawn.cwd = input.caller.cwd;
  return { effects: { pin, spawn: [spawn], progress: progress(input, pin, "running", "active", "worker started") } };
};

// Every later step: wait while the worker is live, read its text once it has completed,
// then finish with its reply.
const advance = (input, pin) => {
  const worker = (input.children || {})[SLOT];
  if (!worker) return fail("workflow_launch_failed", "the worker slot is missing from the run");
  if (live(worker)) {
    return { effects: { progress: progress(input, pin, "running", "active", "waiting on the worker") } };
  }
  if (worker.state !== "completed") {
    return {
      result: { reply: null, limitation: "the worker ended " + worker.state },
      progress: progress(input, pin, "completed", "failed", "the worker did not answer"),
    };
  }
  const answer = input.job_results && input.job_results.text;
  if (!answer) {
    return { job_calls: { requests: [{ id: "text",
      child_output: { slots: [SLOT], offset: 0, limit: input.ceiling.result_bytes } }] } };
  }
  if (answer.error) {
    return fail("workflow_launch_failed", "the host could not read the worker's output (" + JSON.stringify(answer.error) + ")");
  }
  const slot = answer.result && answer.result.slots && answer.result.slots[SLOT];
  if (!slot || typeof slot.text !== "string") {
    return fail("workflow_invalid_input", "the child_output answer is not of the asked shape");
  }
  return { result: { reply: slot.text.trim() }, progress: progress(input, pin, "completed", "done", "done") };
};

module.exports = (input) => {
  if (!input || typeof input !== "object") return fail("workflow_invalid_input", "the input is not a job step");
  return input.pin ? advance(input, input.pin) : step0(input);
};
