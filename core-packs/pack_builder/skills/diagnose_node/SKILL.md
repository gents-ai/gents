---
name: diagnose-node
description: Load when a trigger, tool, request or session didn't work as expected. Gives the order of checks.
---

# Diagnose the node

Find the first step that didn't happen. Change nothing until you know which one it is. For a trigger, start with `trigger explain`. If sessions or fires are multiplying, disable the trigger first.

## Nothing fired

Stop at the first break:

1. Does the source document exist?
2. Is the event source enabled and watching the right collection?
3. Does its filter match that document?
4. Is the trigger enabled and pointing at an enabled task?
5. Was a fire recorded? Check the trigger's status and last error.
6. Was a request created? Read its state and failure reason.
7. Did the agent have the tools? Tools apply to later requests only.

## A tool is missing

`config agent get` shows what the agent's Tools resolve to. Selection alone doesn't prove a tool works; only a successful call does.

## A page is wrong

Run the preview (see `design-page`), and check the manifest lists every surface and tool the page calls.

## Rules

- Keep what you observed apart from what you concluded. Don't name a cause until a test separates it from the alternatives.
- Ask for a restart only when you can say what it will prove.
- Still stuck after three attempts at the same step? Stop and ask, with what you tried and ruled out.
