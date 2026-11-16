import Proofs.Runtime.ClaudeSessionLineage
import Proofs.Conformance.Family

namespace Runtime.ClaudeSessionLineage
open Lean Conformance

private def link (session : Nat) : Option Link := some ⟨session, session + 100⟩

private def chain (length : Nat) (root : Option Cause) : Input :=
  ⟨0, (List.range length).map (fun n => ⟨n, .agentNew, link (n+1)⟩) ++
    (root.toList.map fun cause => ⟨length, cause, none⟩)⟩

@[conformance_family]
def sessionOrigin : Family Input Observation :=
  ⟨[⟨0, []⟩, ⟨0, [⟨0, .agentNew, none⟩]⟩,
     ⟨0, [⟨0, .taskTrigger, link 1⟩]⟩,
     ⟨0, [⟨0, .pluginJob, link 1⟩, ⟨1, .agentNew, link 2⟩]⟩,
     ⟨0, [⟨0, .agentNew, link 0⟩]⟩,
     ⟨0, [⟨0, .pluginJob, link 1⟩, ⟨1, .pluginJob, link 0⟩]⟩] ++
     ([1, 2, 3, 31, 32, 33, 34].flatMap fun length =>
       [chain length none, chain length (some .taskTrigger),
        chain length (some .agentNew), chain length (some .pluginJob)]), walk⟩

end Runtime.ClaudeSessionLineage
