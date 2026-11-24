`Object.keys(tools)` lists the tools a script may call; `tools.NAME.description` and `tools.NAME.parameters` describe one without calling it. An unknown `tools.NAME`, or `tools` without a tool grant, is `undefined`.

`await tools.NAME({...})` runs that tool with the caller's permissions and returns its result, as data where the tool offers JSON. A failed call throws an `Error` carrying the tool's message; catch it to recover.

Calls started before awaiting them (e.g. `Promise.all`) are batched: up to four consecutive calls to read-only tools run together, and every other call (a write, a repeated call, or a call after `sleep`) runs alone, in order.
