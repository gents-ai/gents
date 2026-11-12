/-! The WorkspaceRoot operations every client calls: publishing a root,
granting and revoking an Agent's reader relation, and removing a root. One
handler serves the CLI, the desktop and the yolo start, so nothing here
depends on which client asks. DefraDB document ACP authorizes each write: the
handler writes a fresh revision on the root's Node in the same transaction,
so only an actor who may update that Node publishes, grants, revokes or
removes. Nothing here grants access. -/

namespace WorkspaceRootOperations

/-- What the store holds for the path a publication names. -/
inductive Stored where
  | absent
  | enabled
  | disabled
  deriving DecidableEq, Repr

/-- What the caller says about `enabled`. An edit is a patch: `omitted` is
the caller saying nothing. -/
inductive Toggle where
  | omitted
  | enable
  | disable
  deriving DecidableEq, Repr

/-- Whether the root is enabled after a publication. A new root starts
enabled; an existing root keeps its state unless the caller names one. The
CLI's `workspace-root set` used to write `enabled` from a flag that defaults
to true, so publishing a root again to rename it re-enabled a root an operator
had disabled. The desktop's folder choice names `enable`: choosing a folder
for an Agent is the person asking for it to be usable. -/
def enabledAfter (stored : Stored) (toggle : Toggle) : Bool :=
  match toggle, stored with
  | .enable, _ => true
  | .disable, _ => false
  | .omitted, .disabled => false
  | .omitted, _ => true

theorem omitted_keeps_an_existing_root :
    enabledAfter .enabled .omitted = true ∧ enabledAfter .disabled .omitted = false := by
  decide

theorem a_new_root_starts_enabled : enabledAfter .absent .omitted = true := by decide

theorem a_named_state_is_written (stored : Stored) :
    enabledAfter stored .enable = true ∧ enabledAfter stored .disable = false := by
  cases stored <;> decide

/-- What a publication does to the root's `display_name`, per the three
states of a patched optional text field. -/
inductive NameUpdate where
  | omitted
  | clear
  | set
  deriving DecidableEq, Repr

/-- Whether a publication writes `display_name` at all. An omitted name on an
existing root is preserved; a new root has nothing to preserve, so its name is
written (as absent when omitted). -/
def writesDisplayName (stored : Stored) (name : NameUpdate) : Bool :=
  match name, stored with
  | .omitted, .absent => true
  | .omitted, _ => false
  | _, _ => true

theorem omitted_name_is_preserved :
    writesDisplayName .enabled .omitted = false ∧
    writesDisplayName .disabled .omitted = false := by decide

/-- The actor a reader change names, as the handler's transaction sees it. -/
inductive ReaderSubject where
  /-- An enabled Agent of the root's Node. -/
  | enabledAgent
  | disabledAgent
  /-- No Agent document of the root's Node carries this DID. -/
  | unknownActor
  /-- The Node's own DID. -/
  | nodeActor
  deriving DecidableEq, Repr

inductive ReaderChange where
  | grant
  | revoke
  deriving DecidableEq, Repr

inductive ReaderDecision where
  | admitted
  /-- A grant names an Agent that is missing or disabled. -/
  | agentUnavailable
  /-- The Node actor is never a root reader: the runtime reads through its
  own Node relation and executes as the selected Agent. -/
  | nodeActor
  deriving DecidableEq, Repr

/-- A grant selects an enabled Agent of the root's Node. A revocation needs no
Agent document, so a reader whose Agent was deleted can still be removed. -/
def readerDecision (change : ReaderChange) (subject : ReaderSubject) : ReaderDecision :=
  match subject, change with
  | .nodeActor, _ => .nodeActor
  | .enabledAgent, _ => .admitted
  | _, .revoke => .admitted
  | _, .grant => .agentUnavailable

theorem revocation_survives_a_deleted_agent :
    readerDecision .revoke .unknownActor = .admitted ∧
    readerDecision .revoke .disabledAgent = .admitted := by decide

theorem a_grant_selects_an_enabled_agent (subject : ReaderSubject) :
    readerDecision .grant subject = .admitted ↔ subject = .enabledAgent := by
  cases subject <;> decide

theorem the_node_actor_is_never_a_reader (change : ReaderChange) :
    readerDecision change .nodeActor = .nodeActor := by
  cases change <;> rfl

/-- Why the handler refused. -/
inductive Refusal where
  /-- The path is not absolute. -/
  | pathNotAbsolute
  /-- No visible root has this path. -/
  | rootMissing
  /-- The root at this path belongs to another Node. -/
  | foreignRoot
  /-- The selected Node is missing or not visible to the actor. -/
  | nodeUnavailable
  | agentUnavailable
  | nodeActor
  deriving DecidableEq, Repr

/-- The call a refusal names. -/
inductive Recovery where
  /-- The verb's own help: the input was wrong. -/
  | verbHelp
  /-- List the published roots. -/
  | listRoots
  /-- List the Agents the caller may name. -/
  | listAgents
  /-- Read the caller's Node, which shows what it may administer. -/
  | inspectNode
  deriving DecidableEq, Repr

def Refusal.recovery : Refusal → Recovery
  | .pathNotAbsolute => .verbHelp
  | .rootMissing => .listRoots
  | .foreignRoot => .listRoots
  | .nodeUnavailable => .inspectNode
  | .agentUnavailable => .listAgents
  | .nodeActor => .listAgents

def ReaderDecision.refusal : ReaderDecision → Option Refusal
  | .admitted => none
  | .agentUnavailable => some .agentUnavailable
  | .nodeActor => some .nodeActor

/-- A refused reader change sends the caller to the Agents it may name. -/
theorem reader_refusals_list_agents (change : ReaderChange) (subject : ReaderSubject)
    (refusal : Refusal)
    (h : (readerDecision change subject).refusal = some refusal) :
    refusal.recovery = .listAgents := by
  revert h
  cases change <;> cases subject <;> cases refusal <;> decide

end WorkspaceRootOperations
