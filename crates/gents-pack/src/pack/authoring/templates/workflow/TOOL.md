# __NAME__

Run the __TITLE__ workflow as a background job: it starts the pack's worker on
`args.task`, reads the reply and finishes with it. The call returns a receipt at
once; the result arrives later as a message. End your turn after the call.

## Input

- `args.task`: what the worker is asked to do (default: answer `pong`).
- `args.handle`: a name for the run.
- `agent_budget`: how many child agents the run may start (default 8).

## Output

A receipt `{address, handle, status}`; later the result `{reply}`, or
`{reply: null, limitation}` when the worker did not answer.
