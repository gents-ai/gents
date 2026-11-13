import Proofs.Session.CanonicalOutput.Message
import Proofs.Session.ToolExecution.State

/-!
# Interrupted tool diagnostic budget

An interrupted call's terminal diagnostic shows the tail of its raw output
within the output budget its tool has for the request's Agent, then the cause.
The budget is read when the call ends; the stored presentation is replayed
unchanged afterwards, whatever the configuration says by then.
-/

namespace ToolExecution.InterruptedDiagnostic

open CanonicalOutput

def defaultOutputBudget : Nat := 16000

def bashToolNames : List String := ["bash", "bash_unrestricted"]

/-- A bash call reads the Agent's `host.bash.max_output_chars`; any other tool
reads its own command limit, which these Tools do not configure; Tools that do
not resolve, or leave the limit unset, give the default. -/
def outputBudget (toolName : String) (configuredBash : Option Nat) : Nat :=
  if toolName ∈ bashToolNames then configuredBash.getD defaultOutputBudget
  else defaultOutputBudget

def tail (raw : List UInt8) (budget : Nat) : List UInt8 :=
  raw.drop (terminalTailStart raw budget)

/-- A deadline ends only a call that has not ended; a replayed deadline over
an ended call loses and changes nothing. -/
def deadlineWins (s : ToolCallState) : Bool :=
  s == .pending || s == .running

def afterDeadline (s : ToolCallState) : ToolCallState :=
  if deadlineWins s then .timedOut else s

private def raw20 : List UInt8 := "0123456789abcdefghij".toUTF8.toList

theorem interrupted_tool_diagnostic_follows_the_configured_output_budget :
    tail raw20 (outputBudget "bash" (some 5)) = "fghij".toUTF8.toList ∧
      tail raw20 (outputBudget "mcp__search__query" (some 5)) = raw20 ∧
      tail raw20 (outputBudget "bash" none) = raw20 ∧
      deadlineWins (afterDeadline .running) = false := by native_decide

end ToolExecution.InterruptedDiagnostic
