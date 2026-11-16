import Proofs.Tools.Lsp.ServerStart
import Proofs.Conformance.Family
import Proofs.Conformance.Functions.Tools

namespace Conformance.LspServerStart

open Lean Lsp

deriving instance ToJson for Entry, FileArgument, Refusal, ServerReport

structure Situation where
  entry : Entry
  action : Option LspAction
  file : FileArgument
  deriving ToJson

structure Observation where
  refusal : Option Refusal
  startsServer : Bool
  report : Option ServerReport
  deriving ToJson

/-- Each situation runs in a workspace whose primary server is configured and
detected and whose pool holds no client before the entry runs. -/
@[conformance_family]
def entries : Family Situation Observation where
  inputs :=
    [ { entry := .toolCall, action := some .status, file := .absent },
      { entry := .toolCall, action := some .status, file := .foreignUri },
      { entry := .toolCall, action := some .reload, file := .absent },
      { entry := .toolCall, action := some .hover, file := .foreignUri },
      { entry := .toolCall, action := some .definition, file := .foreignUri },
      { entry := .toolCall, action := some .hover, file := .workspacePath },
      { entry := .postWrite, action := none, file := .workspacePath } ]
  model := fun i =>
    { refusal := match i.entry, i.action with
        | .toolCall, some action => callRefusal action i.file
        | _, _ => none
      startsServer := startsServer i.entry i.action i.file
      report := statusReport i.action false }

end Conformance.LspServerStart
