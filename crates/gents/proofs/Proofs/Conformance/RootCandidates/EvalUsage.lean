import Proofs.Runtime.EvalUsage
import Proofs.Conformance.Family

namespace Runtime.EvalUsage
open Conformance

private def request (id : Nat) (purpose session : String) (claimed terminal : Option Nat) :
    Request :=
  ⟨id, purpose, session, claimed, terminal⟩

private def turn (request seq : Nat) (counts : Option (Nat × Nat))
    (started ended firstToken : Option Nat) : Call :=
  ⟨some request, "inference", seq, counts.map (·.1), counts.map (·.2), counts.map (·.1 / 2),
    started, ended, firstToken⟩

private def tool (request : Nat) (name state : String) (pluginParent : Bool)
    (started completed : Option Nat) : Tool :=
  ⟨some request, name, state, pluginParent, false, started, completed⟩

private def stage : Request := request 1 "normal" "trial" (some 0) (some 9000)

private def audited (callState : Option (Nat × Nat)) (ended firstToken : Option Nat) : Home :=
  { session := "trial", stages := [1]
    requests := [stage, request 2 "title-audit" "trial" (some 100) (some 9500)]
    calls := [turn 1 1 (some (100, 10)) (some 100) (some 1000) (some 350),
      turn 1 2 (some (200, 20)) (some 1100) (some 2000) (some 1180),
      turn 2 1 callState (some 200) ended firstToken]
    tools := [] }

private def delegated : Home :=
  { session := "trial", stages := [1, 5]
    requests := [stage, request 3 "normal" "delegated" (some 3000) (some 4000),
      request 4 "normal" "trial" (some 8000) (some 9500),
      request 5 "normal" "trial" (some 9200) (some 9900)]
    calls := [turn 1 1 (some (900, 40)) (some 100) (some 1600) (some 400),
      turn 3 1 (some (50000, 5000)) (some 3000) (some 4000) (some 3010),
      turn 4 1 (some (1600, 20)) (some 8100) (some 8900) (some 8700),
      turn 5 1 (some (1500, 80)) (some 9300) (some 9800) (some 9500)]
    tools := [tool 1 "agent_new" "completed" false (some 1600) (some 2900),
      tool 3 "write_file" "failed" false (some 3100) (some 3200)] }

private def scripted : Home :=
  { session := "trial", stages := [1]
    requests := [stage]
    calls := [turn 1 1 (some (900, 40)) (some 100) (some 1600) (some 1300),
      turn 1 2 (some (1200, 60)) (some 2000) (some 3000) none]
    tools := [tool 1 "read_file" "completed" false (some 1600) (some 1800),
      tool 1 "script" "completed" false (some 1700) (some 2000),
      tool 1 "read_file" "completed" true (some 1750) (some 1800),
      tool 1 "grep" "failed" true (some 1800) (some 1900),
      tool 1 "write_file" "timedOut" false (some 7100) (some 7600),
      { tool 1 "bash" "failed" false (some 7200) (some 7300) with spawned := true },
      tool 1 "spawn_process" "completed" false (some 7150) (some 7200),
      tool 1 "read_file" "cancelled" false none none] }

private def unreported : Home :=
  { session := "trial", stages := [1]
    requests := [stage]
    calls := [turn 1 1 none (some 100) (some 1600) none,
      turn 1 2 (some (1200, 60)) (some 2000) (some 3000) (some 2450),
      { turn 1 3 (some (1300, 70)) (some 3100) (some 3900) (some 3150) with output := none },
      { turn 1 4 (some (4000, 300)) (some 4000) (some 4800) (some 4100) with
          kind := "compaction" },
      { turn 1 5 (some (500, 5)) (some 5000) (some 5100) (some 5070) with cached := none }]
    tools := [] }

private def unfinished : Home :=
  { session := "trial", stages := [1]
    requests := [stage, request 4 "normal" "trial" (some 9100) none]
    calls := [turn 1 1 (some (900, 40)) (some 100) (some 1600) (some 160),
      turn 4 1 none (some 9200) none (some 9900),
      turn 4 2 none none none (some 9950)]
    tools := [tool 1 "read_file" "running" false (some 1600) none] }

private def elsewhere : Home :=
  { session := "trial", stages := [6]
    requests := [request 6 "normal" "fired" (some 500) (some 2500),
      request 7 "access-review" "trial" (some 600) (some 700),
      request 8 "normal" "automation" (some 800) (some 900),
      request 9 "normal" "trial" none none]
    calls := [turn 6 3 (some (700, 30)) (some 600) (some 1400) (some 640),
      turn 6 2 (some (650, 25)) (some 520) (some 590) (some 510),
      turn 7 1 (some (80, 8)) (some 610) (some 690) (some 650),
      turn 8 1 (some (90, 9)) (some 810) (some 890) (some 830),
      { turn 6 4 (some (10, 1)) none none (some 900) with request := none }]
    tools := [tool 8 "write_file" "completed" false (some 820) (some 830)] }

private def idle : Home :=
  { session := "trial", stages := [1], requests := [stage], calls := [], tools := [] }

private def nothing : Home :=
  { session := "trial", stages := [], requests := [], calls := [], tools := [] }

private def reversed (home : Home) : Home :=
  { home with
    requests := home.requests.reverse
    calls := home.calls.reverse
    tools := home.tools.reverse }

@[conformance_family]
def charges : Family Home Usage :=
  ⟨([audited none none none, audited (some (88, 200)) (some 300) (some 240), delegated,
      scripted, unreported, unfinished, elsewhere, idle, nothing].flatMap fun home =>
      [home, reversed home]),
    trialUsage⟩

end Runtime.EvalUsage
