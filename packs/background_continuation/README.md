# Background continuation pack

This pack proves the operator-visible path behind #1114 and #1116:

```text
create BackgroundContinuationJob
              │
              ▼
      parent task completes
         │             │
         ▼             ▼
  background child  background child
         │             │
         └──── durable completion notifications ────┐
                                                    ▼
                                      coalesced continuation wake
                                                    │
                                                    ▼
                                     exact snapshot acknowledged
```

Run it with a fresh home:

```bash
gents pack run background_continuation
```

The runner requires two completed depth-positive child requests, at least one
completed canonical background-completion wake, at least two acknowledged
notification keys, and zero pending or stranded notifications. It also exports
the parent and wake timelines plus ATIF, OpenAI Codex, LangGraph, and
multi-agent projections under `runs/<job_id>/projections/`.

This provider-backed pack validates the successful end-to-end and successor-
epoch paths. Crash cuts are kept deterministic: the Lean R6 contract emits the
before-claim, during-inference, after-response-persistence, and acknowledgement
restart cases; the queue tests exercise restart recovery, persisted-response
repair, failed-wake redrive, and exact successor acknowledgement against the
real store.

## Declared topology

Document-trigger edges; task writes and host callbacks are described above.

<!-- pack-topology:start -->
```mermaid
flowchart LR
    n0["BackgroundContinuationJob"]
    n1["background-parent-task"]
    n0 -->|"background-parent"| n1
```
<!-- pack-topology:end -->
