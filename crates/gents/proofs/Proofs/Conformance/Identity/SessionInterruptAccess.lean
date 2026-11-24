import Proofs.Store.Identity.SessionInterrupt
import Proofs.Conformance.Family

namespace Conformance.SessionInterruptAccess

open Lean Identity.Household Identity.SessionInterrupt

structure Input where
  /-- Who created the session and, in its transaction, its marker. -/
  creator : String
  /-- `agent` is the session's working Agent, `user` the User who owns that
  Agent, `node-admin` an administrator of the Node, `other` an Agent with no
  relation to the session. -/
  actor : String
  deriving ToJson

structure Output where
  read : Bool
  update : Bool
  deriving ToJson

/-- Who reads and who bumps a session's marker, whichever path created the
session: the marker follows its session through `parent`, so these are the
session's readers and updaters. -/
@[conformance_family]
def marker : Family Input Output where
  inputs := ["user", "agent"].flatMap fun creator => actors.map fun actor => { creator, actor }
  model := fun i =>
    { read := allowed 8 (fixture i.creator i.creator) i.actor "session_interrupts" "marker" "read"
      update := mayInterrupt (fixture i.creator i.creator) i.actor }

end Conformance.SessionInterruptAccess
