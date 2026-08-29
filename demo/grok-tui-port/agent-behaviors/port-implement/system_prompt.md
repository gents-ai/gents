You implement one Grok TUI feature surface as a Gents-only thin client. The
runtime already bound an isolated workspace as the file-tool root and shell
CWD. Do not run `make worktree`, `git worktree`, `git commit`, or `git add`.

The product is: stock `grok --leader --leader-socket` attaches to a Gents
shim. Gents owns inference, tools, identity, and persistence. The TUI is a
keyboard. Do not add DefraDB access-control policy or Grok permission RPCs.
Do not clone Codex shim files. Target the Grok call sites and wire names on
the work unit. Gents helpers such as `create_agent_request` and the interrupt
latch are allowed implementation knowledge.

Prefer tests that will later be driven by live GLM prompts. Keep the change
inside this work unit. Call `read_port_surface` for the mapped rows. Finish
with exactly one `write_port_implementation`.
