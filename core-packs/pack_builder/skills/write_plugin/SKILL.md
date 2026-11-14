---
name: write-plugin
description: Load when logic must be exact: rules, calculations, validation, anything replayable. Contract, data, tests.
---

# Write a plugin

A plugin is code the node runs sealed: the same input always gives the same answer, so agents and pages can trust it. Use one wherever a rule must hold no matter who calls it.

Build it live with `config`: `plugin build NAME` takes the source, its data declaration and test cases, compiles and tests them, and installs `node/NAME` (`help plugin`, `help plugin-source`). No file is needed. JavaScript is the shortest path: `module.exports = (input) => ({ ok: true })`. Keep it pure: input in, answer out. Take randomness from a seed in the input, so results can be replayed.

## Contract

One JSON value in, one out. An error the caller can act on is a normal output, such as `{ "ok": false, "refusal": { "code": "...", "message": "..." } }`.

## Reading documents

Declare access in the source document's `data` field. Without it, the plugin has no data access:

```json
"data": { "collections": ["GameEvent"], "ops": ["query", "insert"], "stamped": {} }
```

To read, return this; a bare array is refused:

```json
{ "data_calls": { "requests": [ { "id": "log", "query": { "collection": "GameEvent", "filter": { "game_id": { "_eq": "g1" } }, "order": { "seq": "ASC" }, "limit": 256, "fields": ["seq", "kind"] } } ] } }
```
 The host runs the requests and calls the plugin again with the original input. Results arrive at `input.data_results.<request id>.result.rows`. A query returns at most 256 rows, so page with `offset`. A call takes at most 64 rounds.

## Writing documents

Put `writes` on the final output: `"writes": [ { "op": "insert", "collection": "GameEvent", "fields": { "seq": 12 } } ]`. At most 100 per call, committed together. The plugin's rules decide what enters the log.

## Test before you install

- Give `plugin build` one test case per rule as `input` and `expect` pairs, including the refusals that must happen. A failing case installs nothing and names the case.
- Try the installed plugin with `plugin run` and a real input.
- To give an agent the tool, select it in that agent's Tools. A rebuild changes the digest, so update the selection after it.
- If you can't run the tests, report the plugin as not tested.

Then return to `plan-work`.
