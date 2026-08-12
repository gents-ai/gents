# Pack: rust-analyzer through the native `lsp` tool

Least-privilege coding surface: ReadOnly file tools rooted at the checked-in
crate, bash and every other tool off, `enable_lsp: true`. The model has to
call `lsp` (hover, definition, status) against rust-analyzer.

This pack is **not** a CI gate. Required CI still says no live rust-analyzer.
Run it locally when `rust-analyzer` is on `PATH` and the DeepSeek box (or
another OpenAI-compatible endpoint) is reachable.

```bash
# rust-analyzer must resolve on PATH
rust-analyzer --version

# From the repo root so the default file_tool_root exists
gents demo run lsp-rust

# Or pin an absolute workspace
GENTS_LSP_WORKSPACE=/abs/path/to/demo/lsp-rust/workspace \
  gents demo run lsp-rust --keep-home
```

The stronger assertion (completed `AgentToolCall` rows with `tool_name == "lsp"`
and a rust-analyzer hover/status result) lives in the ignored live test:

```bash
GENTS_LIVE_LSP=1 cargo test -p gents --test e2e_live \
  lsp_live_model_uses_rust_analyzer \
  -- --ignored --test-threads=1 --nocapture
```

## Layout

| Path | Role |
| --- | --- |
| `workspace/` | Tiny Rust lib with a known `add` symbol |
| `tool-selections/lsp-readonly/` | ReadOnly files + `enable_lsp`; bash off |
| `tasks/lsp-hover-task/` | Prompt forces hover → definition → status |
| `event_triggers/lsp-hover/` | Fires on `LspDemoJob` create |

## Tools

| Behavior | Tools | Why |
| --- | --- | --- |
| lsp-coder | `read_file` / `list_files` / `glob` / `grep` (ReadOnly) + `lsp` | rust-analyzer needs a file root; no writes, no bash |
