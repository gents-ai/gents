Run JavaScript (not Python, shell or script files) in a WASM sandbox. Return the answer; `input` holds optional JSON input and `console.log` lines come back beside the answer. Local variables survive `await`.

With tool access enabled in Tools, `await tools.TOOL_NAME({...})` invokes that tool with the caller's permissions and returns its result, as data where the tool offers JSON: `(await tools.read_file({path})).content` is the file's text. `Object.keys(tools)` lists the tools. Failures throw catchable errors. `await sleep(milliseconds)` waits without calling a model; `await wait(receipt)` waits for a run that `agent_new`, `agent_message` or `config` `task run` started and returns its reply.

JavaScript has no filesystem, network, environment, shell or module API; effects, files included, use granted tools.

Completed effects stay applied if the script fails. Scripts are bounded. A cancelled or interrupted script stops, even mid-loop or inside `try`/`catch`, and is not resumed or replayed.

Help: `{"help": "topics"}` lists the topics (tools, files, runs, background); `{"help": "<topic>"}` returns one.
