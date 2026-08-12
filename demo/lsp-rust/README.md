# Pack: rust-analyzer through the native `lsp` tool

Least-privilege coding surface: ReadOnly file tools rooted at **this Gents
repository**, bash and every other tool off, `enable_lsp: true`. The model
has to call `lsp` (hover + status) against rust-analyzer and answer two
checkable questions about the runtime crate.

This pack is **not** a CI gate. Required CI still says no live rust-analyzer.
Run it locally when `rust-analyzer` is on `PATH` and the DeepSeek box (or
another OpenAI-compatible endpoint) is reachable.

```bash
# rust-analyzer must resolve on PATH
rust-analyzer --version

# From the repo root so rust-analyzer sees the Cargo workspace
gents demo run lsp-rust

# Or pin an absolute workspace
GENTS_LSP_WORKSPACE=/abs/path/to/gents \
  gents demo run lsp-rust --keep-home
```

The stronger assertion (completed `AgentToolCall` hover rows whose results
contain `FileToolMode` and `Disabled`/`Inherit`, plus `rust-analyzer (ready)`)
lives in the ignored live test:

```bash
GENTS_LIVE_LSP=1 cargo test -p gents --test e2e_live \
  lsp_live_model_uses_rust_analyzer \
  -- --ignored --test-threads=1 --nocapture
```

`workspace/` remains a tiny isolated crate used by the rust-analyzer unit
test. The live pack and e2e point at the real Gents tree.

## Layout

| Path | Role |
| --- | --- |
| `workspace/` | Tiny Rust lib for the offline rust-analyzer unit test |
| `tool-selections/lsp-readonly/` | ReadOnly files + `enable_lsp`; bash off; root is the repo |
| `tasks/lsp-hover-task/` | Prompt asks checkable hover questions, then status |
| `event_triggers/lsp-hover/` | Fires on `LspDemoJob` create |

## Tools

| Behavior | Tools | Why |
| --- | --- | --- |
| lsp-coder | `read_file` / `list_files` / `glob` / `grep` (ReadOnly) + `lsp` | rust-analyzer needs a file root; no writes, no bash |
