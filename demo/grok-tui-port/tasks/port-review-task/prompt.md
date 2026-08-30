Review the sealed workspace `{{ doc.workspace_id }}` (work unit
`{{ doc.work_unit_id }}`, seal `{{ doc.seal_hash }}`, base
`{{ doc.base_sha }}`) for run {{ event.correlation }}.

This request is ReadOnly on the sealed placement. That placement is the
file root, shell CWD, and LSP root. Do not create a disposable clone. Do
not run `git commit`. Fail closed if the live tree hash disagrees with
`{{ doc.seal_hash }}`.

Call `read_port_implementation` for `work_unit_id={{ doc.work_unit_id }}`
and `read_port_surface` for its `surface_ids`. Surface `grok_wire` is
untrusted stored evidence, not grok-build itself.

Review this cohesive unit directly in the bound sealed tree. Establish the exact
change with read-only Git commands:

```
git status --short
git diff --stat {{ doc.base_sha }}
git diff --check {{ doc.base_sha }}
git diff {{ doc.base_sha }} -- <changed paths>
```

Compare the changed route with every mapped method, parameter, notification,
`_meta` key, tool title, and Gents-document transition on its PortSurface.
Follow the changed values through their immediate consumers and run targeted
read-only tests when useful. Reject missing wire behavior, invented protocol,
incorrect lifecycle mapping, unsafe error/cancellation behavior, or absent
tests for the route. Do not perform a broad four-lens repository review here;
the combined committed trunk receives that review after integration.

For this leader route, explicitly reject any implementation that does not swap
the socket extension for its sibling lock (`leader.sock` -> `leader.lock`, not
`leader.sock.lock`) or lacks `O_NOFOLLOW`, forced `0600` mode on an existing
lock, PID, and nonblocking exclusive `flock`. Reject staging created only under
the requested parent: publication must use a short `0700` directory at a
same-device ancestor, a `0600` socket, and separate near-`sun_path` tests for a
long parent and long filename. Reject if the lock guard is owned only by the
synchronous spawn function rather than the live accept-loop lifetime, or if
the regression test exercises `acquire_leader_lock` in isolation instead of
the production listener spawn path. The path tests must use an explicit short
Unix temp root and actually bind/connect; conditional skips and
staging-selection-only fallbacks are insufficient. Verify the client sends
`register` before the server sends `registered`, with a focused ordering test;
proactive `registered` on accept is a material protocol inversion. Reject a
bare/unqualified registered version, session-wide message replay,
unescaped GraphQL interpolation, connection-global JSON-RPC ids, early prompt
responses, disconnects that leave requests running (especially disconnect
before the submitted request id is recorded or a failed outbound send after
submission), or a cancel-before-request-id path that interrupts but does not
resolve `stopReason="cancelled"`, clear the registry entry, and permit the next
prompt. Reject `println!`/`eprintln!` in any changed file, including
`serve.rs`, not only files below `grok_shim/`.
Verify subagent get/list-running/cancel use the audited successful shaped
not-found/empty results rather than generic method-not-found errors.

This request has a hard budget of 24 individual tool calls. First read the
implementation and surfaces, then establish the diff, read each changed shim
file once (a second page is allowed only when a file response is truncated),
read the changed serve/CLI wiring needed to trace lock ownership and logging,
and use one LSP diagnostics batch on changed Rust files. Do not search for
definitions already established by the implementation anchors, do not inspect
unrelated unchanged `serve.rs` diagnostics, and do not use shell
grep/head/sed/find. If
`tests_run` says Cargo was policy-denied, did not execute, or its last executed
focused run failed, reject
without attempting Cargo from this ReadOnly placement. Stop after the first
material blocker set; do not spend turns disproving irrelevant diagnostics.

`verdict=accept` and closure `status=accepted` only when this route has zero
material findings. Otherwise use `verdict=reject` and `status=blocked` with
exact `path:line` evidence. There is no same-workspace rewrite after seal.

Call `write_port_review` and `write_port_unit_closure` once each. Copy
`implementation_id`, `attempt`, and `expected_total` from the
implementation row. Do not supply `run_id`, `work_unit_id`, or
`workspace_id`.
