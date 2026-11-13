import Proofs.Tools.Lsp.Types

/-! Which LSP entry points may start a language server. A server-backed tool
action cold-starts its primary server only after its file argument resolves;
status and reload inspect or retire clients without starting one, and the
post-write hook only reuses a client that is already ready. -/

namespace Lsp

namespace LspAction

def mayColdStart : LspAction → Bool
  | .status | .reload => false
  | _ => true

end LspAction

inductive Entry where
  | toolCall
  | postWrite
  deriving DecidableEq, Repr

/-- The file argument a call carries. A foreign URI names a scheme other than
`file:`; no language server is asked to open it. -/
inductive FileArgument where
  | absent
  | workspacePath
  | foreignUri
  deriving DecidableEq, Repr

inductive Refusal where
  | unsupportedUri
  deriving DecidableEq, Repr

inductive ServerReport where
  | notStarted
  | running
  deriving DecidableEq, Repr

def inboundRefusal : FileArgument → Option Refusal
  | .foreignUri => some .unsupportedUri
  | _ => none

/-- Only a cold-starting action resolves its file argument, and it does so
before it leases a server. -/
def callRefusal (action : LspAction) (file : FileArgument) : Option Refusal :=
  if action.mayColdStart then inboundRefusal file else none

/-- Whether an entry starts a server in a workspace whose primary server is
configured and detected but not yet running. -/
def startsServer : Entry → Option LspAction → FileArgument → Bool
  | .postWrite, _, _ => false
  | .toolCall, none, _ => false
  | .toolCall, some action, file => action.mayColdStart && (callRefusal action file).isNone

def serverReport (running : Bool) : ServerReport :=
  if running then .running else .notStarted

/-- Status reports each configured server as the pool held it before the call. -/
def statusReport (action : Option LspAction) (runningBefore : Bool) : Option ServerReport :=
  if action = some .status then some (serverReport runningBefore) else none

theorem status_does_not_start_a_server (file : FileArgument) :
    startsServer .toolCall (some .status) file = false ∧
      statusReport (some .status) false = some .notStarted := by
  cases file <;> decide

theorem writethrough_does_not_start_a_client (action : Option LspAction) (file : FileArgument) :
    startsServer .postWrite action file = false := rfl

theorem invalid_inbound_file_is_rejected_before_server_start (action : LspAction)
    (cold : action.mayColdStart = true) :
    callRefusal action .foreignUri = some .unsupportedUri ∧
      startsServer .toolCall (some action) .foreignUri = false := by
  simp [callRefusal, startsServer, inboundRefusal, cold]

end Lsp
