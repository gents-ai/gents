import Proofs.Client.Types
import Proofs.ClientShell.Projection

/-!
# Live Overlay Conformance Cases

Generated cases asserting the live-overlay render decision under the
seven streaming patterns enumerated in the issue #64 design doc.
-/

namespace Conformance.ContractCases

structure LiveOverlayCase where
  name                : String
  responseStatus      : String   -- "streaming" | "complete" | "error"
  materialized        : Bool
  precedingToolCalls  : Nat
  turnTerminal        : Bool
  turnLabel           : String   -- "waitingForClaim" | "streaming" | "completed"
                                 -- | "failed" | "superseded" | "interrupted"
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
  -- P2P lag scenario: client sees error response before request lifecycle advances to `failed`.
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
