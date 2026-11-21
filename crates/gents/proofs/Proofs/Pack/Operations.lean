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

/-- The process planning an install. The `installer` is the process asked to
install, planning against its own environment: a terminal or desktop install,
or the runtime serving an Agent's tool call. A `hostedRuntime` plans a
User's install that a client sent it, and its plan must equal the one the
client made before sending. -/
inductive Planner | installer | hostedRuntime
  deriving DecidableEq, Repr

/-- The environment a plan's `.environment` source reads: the planner's own
process environment, or the installer's values sent with the request. -/
inductive PlanEnvironment | own | sent
  deriving DecidableEq, Repr

/-- A hosted install is planned twice, by the client and by the runtime, and
the runtime commits only a graph equal to the client's plan. So the runtime
reads the installer's environment as sent and never its own.

External premise: a runtime's service environment (launchd, systemd, the
desktop's spawn) differs from the installing shell's; equal plans cannot
follow from two process environments. -/
def planEnvironment : Planner → PlanEnvironment
  | .installer => .own
  | .hostedRuntime => .sent

/-- The value a plan takes for one install variable of one pack: `packValue` and
`installValue` are the values supplied for that pack and for the whole install,
`own` the planner's process environment and `sent` the installer's
environment as the request carried it. -/
def planValue (actor : Actor) (planner : Planner)
    (packValue installValue own sent : Option String) : Option String :=
  let environment := match planEnvironment planner with
    | .own => own
    | .sent => sent
  match variableSource actor packValue.isSome installValue.isSome environment.isSome with
  | .scoped => packValue
  | .shared => installValue
  | .environment => environment
  | .unset => none

theorem hosted_runtime_never_reads_its_own_environment :
    planEnvironment .hostedRuntime ≠ .own := by
  simp [planEnvironment]

/-- Whatever the runtime's own environment holds, its plan takes the value the
installer's plan takes from the environment the installer sent. -/
theorem hosted_plan_takes_the_installers_value
    (actor : Actor) (packValue installValue installer runtime : Option String) :
    planValue actor .hostedRuntime packValue installValue runtime installer =
      planValue actor .installer packValue installValue installer runtime := rfl

theorem hosted_plan_ignores_the_runtime_environment
    (actor : Actor) (packValue installValue sent first second : Option String) :
    planValue actor .hostedRuntime packValue installValue first sent =
      planValue actor .hostedRuntime packValue installValue second sent := rfl

theorem agent_plan_reads_no_environment (planner : Planner) (own sent : Option String) :
    planValue .agent planner none none own sent = none := by
  cases planner <;> cases own <;> cases sent <;> rfl

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

/-- For a pack Tools document that leaves its filesystem root unset, a live
host configuration owns its existing root, including an explicitly unbound
root. Only a newly configured host inherits the installer's root. Reinstall
and overwrite select content; neither authorizes widening this default. The
install's existing snapshot check refuses concurrent edits before commit. -/
inductive HostRootSource | retained | installer | unbound
  deriving DecidableEq, Repr

def hostRootSource (installedHost installerRoot : Bool) : HostRootSource :=
  if installedHost then .retained else if installerRoot then .installer else .unbound

/-- Repeating an approved install may finish a prior failed root-reader grant
only when its provenance was installer-supplied and its live root still equals
this installer's root. A retained different root never becomes an automatic
reader grant. Explicitly authored roots do not enter this defaulting owner. -/
def hostRootSupplied (source : HostRootSource) (previouslySupplied matchesInstaller : Bool) : Bool :=
  matchesInstaller && (source == .installer || (source == .retained && previouslySupplied))

theorem existing_host_root_is_retained (installerRoot : Bool) :
    hostRootSource true installerRoot = .retained := rfl

theorem retained_different_root_is_not_supplied (previouslySupplied : Bool) :
    hostRootSupplied .retained previouslySupplied false = false := rfl

theorem an_unbound_root_is_not_supplied (previouslySupplied matchesInstaller : Bool) :
    hostRootSupplied .unbound previouslySupplied matchesInstaller = false := by
  cases matchesInstaller <;> simp [hostRootSupplied]

end Pack.Operations
