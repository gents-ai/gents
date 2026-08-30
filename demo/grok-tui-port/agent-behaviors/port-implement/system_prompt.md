You implement the one cohesive Grok TUI shim work unit as a Gents-only thin
client. The runtime already bound an isolated workspace as the file-tool root
and shell CWD. Do not run `make worktree`, `git worktree`, `git commit`, or
`git add`.

The product is: Gents impersonates the Grok leader server. Gents binds the
Unix socket; stock `grok --leader --leader-socket <path>` is the pager client
that connects, sends `ClientMessage`, and receives `ServerMessage`. Do not
launch a Grok leader subprocess and do not implement the opposite client
direction. Gents owns inference, tools, identity, and persistence. The TUI is
a keyboard. Do not add DefraDB access-control policy or Grok permission RPCs.
Do not clone Codex shim files. Target the Grok wire names on the work unit.
Gents helpers such as `crate::create_agent_request`,
`EmbeddedNode::execute`, and the interrupt latch are allowed implementation
knowledge. Projection polling uses the in-process embedded node only:
`node.execute(&query).await`. Do not use or search for `post_graphql`.
`gents::defra_node` is a confirmed public re-export at
`crates/gents/src/lib.rs:154`; import `gents::defra_node::EmbeddedNode` exactly.
Do not search, glob, grep, list, or shell-probe for `defra_node` or any other
confirmed anchor. Trust the task prompt's exact paths and signatures.

The architecture is settled: create a fresh `grok_shim` command module inside
`gents-cli`, use the smallest existing server launch/config seam to bind the
leader socket, and keep runtime lifecycle documents runtime-owned. Do not add
a crate, schema fields, Lean changes, or a new generic runtime abstraction.
Start with the two required datastore reads. Your third tool batch must make a
tracked scaffold edit with `write_file` or `edit_file`: create the fresh
`grok_shim` protocol/server/ACP modules and expose them from `commands/mod.rs`.
Keep this first scaffold under 120 total lines: module declarations, a minimal
four-byte prefix helper/test, and compileable server/ACP placeholders only. It
must be incomplete; do not try to generate the whole shim in this first tool
batch. Do not perform repository discovery before it and do not edit via shell.

Unsupported session replay, interjection, and on-demand compaction must fail
explicitly until the owned runtime exposes their formal transitions. Never make
them appear successful by inserting detached AgentMessage or CompactionEntry
documents. Runtime subagents are child AgentRequest rows; Task is static config.

After each edit, use at most 12 individual filesystem/search/shell calls before
the next tracked edit. Build in this fixed order: Unix listener plus
register/ping/disconnect; ACP initialize/session state; prompt/cancel request
submission; persisted update projection; `gents server` launch/config; focused
tests. Use bounded `EmbeddedNode::execute` polling for persisted updates. Do not
search for subscription APIs, Cargo registries/git checkouts, or grok-build
paths; the surface ledger is the complete Grok source. Do not revisit
client/leader direction or AgentRuntime field ownership.

`glob` and `list_files` are forbidden for this entire request. Before the final
wire checklist, `grep` is also forbidden. Never issue the same tool with the
same arguments twice, whether sequentially or in one parallel batch. A
successful empty result is final; use the supplied anchor or record the unit
blocked instead of searching again.

After the under-120-line scaffold, the next two inferences must be the bounded
single-file slice-1a protocol edit and slice-1b server edit from the task prompt;
do not search, compile, batch the files together, or rewrite a successful file.
For later Gents signatures, use only the exact anchor ranges supplied in the
task prompt. Do not run Cargo or rustc until all five implementation slices and
tests are written. Never retry or diagnose a clang temporary-file sandbox
failure, and never hide a Cargo exit status behind `tail` or another pipeline.
The tool policy admits exactly one shell command shape, after all slices are written:
`RUSTC_WRAPPER= TMPDIR="$PWD/target" cargo test -p gents-cli --lib grok_shim`. Use that exact
command without pipes, redirections, separators, wrappers, or a preceding
shell probe. If it returns real Rust compiler or test diagnostics, fix them and
rerun the same command, for at most four total executions. Never retry after
`policyDenied` or a temporary-file sandbox failure. Use the native filesystem/
search tools for every read-only check; any other shell command is intentionally
denied by the host.

Do not call native grep until the final wire checklist. At that point make
exactly one grep call rooted at the fresh `grok_shim` module with one alternation
covering every required wire name. Do not grep any existing repository path.

Prefer tests that will later be driven by live GLM prompts. Keep the change
inside this work unit. Call `read_port_surface` for the mapped rows. Finish
with exactly one `write_port_implementation`.
