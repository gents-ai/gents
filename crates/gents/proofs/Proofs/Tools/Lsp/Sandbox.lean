import Proofs.Session.CommandPolicy.Sandbox

/-! The command mode a language server process runs under. -/

namespace Lsp

open CommandPolicy

/-- A language server is a long-lived process the runtime starts for a
workspace, and it writes and executes while it works: rust-analyzer runs
`cargo metadata` and `cargo check`, which run build scripts and proc macros and
write `Cargo.lock` and `target/`; ruby-lsp writes `.ruby-lsp/` and elixir-ls
`.elixir_ls/` in the workspace; gopls, tsserver, deno and the Python servers
write caches under the user's cache directory. A read-only command policy
grants none of that, so under it no language server runs: the surface does not
offer the `lsp` tool, and a call whose request narrows the policy to read-only
is refused before any server starts. Under every other mode a server runs under
that mode's sandbox. -/
def serverSandbox (mode : ExecutionMode) : Option ExecutionMode :=
  match mode with
  | .readOnly => none
  | .workspaceWrite => some .workspaceWrite
  | .artifactWrite => some .artifactWrite
  | .unrestricted => some .unrestricted

theorem read_only_runs_no_server (mode : ExecutionMode) :
    serverSandbox mode = none ↔ mode = .readOnly := by
  cases mode <;> simp [serverSandbox]

theorem server_sandbox_is_the_command_mode (mode : ExecutionMode) (sandbox : ExecutionMode)
    (runs : serverSandbox mode = some sandbox) : sandbox = mode := by
  cases mode <;> simp [serverSandbox] at runs <;> exact runs.symm

end Lsp
