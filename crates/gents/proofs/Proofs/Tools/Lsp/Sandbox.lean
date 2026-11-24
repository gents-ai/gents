import Proofs.Session.CommandPolicy.Sandbox

/-! The command mode a language server process runs under. -/

namespace Lsp

open CommandPolicy

/-- A language server is a long-lived process the runtime starts for a
workspace, and it writes while it works: rust-analyzer runs `cargo metadata`
and `cargo check`, which write `Cargo.lock` and `target/` in the workspace;
ruby-lsp writes `.ruby-lsp/` and elixir-ls `.elixir_ls/` in the workspace;
gopls, tsserver, deno and the Python servers write caches under the user's cache
directory; and every server may use the temporary directory. So under a
read-only command policy, a host with a sandbox backend runs language servers
under the workspace-write sandbox, which confines their writes to the
workspace and its temporary directory and refuses the user's cache directory; a
host without a backend has no profile to confine them and runs them
unrestricted. Under every other mode a server runs under that mode's sandbox. -/
def serverSandbox (sandboxEnforced : Bool) (mode : ExecutionMode) : ExecutionMode :=
  match mode with
  | .readOnly => if sandboxEnforced then .workspaceWrite else .unrestricted
  | .workspaceWrite => .workspaceWrite
  | .artifactWrite => .artifactWrite
  | .unrestricted => .unrestricted

theorem server_sandbox_never_widens_a_bounded_mode (enforced : Bool) :
    serverSandbox enforced .workspaceWrite = .workspaceWrite ∧
      serverSandbox enforced .artifactWrite = .artifactWrite := ⟨rfl, rfl⟩

end Lsp
