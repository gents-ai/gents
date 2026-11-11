use std::sync::{Arc, Mutex};

use gents_lean_vocab::{
    lean_lifecycle_hook_admit_cases, lean_lifecycle_hook_combine_cases,
    lean_lifecycle_hook_delivery_cases, lean_lifecycle_hook_effect_cases,
    lean_lifecycle_hook_gate_cases, lean_lifecycle_hook_grant_cases,
    lean_lifecycle_hook_must_commit_cases, lean_lifecycle_hook_queue_scope_cases,
    lean_lifecycle_hook_stage_cases, LeanLifecycleHookAccess, LeanLifecycleHookAdmitResult,
    LeanLifecycleHookBinding, LeanLifecycleHookChoice, LeanLifecycleHookDecision,
    LeanLifecycleHookDeliveryRecord, LeanLifecycleHookDeliveryStep, LeanLifecycleHookEffect,
    LeanLifecycleHookGateResult, LeanLifecycleHookManifest, LeanLifecycleHookPartition,
    LeanLifecycleHookPreparedVerdict, LeanLifecycleHookQueueScopeResult, LeanLifecycleHookRef,
    LeanLifecycleHookRefusal, LeanLifecycleHookRow, LeanLifecycleHookScopeAnswer,
    LeanLifecycleHookScopeKey, LeanLifecycleHookScript, LeanLifecycleHookStageResult,
    LeanLifecycleHookStale, LeanLifecycleHookState, LeanLifecycleHookSubject,
    LeanLifecycleHookSubjectKind, LeanLifecycleHookVerdict, LeanLifecycleHookWorld,
};
use gents_session::request::hooks::claim::{combine, ClaimVerdict};
use gents_session::request::hooks::delivery::DeliveryRecord;
use gents_session::request::hooks::effect::{
    validate, Access, CoreOp, DocRef, Effect, Grant, Grantable, Manifest, Named, OwnedCollection,
    Row, Subject, SubjectKind, Write,
};
use gents_session::request::hooks::hook::{Hook, Outcome, Query};
use gents_session::request::hooks::prepared::{admit, prepare, Binding, Stale, Verdict};
use gents_session::request::hooks::queue_scope::{
    partition, Partition, ScopeAnswer, ScopeAnswers, ScopeFailure, ScopeRefused,
};
use gents_session::request::hooks::stage::{
    gate, owed, stage, stage_dropping, ApplyFailure, HookError, Host, Plugin, Staged,
};
use serde_json::Value;

/// The filter field naming the plugin a declared read belongs to, so the
/// store double can fail exactly the reads the model marks as failed.
const PLUGIN_FIELD: &str = "plugin";

/// `Manifest` holds `&'static str` names because the composition root
/// registers plugins at build time; generated cases name them at run time.
fn leak(text: &str) -> &'static str {
    Box::leak(text.to_owned().into_boxed_str())
}

fn manifest(name: &str, owns: &[String], bound: usize) -> Manifest {
    Manifest {
        name: leak(name),
        owns: owns
            .iter()
            .map(|collection| OwnedCollection {
                name: leak(collection),
                schema: None,
                grants: &[],
            })
            .collect(),
        bound,
    }
}

fn manifest_of(lean: &LeanLifecycleHookManifest) -> Manifest {
    manifest(&lean.name, &lean.owns, lean.bound)
}

fn access(lean: LeanLifecycleHookAccess) -> Access {
    match lean {
        LeanLifecycleHookAccess::Create => Access::Create,
        LeanLifecycleHookAccess::Update => Access::Update,
        LeanLifecycleHookAccess::Delete => Access::Delete,
    }
}

fn effect(lean: &LeanLifecycleHookEffect) -> Effect {
    match lean {
        LeanLifecycleHookEffect::Create {
            collection,
            doc,
            value,
        } => Effect::Write(Write::Create {
            doc: DocRef::new(collection, doc),
            value: Value::String(value.clone()),
            grants: Vec::new(),
        }),
        LeanLifecycleHookEffect::Update {
            collection,
            doc,
            expected,
            value,
        } => Effect::Write(Write::Update {
            doc: DocRef::new(collection, doc),
            expected: *expected,
            value: Value::String(value.clone()),
        }),
        LeanLifecycleHookEffect::Delete {
            collection,
            doc,
            expected,
        } => Effect::Write(Write::Delete {
            doc: DocRef::new(collection, doc),
            expected: *expected,
        }),
        LeanLifecycleHookEffect::Core { op } => Effect::Core(CoreOp::new(op, Value::Null)),
    }
}

fn effects(lean: &[LeanLifecycleHookEffect]) -> Vec<Effect> {
    lean.iter().map(effect).collect()
}

fn row(lean: &LeanLifecycleHookRow) -> Row {
    Row {
        doc: DocRef::new(&lean.collection, &lean.doc),
        revision: lean.revision,
        value: Value::String(lean.value.clone()),
    }
}

fn verdict(lean: &LeanLifecycleHookVerdict) -> ClaimVerdict {
    match lean {
        LeanLifecycleHookVerdict::Allow => ClaimVerdict::Allow,
        LeanLifecycleHookVerdict::LeaveQueued => ClaimVerdict::LeaveQueued,
        LeanLifecycleHookVerdict::Retire { reason } => ClaimVerdict::Retire {
            reason: reason.clone(),
            successor: None,
        },
        LeanLifecycleHookVerdict::Reject { reason } => ClaimVerdict::Reject {
            reason: reason.clone(),
        },
    }
}

/// Lean `Hooks.State` as the store double holds it.
#[derive(Debug, Clone, PartialEq)]
struct Snapshot {
    core: Vec<String>,
    rows: Vec<Row>,
}

fn snapshot_of(lean: &LeanLifecycleHookState) -> Snapshot {
    Snapshot {
        core: lean.core.clone(),
        rows: lean.rows.iter().map(row).collect(),
    }
}

/// An in-memory lifecycle transaction. Row order follows Lean `Store`: a
/// create prepends, an update rewrites in place and a delete filters.
struct StoreDouble {
    state: Mutex<Snapshot>,
    denied: Vec<(Access, DocRef)>,
    failing_ops: Vec<String>,
    unreadable: Vec<String>,
    failed_operation: Mutex<Option<&'static str>>,
}

impl StoreDouble {
    fn new(world: &LeanLifecycleHookWorld, core: Vec<String>, unreadable: &[String]) -> Self {
        Self {
            state: Mutex::new(Snapshot {
                core,
                rows: world.rows.iter().map(row).collect(),
            }),
            denied: world
                .denied
                .iter()
                .map(|denial| {
                    (
                        access(denial.access),
                        DocRef::new(&denial.collection, &denial.doc),
                    )
                })
                .collect(),
            failing_ops: world.failing_ops.clone(),
            unreadable: unreadable.to_vec(),
            failed_operation: Mutex::new(None),
        }
    }

    fn of(world: &LeanLifecycleHookWorld) -> Self {
        Self::new(world, world.core.clone(), &[])
    }

    fn snapshot(&self) -> Snapshot {
        self.state.lock().unwrap().clone()
    }

    fn permits(&self, access: Access, doc: &DocRef) -> bool {
        !self
            .denied
            .iter()
            .any(|(denied_access, denied_doc)| *denied_access == access && denied_doc == doc)
    }

    /// Lean `Hooks.applyWrite`.
    fn write(rows: &mut Vec<Row>, write: &Write) -> bool {
        match write {
            Write::Create { doc, value, .. } => {
                if rows.iter().any(|row| &row.doc == doc) {
                    return false;
                }
                rows.insert(
                    0,
                    Row {
                        doc: doc.clone(),
                        revision: 0,
                        value: value.clone(),
                    },
                );
                true
            }
            Write::Update {
                doc,
                expected,
                value,
            } => {
                let mut held = false;
                for row in rows.iter_mut() {
                    if &row.doc == doc && row.revision == *expected {
                        row.revision = expected + 1;
                        row.value = value.clone();
                        held = true;
                    }
                }
                held
            }
            Write::Delete { doc, expected } => {
                if !rows
                    .iter()
                    .any(|row| &row.doc == doc && row.revision == *expected)
                {
                    return false;
                }
                rows.retain(|row| &row.doc != doc);
                true
            }
        }
    }

    /// Lean `Hooks.applyEffects` with `World.perform`: all or none.
    fn applied(&self, plugin: &'static str, effects: &[Effect]) -> Result<Snapshot, HookError> {
        let mut next = self.snapshot();
        for effect in effects {
            match effect {
                Effect::Write(write) => {
                    if !Self::write(&mut next.rows, write) {
                        return Err(HookError::Conflict { plugin });
                    }
                }
                Effect::Core(operation) => {
                    if self.failing_ops.contains(&operation.name) {
                        *self.failed_operation.lock().unwrap() = Some(plugin);
                        return Err(HookError::Operation(anyhow::anyhow!(
                            "core refused {}",
                            operation.name
                        )));
                    }
                    next.core.push(operation.name.clone());
                }
            }
        }
        Ok(next)
    }

    fn refusal(&self, error: &HookError) -> LeanLifecycleHookRefusal {
        match error {
            HookError::Refused { plugin, reason } => LeanLifecycleHookRefusal::Refused {
                plugin: (*plugin).to_owned(),
                reason: reason.clone(),
            },
            HookError::Invalid { plugin, .. } | HookError::GateEffects { plugin } => {
                LeanLifecycleHookRefusal::Invalid {
                    plugin: (*plugin).to_owned(),
                }
            }
            HookError::Conflict { plugin } => LeanLifecycleHookRefusal::Conflict {
                plugin: (*plugin).to_owned(),
            },
            HookError::Operation(_) => LeanLifecycleHookRefusal::Conflict {
                plugin: self
                    .failed_operation
                    .lock()
                    .unwrap()
                    .expect("a failed core operation records its plugin")
                    .to_owned(),
            },
            other => panic!("the model has no refusal for {other:?}"),
        }
    }
}

#[async_trait::async_trait]
impl Host for StoreDouble {
    fn actor(&self) -> Option<String> {
        None
    }

    async fn select(&self, query: &Query) -> Result<Vec<Row>, HookError> {
        let unreadable = query.filter.iter().any(|(field, value)| {
            field == PLUGIN_FIELD
                && value
                    .as_str()
                    .is_some_and(|plugin| self.unreadable.iter().any(|name| name == plugin))
        });
        if unreadable {
            return Err(HookError::Query(format!(
                "{} rows are unavailable",
                query.collection
            )));
        }
        Ok(self
            .snapshot()
            .rows
            .into_iter()
            .filter(|row| row.doc.collection == query.collection)
            .collect())
    }

    async fn allowed(&self, access: Access, doc: &DocRef) -> Result<bool, HookError> {
        Ok(self.permits(access, doc))
    }

    async fn apply(&self, manifest: &Manifest, effects: &[Effect]) -> Result<(), ApplyFailure> {
        let next = self
            .applied(manifest.name, effects)
            .map_err(|error| ApplyFailure { clean: true, error })?;
        *self.state.lock().unwrap() = next;
        Ok(())
    }
}

/// Lean `Conformance.HooksContracts.Decision` rebuilt over production values.
enum Scripted<V> {
    Refuse(String),
    Proceed(V, Vec<Effect>),
    ByShown {
        count: usize,
        matching: (V, Vec<Effect>),
        otherwise: (V, Vec<Effect>),
    },
}

struct ScriptedHook<V> {
    manifest: Manifest,
    reads: Vec<String>,
    decision: Scripted<V>,
}

impl<V> Hook<()> for ScriptedHook<V>
where
    V: Clone + Send + Sync,
{
    type Value = V;

    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn reads(&self, _event: &()) -> Vec<Query> {
        self.reads
            .iter()
            .map(|collection| {
                Query::select(collection.clone(), "value").eq(PLUGIN_FIELD, self.manifest.name)
            })
            .collect()
    }

    fn decide(&self, _event: &(), rows: &[Vec<Row>]) -> Outcome<V> {
        let (value, effects) = match &self.decision {
            Scripted::Refuse(reason) => return Outcome::Refuse(reason.clone()),
            Scripted::Proceed(value, effects) => (value, effects),
            Scripted::ByShown {
                count,
                matching,
                otherwise,
            } => {
                let shown: usize = rows.iter().map(Vec::len).sum();
                let chosen = if shown == *count { matching } else { otherwise };
                (&chosen.0, &chosen.1)
            }
        };
        Outcome::Proceed {
            value: value.clone(),
            effects: effects.clone(),
        }
    }
}

fn plugin<L, V, F>(script: &LeanLifecycleHookScript<L>, value: F) -> Plugin<(), V>
where
    V: Clone + Send + Sync + 'static,
    F: Fn(&L) -> V,
{
    let choice =
        |choice: &LeanLifecycleHookChoice<L>| (value(&choice.value), effects(&choice.effects));
    let decision = match &script.decision {
        LeanLifecycleHookDecision::Refuse { reason } => Scripted::Refuse(reason.clone()),
        LeanLifecycleHookDecision::Proceed {
            value: chosen,
            effects: listed,
        } => Scripted::Proceed(value(chosen), effects(listed)),
        LeanLifecycleHookDecision::ByShown {
            count,
            matching,
            otherwise,
        } => Scripted::ByShown {
            count: *count,
            matching: choice(matching),
            otherwise: choice(otherwise),
        },
    };
    Arc::new(ScriptedHook {
        manifest: manifest(&script.name, &script.owns, script.bound),
        reads: script.reads.clone(),
        decision,
    })
}

fn string_plugins(scripts: &[LeanLifecycleHookScript<String>]) -> Vec<Plugin<(), String>> {
    scripts
        .iter()
        .map(|script| plugin(script, String::clone))
        .collect()
}

#[tokio::test]
async fn generated_lifecycle_hook_effect_cases_drive_validation_and_application() {
    let cases = lean_lifecycle_hook_effect_cases();
    assert!(!cases.is_empty());
    for case in cases {
        let host = StoreDouble::of(&case.world);
        let manifest = manifest_of(&case.manifest);
        let effects = effects(&case.effects);
        let valid = validate(
            &manifest,
            &effects,
            |access, doc| host.permits(access, doc),
            &Named::default(),
        );
        assert_eq!(valid.is_ok(), case.valid, "{}: {valid:?}", case.name);
        let committed = match valid {
            Ok(()) => host
                .apply(&manifest, &effects)
                .await
                .ok()
                .map(|()| host.snapshot()),
            Err(_) => None,
        };
        assert_eq!(
            committed,
            case.committed.as_ref().map(snapshot_of),
            "{}",
            case.name
        );
    }
}

fn lean_ref(lean: &LeanLifecycleHookRef) -> DocRef {
    DocRef::new(&lean.collection, &lean.doc)
}

/// Lean `Hooks.grantsValid` decided by production `validate`. A native grant
/// rides on the create of its target, so a case granting on a document the
/// result does not create has no native form; the model must find it invalid.
#[test]
fn generated_lifecycle_hook_grant_cases_drive_create_grant_validation() {
    let cases = lean_lifecycle_hook_grant_cases();
    assert!(!cases.is_empty());
    let mut representable = 0;
    for case in cases {
        let mut effects = effects(&case.effects);
        let mut placed = 0;
        for effect in &mut effects {
            let Effect::Write(Write::Create { doc, grants, .. }) = effect else {
                continue;
            };
            for grant in &case.grants {
                if lean_ref(&grant.target) != *doc {
                    continue;
                }
                placed += 1;
                grants.push(Grant {
                    relation: grant.relation.clone(),
                    subject: match &grant.subject {
                        LeanLifecycleHookSubject::Actor { did } => Subject::Actor(did.clone()),
                        LeanLifecycleHookSubject::Document { collection, doc } => {
                            Subject::Document(DocRef::new(collection, doc))
                        }
                    },
                });
            }
        }
        if placed < case.grants.len() {
            assert!(
                !case.valid,
                "{}: a grant off its create is valid",
                case.name
            );
            continue;
        }
        representable += 1;
        let mut owns: Vec<String> = case
            .effects
            .iter()
            .filter_map(|effect| match effect {
                LeanLifecycleHookEffect::Create { collection, .. }
                | LeanLifecycleHookEffect::Update { collection, .. }
                | LeanLifecycleHookEffect::Delete { collection, .. } => Some(collection.clone()),
                LeanLifecycleHookEffect::Core { .. } => None,
            })
            .collect();
        owns.sort();
        owns.dedup();
        let manifest = Manifest {
            name: "grants",
            owns: owns
                .iter()
                .map(|collection| OwnedCollection {
                    name: leak(collection),
                    schema: None,
                    grants: Box::leak(
                        case.declared
                            .iter()
                            .filter(|declared| &declared.collection == collection)
                            .map(|declared| Grantable {
                                relation: leak(&declared.relation),
                                subject: match &declared.subject {
                                    LeanLifecycleHookSubjectKind::Actor => SubjectKind::Actor,
                                    LeanLifecycleHookSubjectKind::Document {
                                        resource,
                                        collection,
                                    } => SubjectKind::Document {
                                        resource: leak(resource),
                                        collection: leak(collection),
                                    },
                                },
                            })
                            .collect::<Vec<_>>()
                            .into_boxed_slice(),
                    ),
                })
                .collect(),
            bound: effects.len(),
        };
        let named = Named {
            actor: Some(case.actor.clone()),
            actors: case.named_actors.clone(),
            docs: case.named_docs.iter().map(lean_ref).collect(),
        };
        let valid = validate(&manifest, &effects, |_, _| true, &named);
        assert_eq!(valid.is_ok(), case.valid, "{}: {valid:?}", case.name);
    }
    assert!(representable > 0);
}

#[tokio::test]
async fn generated_lifecycle_hook_stage_cases_drive_refusable_staging() {
    let cases = lean_lifecycle_hook_stage_cases();
    assert!(!cases.is_empty());
    for case in cases {
        let host = StoreDouble::of(&case.world);
        let initial = host.snapshot();
        let plugins = string_plugins(&case.plugins);
        let staged = stage(&host, &plugins, &()).await;
        // The caller's transaction commits only an `Ok`; an error rolls back.
        let settled = match (&staged, &case.result) {
            (
                Ok(values),
                LeanLifecycleHookStageResult::Ok {
                    state,
                    values: expected,
                },
            ) => {
                assert_eq!(values, expected, "{}: values", case.name);
                assert_eq!(host.snapshot(), snapshot_of(state), "{}: state", case.name);
                host.snapshot()
            }
            (Err(error), LeanLifecycleHookStageResult::Error { refusal }) => {
                assert_eq!(&host.refusal(error), refusal, "{}: {error:?}", case.name);
                initial
            }
            (actual, expected) => {
                panic!("{}: staged {actual:?}, model {expected:?}", case.name)
            }
        };
        assert_eq!(
            settled,
            snapshot_of(&case.settled),
            "{}: settled",
            case.name
        );
    }
}

#[tokio::test]
async fn generated_lifecycle_hook_must_commit_cases_drive_dropping_staging() {
    let cases = lean_lifecycle_hook_must_commit_cases();
    assert!(!cases.is_empty());
    for case in cases {
        if case.transition_refused {
            assert_eq!(case.committed, None, "{}", case.name);
            assert!(case.owed.is_empty(), "{}", case.name);
            continue;
        }
        let expected = case
            .committed
            .as_ref()
            .unwrap_or_else(|| panic!("{}: core's transition commits", case.name));
        for name in &case.read_failed {
            let script = case
                .plugins
                .iter()
                .find(|script| &script.name == name)
                .unwrap_or_else(|| panic!("{}: unknown plugin {name}", case.name));
            assert!(
                !script.reads.is_empty(),
                "{}: a failed read needs a declared read",
                case.name
            );
        }
        let mut core = case.world.core.clone();
        core.push(case.transition.clone());
        let host = StoreDouble::new(&case.world, core, &case.read_failed);
        let plugins = string_plugins(&case.plugins);
        let staged = stage_dropping(&host, &plugins, &())
            .await
            .unwrap_or_else(|error| panic!("{}: {error:?}", case.name));
        let dropped: Vec<bool> = staged.iter().map(Staged::is_owed).collect();
        assert_eq!(dropped, expected.dropped, "{}: dropped", case.name);
        assert_eq!(owed(&staged), case.owed, "{}: owed", case.name);
        assert_eq!(
            host.snapshot(),
            snapshot_of(&expected.state),
            "{}: state",
            case.name
        );
    }
}

#[test]
fn generated_lifecycle_hook_combine_cases_drive_verdict_combination() {
    let cases = lean_lifecycle_hook_combine_cases();
    assert!(!cases.is_empty());
    for case in cases {
        assert_eq!(
            combine(case.verdicts.iter().map(verdict)),
            verdict(&case.combined),
            "{}",
            case.name
        );
    }
}

#[tokio::test]
async fn generated_lifecycle_hook_gate_cases_drive_the_claim_gate() {
    let cases = lean_lifecycle_hook_gate_cases();
    assert!(!cases.is_empty());
    for case in cases {
        let host = StoreDouble::of(&case.world);
        let initial = host.snapshot();
        let plugins: Vec<Plugin<(), ClaimVerdict>> = case
            .plugins
            .iter()
            .map(|script| plugin(script, verdict))
            .collect();
        let gated = gate(&host, &plugins, &()).await;
        match (&gated, &case.result) {
            (
                Ok(verdicts),
                LeanLifecycleHookGateResult::Ok {
                    verdicts: expected,
                    combined,
                },
            ) => {
                let expected: Vec<ClaimVerdict> = expected.iter().map(verdict).collect();
                assert_eq!(verdicts, &expected, "{}: verdicts", case.name);
                assert_eq!(
                    combine(verdicts.iter().cloned()),
                    verdict(combined),
                    "{}: combined",
                    case.name
                );
            }
            (Err(error), LeanLifecycleHookGateResult::Error { refusal }) => {
                assert_eq!(&host.refusal(error), refusal, "{}: {error:?}", case.name);
            }
            (actual, expected) => {
                panic!("{}: gated {actual:?}, model {expected:?}", case.name)
            }
        }
        assert_eq!(
            host.snapshot(),
            initial,
            "{}: a gate writes nothing",
            case.name
        );
    }
}

/// A Lean key is an opaque string; production keys are
/// `RequestAssignmentScope` values, compared without interpretation.
fn opaque_key(key: &str) -> gents_protocol::goal::RequestAssignmentScope {
    gents_protocol::goal::RequestAssignmentScope::Unresolved {
        entry_doc_id: key.to_owned(),
    }
}

/// A Lean answer list is aligned with the requests asked; production answers
/// are by physical request.
fn scope_answers(asked: &[String], lean: &[LeanLifecycleHookScopeAnswer]) -> ScopeAnswers {
    let mut answers = ScopeAnswers::default();
    for (doc, answer) in asked.iter().zip(lean) {
        match answer {
            LeanLifecycleHookScopeAnswer::Unanswered => {}
            LeanLifecycleHookScopeAnswer::Keyed { key } => {
                answers.insert(doc.clone(), ScopeAnswer::Keyed(opaque_key(key)));
            }
            LeanLifecycleHookScopeAnswer::Failed { reason } => {
                answers.insert(
                    doc.clone(),
                    ScopeAnswer::Failed(ScopeFailure::of(&anyhow::anyhow!(reason.clone()))),
                );
            }
        }
    }
    answers
}

fn partition_of(scope: anyhow::Result<Partition>) -> LeanLifecycleHookPartition {
    match scope {
        Ok(partition) => LeanLifecycleHookPartition::Ok {
            partition: partition
                .keys()
                .iter()
                .map(|(plugin, key)| LeanLifecycleHookScopeKey {
                    plugin: (*plugin).to_owned(),
                    key: match key {
                        gents_protocol::goal::RequestAssignmentScope::Unresolved {
                            entry_doc_id,
                        } => entry_doc_id.clone(),
                        other => panic!("a generated key is opaque, not {other:?}"),
                    },
                })
                .collect(),
        },
        Err(error) => {
            let refused = error
                .downcast_ref::<ScopeRefused>()
                .unwrap_or_else(|| panic!("a request failure is a ScopeRefused: {error:?}"));
            LeanLifecycleHookPartition::Error {
                refusal: LeanLifecycleHookRefusal::Refused {
                    plugin: refused.plugin.to_owned(),
                    reason: error.to_string(),
                },
            }
        }
    }
}

#[tokio::test]
async fn generated_lifecycle_hook_queue_scope_cases_drive_the_queue_partition() {
    let cases = lean_lifecycle_hook_queue_scope_cases();
    assert!(!cases.is_empty());
    for case in cases {
        let host = StoreDouble::of(&case.world);
        let initial = host.snapshot();
        let plugins: Vec<Plugin<(), ScopeAnswers>> = case
            .plugins
            .iter()
            .map(|script| plugin(script, |answers| scope_answers(&case.asked, answers)))
            .collect();
        let answered = gate(&host, &plugins, &()).await;
        match (answered, &case.result) {
            (Ok(answers), LeanLifecycleHookQueueScopeResult::Ok { scopes }) => {
                let named: Vec<(&'static str, ScopeAnswers)> = plugins
                    .iter()
                    .map(|plugin| plugin.manifest().name)
                    .zip(answers)
                    .collect();
                let partitions: Vec<LeanLifecycleHookPartition> = case
                    .asked
                    .iter()
                    .map(|doc| partition_of(partition(&named, doc)))
                    .collect();
                assert_eq!(&partitions, scopes, "{}: partitions", case.name);
            }
            (Err(error), LeanLifecycleHookQueueScopeResult::Error { refusal }) => {
                assert_eq!(&host.refusal(&error), refusal, "{}: {error:?}", case.name);
            }
            (actual, expected) => {
                panic!("{}: answered {actual:?}, model {expected:?}", case.name)
            }
        }
        assert_eq!(
            host.snapshot(),
            initial,
            "{}: a returned-value point writes nothing",
            case.name
        );
    }
}

fn record_of(lean: &LeanLifecycleHookDeliveryRecord) -> DeliveryRecord<String, String> {
    DeliveryRecord {
        terminal: lean.terminal.clone(),
        owed: lean.owed.clone(),
        delivered: lean.delivered.clone(),
    }
}

#[test]
fn generated_lifecycle_hook_delivery_cases_drive_the_delivery_record() {
    let cases = lean_lifecycle_hook_delivery_cases();
    assert!(!cases.is_empty());
    for case in cases {
        let mut record = record_of(&case.initial);
        for (index, entry) in case.trace.iter().enumerate() {
            match &entry.step {
                LeanLifecycleHookDeliveryStep::Publish {
                    outcome,
                    keys,
                    commit,
                } => record.publish(outcome.clone(), keys.iter().cloned(), *commit),
                LeanLifecycleHookDeliveryStep::Deliver { key, applied } => {
                    record.deliver(key, *applied)
                }
            }
            assert_eq!(
                record,
                record_of(&entry.state),
                "{} step {index}: record",
                case.name
            );
            assert_eq!(
                record.pending(),
                entry.pending,
                "{} step {index}: pending",
                case.name
            );
        }
    }
}

fn binding_of(lean: &LeanLifecycleHookBinding) -> Binding<String> {
    Binding {
        actor: lean.actor.clone(),
        point: leak(&lean.point),
        subject: lean.subject.clone(),
        inputs: lean.inputs.clone(),
        configuration: lean.configuration.clone(),
    }
}

fn stale_of(lean: &LeanLifecycleHookStale) -> Stale {
    match lean {
        LeanLifecycleHookStale::Unauthorized => Stale::Unauthorized,
        LeanLifecycleHookStale::Cancelled => Stale::Cancelled,
        LeanLifecycleHookStale::Missing => Stale::Missing,
        LeanLifecycleHookStale::Stale => Stale::Stale,
        LeanLifecycleHookStale::Denied { reason } => Stale::Denied(reason.clone()),
        LeanLifecycleHookStale::Unavailable => Stale::Unavailable,
    }
}

#[test]
fn generated_lifecycle_hook_admit_cases_drive_prepared_admission() {
    let cases = lean_lifecycle_hook_admit_cases();
    assert!(!cases.is_empty());
    for case in cases {
        let current = binding_of(&case.current);
        let decision = case.prepared.as_ref().map(|prepared| {
            let captured = binding_of(&prepared.binding);
            let verdict = match &prepared.verdict {
                LeanLifecycleHookPreparedVerdict::Allow { payload } => {
                    Verdict::Allow(payload.clone())
                }
                LeanLifecycleHookPreparedVerdict::Deny { reason } => Verdict::Deny(reason.clone()),
                LeanLifecycleHookPreparedVerdict::Unavailable => Verdict::Unavailable,
            };
            let decision = prepare(captured.clone(), |_: &()| verdict, &());
            assert_eq!(
                decision.binding, captured,
                "{}: the host's binding is kept",
                case.name
            );
            decision
        });
        let expected = match &case.result {
            LeanLifecycleHookAdmitResult::Ok { payload } => Ok(payload.clone()),
            LeanLifecycleHookAdmitResult::Error { stale } => Err(stale_of(stale)),
        };
        assert_eq!(
            admit(case.authorized, case.cancelled, &current, decision),
            expected,
            "{}",
            case.name
        );
    }
}
