/-! The admission of a pack install, update, removal or wiring, decided once
for every client. A client names who acts; what differs between a person and
an Agent is a rule about that actor here, not a difference between clients. -/

namespace Pack.Operations

/-- Who signs the operation. A User acts at a terminal or in the desktop and
may do what an operator may. An Agent acts through its tool call. -/
inductive Actor | user | agent
  deriving DecidableEq, Repr

/-- The digest a caller expects, against the artifact the handler resolved. -/
inductive Digest | absent | matching | differing
  deriving DecidableEq, Repr

inductive DigestAdmission | admitted | previewRequired | changed
  deriving DecidableEq, Repr

/-- A destructive change by an Agent commits only the artifact a preview
pinned: the digest is the Agent's proof that what it reviewed is what is
written. A User reviews at the terminal and commits in one call; a digest the
User does name still binds. A preview needs none, and a named one still must
match. -/
def digestAdmission (actor : Actor) (commit : Bool) : Digest → DigestAdmission
  | .differing => .changed
  | .matching => .admitted
  | .absent => if commit && actor = .agent then .previewRequired else .admitted

theorem differing_digest_never_admitted (actor : Actor) (commit : Bool) :
    digestAdmission actor commit .differing = .changed := rfl

theorem agent_commit_names_the_previewed_artifact (digest : Digest)
    (h : digestAdmission .agent true digest = .admitted) : digest = .matching := by
  cases digest <;> simp_all [digestAdmission]

theorem user_commits_in_one_call :
    digestAdmission .user true .absent = .admitted := rfl

theorem preview_needs_no_digest (actor : Actor) :
    digestAdmission actor false .absent = .admitted := by
  cases actor <;> rfl

/-- What a change does with documents edited since the pack was installed. -/
inductive Edited | refuse | overwrite | keep
  deriving DecidableEq, Repr

inductive EditedAdmission | admitted | decisionRequired
  deriving DecidableEq, Repr

/-- Edited documents are never lost or kept silently: a commit that meets them
carries the caller's decision. A preview reports them and writes nothing. -/
def editedAdmission (commit editedPresent : Bool) (decision : Edited) : EditedAdmission :=
  if commit && editedPresent && decision = .refuse then .decisionRequired else .admitted

theorem undecided_edits_refuse_the_commit :
    editedAdmission true true .refuse = .decisionRequired := rfl

theorem a_decision_admits (commit editedPresent : Bool) (decision : Edited)
    (h : decision ≠ .refuse) : editedAdmission commit editedPresent decision = .admitted := by
  cases decision <;> simp_all [editedAdmission]

theorem nothing_edited_needs_no_decision (commit : Bool) (decision : Edited) :
    editedAdmission commit false decision = .admitted := by
  simp [editedAdmission]

/-- Where one pack of an install takes the value of an install variable. -/
inductive VariableSource | scoped | shared | environment | unset
  deriving DecidableEq, Repr

/-- A value supplied for one pack (`NAMESPACE/PACK:NAME=VALUE`) wins over one
supplied for the whole install (`NAME=VALUE`). Only a User's install falls back
to the process environment: an Agent names every value it supplies and never
reads the runtime's environment, which holds the operator's credentials. -/
def variableSource (actor : Actor) (forPack forInstall inEnvironment : Bool) : VariableSource :=
  if forPack then .scoped
  else if forInstall then .shared
  else if actor = .user && inEnvironment then .environment
  else .unset

theorem agent_never_reads_the_environment (forPack forInstall inEnvironment : Bool) :
    variableSource .agent forPack forInstall inEnvironment ≠ .environment := by
  cases forPack <;> cases forInstall <;> cases inEnvironment <;> simp [variableSource]

theorem supplied_value_wins_over_environment (actor : Actor) (forPack forInstall : Bool)
    (h : forPack = true ∨ forInstall = true) :
    variableSource actor forPack forInstall true ≠ .environment := by
  cases forPack <;> cases forInstall <;> simp_all [variableSource]

/-- The published root an Agent the pack ships needs reader on. -/
inductive RootReader | notNeeded | published | unpublished
  deriving DecidableEq, Repr

inductive ReaderAction | none | grant | reportGrantStep | reportPublishStep
  deriving DecidableEq, Repr

/-- WorkspaceRoot reader is granted only by the Node's owner and
administrators. A User's install grants it to each Agent the pack ships; an
Agent's install reports the step for a User and grants nothing. A root nobody
published is reported to either. -/
def readerAction (actor : Actor) : RootReader → ReaderAction
  | .notNeeded => .none
  | .unpublished => .reportPublishStep
  | .published => match actor with
    | .user => .grant
    | .agent => .reportGrantStep

theorem agent_install_grants_no_reader (reader : RootReader) :
    readerAction .agent reader ≠ .grant := by
  cases reader <;> simp [readerAction]

end Pack.Operations
