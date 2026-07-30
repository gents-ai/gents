import Proofs.Client.Types
import Proofs.ClientShell.Projection

namespace Conformance.ContractCases

structure LiveOverlayCase where
  name                : String
  responseStatus      : String
  materialized        : Bool
  precedingToolCalls  : Nat
  turnTerminal        : Bool
  turnLabel           : String
  hasContent          : Bool
  hasReasoning        : Bool
  expectOverlay       : Bool
  deriving Repr

def liveOverlayCases : List LiveOverlayCase :=
  [ { name := "pre_first_tool"
    , responseStatus := "streaming", materialized := false
    , precedingToolCalls := 0
    , turnTerminal := false, turnLabel := "streaming"
    , hasContent := true, hasReasoning := false
    , expectOverlay := true }
  , { name := "post_tool_resumed"
    , responseStatus := "streaming", materialized := false
    , precedingToolCalls := 1
    , turnTerminal := false, turnLabel := "streaming"
    , hasContent := true, hasReasoning := false
    , expectOverlay := true }
  , { name := "interleaved_two_tools"
    , responseStatus := "streaming", materialized := false
    , precedingToolCalls := 2
    , turnTerminal := false, turnLabel := "streaming"
    , hasContent := true, hasReasoning := false
    , expectOverlay := true }
  , { name := "tool_first_no_pre_text"
    , responseStatus := "streaming", materialized := false
    , precedingToolCalls := 1
    , turnTerminal := false, turnLabel := "streaming"
    , hasContent := false, hasReasoning := false
    , expectOverlay := false }
  , { name := "interrupted_mid_stream"
    , responseStatus := "streaming", materialized := false
    , precedingToolCalls := 0
    , turnTerminal := true, turnLabel := "interrupted"
    , hasContent := true, hasReasoning := false
    , expectOverlay := false }
  , { name := "error_mid_stream"
    , responseStatus := "error", materialized := false
    , precedingToolCalls := 0
    , turnTerminal := false, turnLabel := "streaming"
    , hasContent := false, hasReasoning := false
    , expectOverlay := false }
  , { name := "materialized_final"
    , responseStatus := "complete", materialized := true
    , precedingToolCalls := 0
    , turnTerminal := true, turnLabel := "completed"
    , hasContent := false, hasReasoning := false
    , expectOverlay := false }
  ]

def liveOverlayCaseNames : List String :=
  liveOverlayCases.map LiveOverlayCase.name

end Conformance.ContractCases
