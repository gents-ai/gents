You implement the one cohesive Grok TUI shim work unit as a Gents-only thin
client. The runtime already bound an isolated workspace as the file-tool root
and shell CWD. Do not run `make worktree`, `git worktree`, `git commit`, or
`git add`.

The product is: stock `grok --leader --leader-socket` attaches to a Gents
shim. Gents owns inference, tools, identity, and persistence. The TUI is a
keyboard. Do not add DefraDB access-control policy or Grok permission RPCs.
Do not clone Codex shim files. Target the Grok call sites and wire names on
the work unit. Gents helpers such as `create_agent_request` and the interrupt
latch are allowed implementation knowledge.

The architecture is settled: create a fresh `grok_shim` command module inside
`gents-cli`, use the smallest existing server launch/config seam to bind the
leader socket, and keep runtime lifecycle documents runtime-owned. Do not add
a crate, schema fields, Lean changes, or a new generic runtime abstraction.
Start with the two required datastore reads. Your third tool batch must make a
tracked scaffold edit with `write_file` or `edit_file`: create the fresh
`grok_shim` module and expose it from `commands/mod.rs`. It is fine for that
first scaffold to be incomplete while you inspect the exact helper signatures.
Do not perform repository discovery before this scaffold edit, do not edit via
shell, and do not revisit already-settled client/leader direction or
AgentRuntime field ownership. After the scaffold exists, inspect narrowly and
build it out in compile-tested slices.

Prefer tests that will later be driven by live GLM prompts. Keep the change
inside this work unit. Call `read_port_surface` for the mapped rows. Finish
with exactly one `write_port_implementation`.
