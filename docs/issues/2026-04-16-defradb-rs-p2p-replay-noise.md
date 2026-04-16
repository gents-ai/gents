# Issue: `defradb.rs` P2P Replay / Push Noise Under Concurrent Live Desktop Chats

This note is intended for an agent investigating P2P stability in
`defradb.rs`.

## Repro

Command:

```bash
DEFRA_AGENT_DESKTOP_LIVE_BACKEND_ENDPOINT=http://workstation-1:8000/v1 \
DEFRA_AGENT_DESKTOP_LIVE_BACKEND_MODEL=MiniMax-M2.7-NVFP4 \
DEFRA_AGENT_DESKTOP_LIVE_BACKEND_PROVIDER=openai-compatible \
cargo test -p defra-agent-desktop desktop_app_live_inference_smoke -- --ignored --nocapture
```

What the smoke does:

- starts a desktop embedded node
- starts a peer embedded node
- spawns a live agent backed by the workstation model
- configures file tools
- creates two conversations
- submits overlapping live requests
- verifies both conversations can complete tool-driven responses
- switches activities and returns to chat

Important outcome:

- the smoke now passes end-to-end
- the transport still emits a large amount of replay / push / shutdown noise during the run

## Observed Warning / Error Families

These show up repeatedly during a passing run:

- `Existing document replay PushLog failed ... error=dial error: closed`
- `Existing document replay PushLog failed ... error=codec error: failed to read length: connection lost`
- `Existing document replay PushLog failed ... error=channel send error`
- `Existing document replay PushLog failed ... error=channel receive error`
- `Failed to send two-stream response via token ... error=codec error: failed to write length: connection lost`
- `Failed to complete connection handshake: aborted by peer: the application or application protocol caused the connection to be closed during the handshake`
- `Failed to get replicators for push error=channel send error`
- `Failed to broadcast to both topics ... doc_error=channel send error collection_error=channel send error`
- `Fire-and-forget broadcast failed — document committed locally but NOT replicated`
- `Iroh endpoint task did not stop after graceful shutdown; aborting`
- `Endpoint dropped without calling Endpoint::close. Aborting ungracefully.`

## Why This Is Interesting

The desktop smoke is no longer failing on basic chat behavior:

- two concurrent live chats complete
- tool-driven file reads work
- follow-up messages work
- session switching works

But the underlying transport still looks unhealthy while that is happening.

That suggests at least one of these:

- replay repair is retrying against stale or already-closed peer/stream state
- replicator bookkeeping can outlive the connection it depends on
- transport/channel teardown races with replay push scheduling
- shutdown ordering around the Iroh endpoint is incomplete
- successful foreground chat traffic is masking background replication failures

## Context From Desktop-Side Fixes

The desktop changes that improved stability were:

- peer maintenance now checks live connectivity instead of trusting stale `dial_succeeded`
- repair loop interval reduced from 5s to 2s
- peer add uses the same retry/wait path as bootstrap
- embedded desktop defaults now use a larger push budget:
  - `max_concurrent_push_tasks = 32`
  - `rate_limit_burst = 5000`
  - `rate_limit_rate = 500.0`

Those changes made the live smoke reliable enough to pass, but they did not eliminate the underlying warnings above.

## Questions For `defradb.rs`

1. Why can `PushLog` replay continue to hammer `channel send error` and `channel receive error` after a connection is clearly unhealthy?
2. Is replay work being requeued against a dead transport handle instead of forcing a fresh transport / replicator rebind?
3. Why do topic broadcast paths report `document committed locally but NOT replicated` during a run that otherwise keeps chat functional?
4. Is there a known race between connection teardown and:
   - replay push tasks
   - replicator lookup
   - two-stream response send
5. Why does the endpoint still need abort-on-drop instead of a clean close path at test shutdown?

## Representative Snippets

```text
WARN Existing document replay PushLog failed ... error=channel send error
WARN Existing document replay PushLog failed ... error=codec error: failed to read length: connection lost
WARN Failed to send two-stream response via token ... error=codec error: failed to write length: connection lost
WARN Failed to complete connection handshake: aborted by peer ...
WARN Failed to get replicators for push error=channel send error
ERROR Failed to broadcast to both topics ... doc_error=channel send error collection_error=channel send error
ERROR Fire-and-forget broadcast failed — document committed locally but NOT replicated
WARN Iroh endpoint task did not stop after graceful shutdown; aborting
ERROR Endpoint dropped without calling `Endpoint::close`. Aborting ungracefully.
```

## Practical Next Steps Upstream

- trace replay task ownership against connection lifecycle
- inspect whether failed replay work keeps a stale sender / receiver alive
- inspect replicator lookup and topic broadcast after transport reset
- add a focused integration test that:
  - opens two concurrent chats
  - forces a peer disconnect / reconnect mid-run
  - asserts replay recovery does not spin on channel errors
- tighten endpoint shutdown so tests do not end with forced abort
