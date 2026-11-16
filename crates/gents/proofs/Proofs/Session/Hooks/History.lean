import Proofs.Session.Hooks.MustCommit

namespace Hooks

/-- A declared read of revision history: every native revision of the
documents of `collection` its filter selects, deleted ones included; `doc` is
the document the filter pins by id, if it pins one. -/
structure HistoryRead (Doc : Type) where
  collection : String
  doc : Option Doc
  deriving DecidableEq, Repr

variable {Core Q Ev V Doc Val Op : Type} [DecidableEq Doc]

/-- The bound on a declared history read.

Decision 2026-11-11, maintainer-approved: a plugin may read the revision
history of documents only in a collection it owns, or of a document the event
names, and only of a collection its manifest declares history for
(`history`, beside the grant declaration). History shows every value a
document ever held, deleted documents included, which the current-row reads
do not; the bound keeps a plugin from reading the past of core's or another
plugin's rows. -/
def historyPermitted (manifest : Manifest) (history : List String) (named : List (Ref Doc))
    (read : HistoryRead Doc) : Bool :=
  history.contains read.collection &&
    (manifest.owns.contains read.collection ||
      read.doc.any fun doc => named.contains ⟨read.collection, doc⟩)

/-- Every history read among a plugin's declared reads is within the bound.
`historyOf` recognises a history read among the host's queries. -/
def readsBounded (historyOf : Q → Option (HistoryRead Doc)) (manifest : Manifest)
    (history : List String) (named : List (Ref Doc)) (reads : List Q) : Bool :=
  reads.all fun query => (historyOf query).all (historyPermitted manifest history named)

/-- The plugins at a point whose declared reads at `event` leave the bound;
`history` is each plugin's declaration, by name. -/
def unbounded (historyOf : Q → Option (HistoryRead Doc)) (history : String → List String)
    (named : List (Ref Doc)) (event : Ev) (plugins : List (Plugin Q Ev V Doc Val Op)) :
    List (Plugin Q Ev V Doc Val Op) :=
  plugins.filter fun plugin =>
    !readsBounded historyOf plugin.manifest (history plugin.manifest.name) named
      (plugin.hook.reads event)

/-- A point's runner (`stage`, `answer`) behind the bound. The host checks
every declared read before it answers any: a history read outside the bound
is not answered, the plugin is shown nothing and decides nothing, and the
point fails as an invalid result of that plugin. This is how the native host
treats any read it cannot answer: at a point that may refuse the transaction
does not commit; at a point that must commit the plugin is dropped and owed
(`out_of_bound_history_is_owed`). -/
def bounded {α : Type} (historyOf : Q → Option (HistoryRead Doc))
    (history : String → List String) (named : List (Ref Doc)) (event : Ev)
    (plugins : List (Plugin Q Ev V Doc Val Op)) (run : Except Refusal α) : Except Refusal α :=
  match unbounded historyOf history named event plugins with
  | [] => run
  | plugin :: _ => .error (.invalid plugin.manifest.name)

/-- Within the bound the runner is unchanged, so every staging theorem
holds. -/
theorem bounded_reads_run_unchanged {α : Type} (historyOf : Q → Option (HistoryRead Doc))
    (history : String → List String) (named : List (Ref Doc)) (event : Ev)
    (plugins : List (Plugin Q Ev V Doc Val Op)) (run : Except Refusal α)
    (within : ∀ plugin ∈ plugins, readsBounded historyOf plugin.manifest
      (history plugin.manifest.name) named (plugin.hook.reads event) = true) :
    bounded historyOf history named event plugins run = run := by
  have none : unbounded historyOf history named event plugins = [] := by
    simp only [unbounded, List.filter_eq_nil_iff, Bool.not_eq_true']
    intro plugin member
    simpa using within plugin member
  simp [bounded, none]

private theorem unbounded_nonempty (historyOf : Q → Option (HistoryRead Doc))
    (history : String → List String) (named : List (Ref Doc)) (event : Ev)
    (plugins : List (Plugin Q Ev V Doc Val Op)) (plugin : Plugin Q Ev V Doc Val Op)
    (member : plugin ∈ plugins) (query : Q) (declared : query ∈ plugin.hook.reads event)
    (read : HistoryRead Doc) (recognised : historyOf query = some read)
    (outside : historyPermitted plugin.manifest (history plugin.manifest.name) named read = false) :
    unbounded historyOf history named event plugins ≠ [] := by
  intro empty
  have kept : plugin ∈ unbounded historyOf history named event plugins := by
    simp only [unbounded, List.mem_filter, member, true_and, Bool.not_eq_true', readsBounded,
      List.all_eq_false]
    exact ⟨query, declared, by simp [recognised, outside]⟩
  rw [empty] at kept
  cases kept

/-- A history read outside the bound is not answered: the point fails before
any plugin decides, and nothing commits. -/
theorem out_of_bound_history_is_not_answered (host : Host Core Q Doc Val Op)
    (historyOf : Q → Option (HistoryRead Doc)) (history : String → List String)
    (named : List (Ref Doc)) (plugins : List (Plugin Q Ev V Doc Val Op)) (actor : Actor)
    (event : Ev) (snapshot : State Core Doc Val) (plugin : Plugin Q Ev V Doc Val Op)
    (member : plugin ∈ plugins) (query : Q) (declared : query ∈ plugin.hook.reads event)
    (read : HistoryRead Doc) (recognised : historyOf query = some read)
    (outside : historyPermitted plugin.manifest (history plugin.manifest.name) named read = false) :
    (∃ offender, bounded historyOf history named event plugins
        (stage host plugins actor event snapshot) = .error (.invalid offender)) ∧
      settle snapshot (bounded historyOf history named event plugins
        (stage host plugins actor event snapshot)) = snapshot := by
  have nonempty := unbounded_nonempty historyOf history named event plugins plugin member query
    declared read recognised outside
  cases offenders : unbounded historyOf history named event plugins with
  | nil => exact absurd offenders nonempty
  | cons first rest =>
    simp [bounded, offenders, settle]

/-- At a point that must commit, a plugin whose declared reads leave the bound
is read as failed: it is dropped and owed, and the transition commits. -/
theorem out_of_bound_history_is_owed (host : Host Core Q Doc Val Op)
    (historyOf : Q → Option (HistoryRead Doc)) (history : String → List String)
    (named : List (Ref Doc)) (actor : Actor) (event : Ev) (snapshot : State Core Doc Val)
    (plugins : List (Plugin Q Ev V Doc Val Op)) (staged : State Core Doc Val)
    (plugin : Plugin Q Ev V Doc Val Op) (member : plugin ∈ plugins) (query : Q)
    (declared : query ∈ plugin.hook.reads event) (read : HistoryRead Doc)
    (recognised : historyOf query = some read)
    (outside : historyPermitted plugin.manifest (history plugin.manifest.name) named read = false) :
    plugin.manifest.name ∈ owed plugins (stageDropping host actor event snapshot
      (fun name => (unbounded historyOf history named event plugins).any
        (·.manifest.name == name)) plugins staged).2 := by
  apply failed_staging_is_owed
  · exact member
  · left
    simp only [List.any_eq_true, beq_iff_eq]
    refine ⟨plugin, ?_, rfl⟩
    simp only [unbounded, List.mem_filter, member, true_and, Bool.not_eq_true', readsBounded,
      List.all_eq_false]
    exact ⟨query, declared, by simp [recognised, outside]⟩

/-- The two ways a history read is within the bound. -/
theorem owned_declared_history_is_permitted (manifest : Manifest) (history : List String)
    (named : List (Ref Doc)) (read : HistoryRead Doc)
    (owned : read.collection ∈ manifest.owns) (declared : read.collection ∈ history) :
    historyPermitted manifest history named read = true := by
  simp [historyPermitted, owned, declared]

theorem named_declared_history_is_permitted (manifest : Manifest) (history : List String)
    (named : List (Ref Doc)) (collection : String) (doc : Doc)
    (event : (⟨collection, doc⟩ : Ref Doc) ∈ named) (declared : collection ∈ history) :
    historyPermitted manifest history named ⟨collection, some doc⟩ = true := by
  simp [historyPermitted, event, declared]

/-- An undeclared history read is outside the bound, whatever the plugin
owns. -/
theorem undeclared_history_is_refused (manifest : Manifest) (history : List String)
    (named : List (Ref Doc)) (read : HistoryRead Doc) (undeclared : read.collection ∉ history) :
    historyPermitted manifest history named read = false := by
  simp [historyPermitted, undeclared]

end Hooks
