Run JavaScript in a WASM sandbox. Return the answer; `input` holds optional JSON input. Local variables survive `await`.

When tool access is enabled in Tools, `await tools.TOOL_NAME({...})` invokes that tool with the caller's permissions and returns its result. Failures throw catchable errors. Set `with_tools: false` for compute-only execution. Without a tool grant, `tools` is undefined. `await sleep(milliseconds)` waits without calling a model.

For background execution, call `spawn_process` with `{"tool_name":"script","args":{"script":"YOUR JAVASCRIPT","input":{}}}`. Its receipt names the running call; use the ordinary process controls to wait, inspect or cancel it. A background script keeps its caller's selected tool surface and runs within the existing background lifetime configured for that tool, rather than the foreground script time allowance. Calls to agent, goal and process tools retain those tools' normal behavior, including asynchronous receipts.

There is no filesystem, network, environment, shell, or module loading in JavaScript. Use granted tools for effects. Completed effects remain applied if the script fails. Scripts are bounded, and interrupted scripts are not resumed or replayed.
