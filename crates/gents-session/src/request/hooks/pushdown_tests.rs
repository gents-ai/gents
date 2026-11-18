use std::sync::{Arc, Mutex};

use crate::request::hooks::claim::{combine, ClaimVerdict, Successor};
use crate::request::hooks::delivery::DeliveryRecord;
use crate::request::hooks::effect::{
    validate, Access, CoreOp, DocRef, Effect, Grant, Grantable, Manifest, Named, OwnedCollection,
    Row, Subject, SubjectKind, Write,
};
use crate::request::hooks::hook::{Hook, Outcome, Query};
use crate::request::hooks::owed;
use crate::request::hooks::prepared::{admit, prepare, Binding, Stale, Verdict};
use crate::request::hooks::queue_scope::{
    partition, Partition, ScopeAnswer, ScopeAnswers, ScopeFailure, ScopeRefused,
};
use crate::request::hooks::stage::{
    gate, owed, stage, stage_dropping, ApplyFailure, HookError, Host, Plugin, Staged,
};
use gents_lean_vocab::families::hooks_contracts_claim_gate::{self as claim_gate};
use gents_lean_vocab::families::hooks_contracts_delivery_step::{self as delivery_step};
use gents_lean_vocab::families::hooks_contracts_effect_commit::{self as effect_commit};
use gents_lean_vocab::families::hooks_contracts_grant_validity::{self as grant_validity};
use gents_lean_vocab::families::hooks_contracts_must_commit_staging::{self as must_commit};
use gents_lean_vocab::families::hooks_contracts_owed_derive::{self as owed_derive};
use gents_lean_vocab::families::hooks_contracts_owed_prune::{self as owed_prune};
use gents_lean_vocab::families::hooks_contracts_owed_trace::{self as owed_trace};
use gents_lean_vocab::families::hooks_contracts_prepared_admission::{self as admission};
use gents_lean_vocab::families::hooks_contracts_queue_partition::{self as queue_partition};
use gents_lean_vocab::families::hooks_contracts_refusable_staging::{self as staging};
use gents_lean_vocab::families::hooks_contracts_verdict_combination::{self as combination};
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
        history: Vec::new(),
    }
}

/// Lean `Hooks.State` as the store double holds it.
#[derive(Debug, Clone, PartialEq)]
struct Snapshot {
    core: Vec<String>,
    rows: Vec<Row>,
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
    fn of_wire(world: &effect_commit::WorldWire, core: Vec<String>, unreadable: &[String]) -> Self {
        Self {
            state: Mutex::new(Snapshot {
                core,
                rows: world.rows.iter().map(row_of).collect(),
            }),
            denied: world
                .denied
                .iter()
                .map(|denial| {
                    (
                        access_of(&denial.access),
                        DocRef::new(&denial.collection, &denial.doc),
                    )
                })
                .collect(),
            failing_ops: world.failing_ops.clone(),
            unreadable: unreadable.to_vec(),
            failed_operation: Mutex::new(None),
        }
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
                ..
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
            Write::Delete { doc, expected, .. } => {
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
                        return Err(HookError::Conflict {
                            plugin,
                            message: write.conflict().map(str::to_owned),
                        });
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

    fn refusal_wire(&self, error: &HookError) -> staging::RefusalWire {
        let (kind, plugin, reason) = match error {
            HookError::Refused { plugin, reason } => ("refused", *plugin, Some(reason.clone())),
            HookError::Invalid { plugin, .. } | HookError::GateEffects { plugin } => {
                ("invalid", *plugin, None)
            }
            HookError::Conflict { plugin, .. } => ("conflict", *plugin, None),
            HookError::Operation(_) => (
                "conflict",
                self.failed_operation
                    .lock()
                    .unwrap()
                    .expect("a failed core operation records its plugin"),
                None,
            ),
            other => panic!("the model has no refusal for {other:?}"),
        };
        staging::RefusalWire {
            kind: kind.to_owned(),
            plugin: Some(plugin.to_owned()),
            reason,
        }
    }
}

#[async_trait::async_trait]
impl Host for StoreDouble {
    fn actor(&self) -> Option<String> {
        None
    }

    async fn select(
        &self,
        _manifest: &Manifest,
        _named: &[DocRef],
        query: &Query,
    ) -> Result<Vec<Row>, HookError> {
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

fn effect_of(wire: &effect_commit::EffectWire) -> Effect {
    let doc = || {
        DocRef::new(
            wire.collection
                .as_deref()
                .expect("a write names its collection"),
            wire.doc.as_deref().expect("a write names its document"),
        )
    };
    let value = || Value::String(wire.value.clone().expect("the write carries its value"));
    let expected = || {
        wire.expected
            .expect("the write names the revision it was shown")
    };
    match wire.kind.as_str() {
        "create" => Effect::Write(Write::Create {
            doc: doc(),
            value: value(),
            grants: Vec::new(),
        }),
        "update" => Effect::Write(Write::Update {
            doc: doc(),
            expected: expected(),
            value: value(),
            conflict: None,
        }),
        "delete" => Effect::Write(Write::Delete {
            doc: doc(),
            expected: expected(),
            conflict: None,
        }),
        "core" => Effect::Core(CoreOp::new(
            wire.op
                .as_deref()
                .expect("a core effect names its operation"),
            Value::Null,
        )),
        other => panic!("the model has no effect {other}"),
    }
}

fn row_of(wire: &effect_commit::RowWire) -> Row {
    Row {
        doc: DocRef::new(&wire.collection, &wire.doc),
        revision: wire.revision,
        value: Value::String(wire.value.clone()),
    }
}

fn row_wire(row: &Row) -> effect_commit::RowWire {
    effect_commit::RowWire {
        collection: row.doc.collection.clone(),
        doc: row.doc.doc_id.clone(),
        revision: row.revision,
        value: row
            .value
            .as_str()
            .expect("a modeled row holds a string")
            .to_owned(),
    }
}

fn state_wire(snapshot: &Snapshot) -> effect_commit::StateWire {
    effect_commit::StateWire {
        core: snapshot.core.clone(),
        rows: snapshot.rows.iter().map(row_wire).collect(),
    }
}

fn access_of(wire: &str) -> Access {
    match wire {
        "create" => Access::Create,
        "update" => Access::Update,
        "delete" => Access::Delete,
        other => panic!("the model has no access {other}"),
    }
}

fn bound_of(bound: u64) -> usize {
    usize::try_from(bound).expect("a modeled bound fits")
}

#[tokio::test]
async fn effect_validation_and_application_follow_the_lean_model() {
    for case in gents_lean_vocab::families::hooks_contracts_effect_commit::cases() {
        let input = &case.input;
        let host = StoreDouble::of_wire(&input.world, input.world.core.clone(), &[]);
        let manifest = manifest(
            &input.manifest.name,
            &input.manifest.owns,
            bound_of(input.manifest.bound),
        );
        let effects: Vec<Effect> = input.effects.iter().map(effect_of).collect();
        let valid = validate(
            &manifest,
            &effects,
            |access, doc| host.permits(access, doc),
            &Named::default(),
        )
        .is_ok();
        let committed = if valid {
            host.apply(&manifest, &effects)
                .await
                .ok()
                .map(|()| state_wire(&host.snapshot()))
        } else {
            None
        };
        case.check(effect_commit::EffectOutput { valid, committed });
    }
}

fn grant_ref(wire: &grant_validity::RefWire) -> DocRef {
    DocRef::new(&wire.collection, &wire.doc)
}

/// Lean `Hooks.grantsValid` decided by production `validate`, each grant
/// riding on the create of its target.
#[test]
fn create_grant_validation_follows_the_lean_model() {
    gents_lean_vocab::families::hooks_contracts_grant_validity::replay(
        |input: &grant_validity::GrantInput| {
            let wires: Vec<effect_commit::EffectWire> =
                gents_lean_vocab::family::recode(&input.effects);
            let mut effects: Vec<Effect> = wires.iter().map(effect_of).collect();
            let mut placed = 0;
            for effect in &mut effects {
                let Effect::Write(Write::Create { doc, grants, .. }) = effect else {
                    continue;
                };
                for grant in &input.grants {
                    if grant_ref(&grant.target) != *doc {
                        continue;
                    }
                    placed += 1;
                    grants.push(Grant {
                        relation: grant.relation.clone(),
                        subject: match grant.subject.kind.as_str() {
                            "actor" => Subject::Actor(
                                grant
                                    .subject
                                    .did
                                    .clone()
                                    .expect("an actor grant names its DID"),
                            ),
                            "document" => Subject::Document(DocRef::new(
                                grant
                                    .subject
                                    .collection
                                    .as_deref()
                                    .expect("a document grant names its collection"),
                                grant
                                    .subject
                                    .doc
                                    .as_deref()
                                    .expect("a document grant names its document"),
                            )),
                            other => panic!("the model has no grant subject {other}"),
                        },
                    });
                }
            }
            assert_eq!(
                placed,
                input.grants.len(),
                "{}: every grant rides on a create",
                input.name
            );
            let mut owns: Vec<String> = wires
                .iter()
                .filter_map(|effect| effect.collection.clone())
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
                            input
                                .declared
                                .iter()
                                .filter(|declared| &declared.collection == collection)
                                .map(|declared| Grantable {
                                    relation: leak(&declared.relation),
                                    subject: match declared.subject.kind.as_str() {
                                        "actor" => SubjectKind::Actor,
                                        "document" => {
                                            SubjectKind::Document {
                                                resource: leak(
                                                    declared.subject.resource.as_deref().expect(
                                                        "a document grant names its resource",
                                                    ),
                                                ),
                                                collection: leak(
                                                    declared.subject.collection.as_deref().expect(
                                                        "a document grant names its collection",
                                                    ),
                                                ),
                                            }
                                        }
                                        other => panic!("the model has no subject kind {other}"),
                                    },
                                })
                                .collect::<Vec<_>>()
                                .into_boxed_slice(),
                        ),
                    })
                    .collect(),
                bound: effects.len(),
                history: Vec::new(),
            };
            let named = Named {
                actor: Some(input.actor.clone()),
                actors: input.named_actors.clone(),
                docs: input.named_docs.iter().map(grant_ref).collect(),
            };
            grant_validity::GrantOutput {
                valid: validate(&manifest, &effects, |_, _| true, &named).is_ok(),
            }
        },
    );
}

fn effects_of<T>(wires: &T) -> Vec<Effect>
where
    T: serde::Serialize,
{
    let wires: Vec<effect_commit::EffectWire> = gents_lean_vocab::family::recode(wires);
    wires.iter().map(effect_of).collect()
}

fn scripted<V, F>(script: &staging::ScriptWire, value: F) -> Plugin<(), V>
where
    V: Clone + Send + Sync + 'static,
    F: Fn(&staging::ValueWire) -> V,
{
    let decision = &script.decision;
    let choice = |choice: &Option<staging::ChoiceWire>| {
        let choice = choice
            .as_ref()
            .expect("a shown-count decision has both choices");
        (value(&choice.value), effects_of(&choice.effects))
    };
    let decision = match decision.kind.as_str() {
        "refuse" => Scripted::Refuse(decision.reason.clone().expect("a refusal has a reason")),
        "proceed" => Scripted::Proceed(
            value(
                decision
                    .value
                    .as_ref()
                    .expect("a proceed carries its value"),
            ),
            effects_of(&decision.effects),
        ),
        "by_shown" => Scripted::ByShown {
            count: usize::try_from(decision.count.expect("a shown-count decision has a count"))
                .expect("a modeled count fits"),
            matching: choice(&decision.matching),
            otherwise: choice(&decision.otherwise),
        },
        other => panic!("the model has no decision {other}"),
    };
    Arc::new(ScriptedHook {
        manifest: manifest(&script.name, &script.owns, bound_of(script.bound)),
        reads: script.reads.clone(),
        decision,
    })
}

fn text_plugins(scripts: &[staging::ScriptWire]) -> Vec<Plugin<(), String>> {
    scripts
        .iter()
        .map(|script| {
            scripted(script, |value| {
                value.text.clone().expect("a staging value is text")
            })
        })
        .collect()
}

fn host_of<T>(world: &T, core: Option<Vec<String>>, unreadable: &[String]) -> StoreDouble
where
    T: serde::Serialize,
{
    let world: effect_commit::WorldWire = gents_lean_vocab::family::recode(world);
    let core = core.unwrap_or_else(|| world.core.clone());
    StoreDouble::of_wire(&world, core, unreadable)
}

fn state_of<T>(snapshot: &Snapshot) -> T
where
    T: serde::de::DeserializeOwned,
{
    gents_lean_vocab::family::recode(&state_wire(snapshot))
}

#[tokio::test]
async fn refusable_staging_follows_the_lean_model() {
    for case in gents_lean_vocab::families::hooks_contracts_refusable_staging::cases() {
        let host = host_of(&case.input.world, None, &[]);
        let initial = host.snapshot();
        let plugins = text_plugins(&case.input.plugins);
        let staged = stage(&host, &plugins, &()).await;
        let (result, settled) = match staged {
            Ok(values) => (
                staging::StageResultWire {
                    kind: "ok".to_owned(),
                    state: Some(state_of(&host.snapshot())),
                    values,
                    refusal: None,
                },
                host.snapshot(),
            ),
            Err(error) => (
                staging::StageResultWire {
                    kind: "error".to_owned(),
                    state: None,
                    values: Vec::new(),
                    refusal: Some(host.refusal_wire(&error)),
                },
                initial,
            ),
        };
        case.check(staging::StageOutput {
            result,
            settled: state_of(&settled),
        });
    }
}

#[tokio::test]
async fn must_commit_staging_follows_the_lean_model() {
    for case in gents_lean_vocab::families::hooks_contracts_must_commit_staging::cases() {
        let input = &case.input;
        assert!(
            !input.transition_refused,
            "{}: a refused transition never reaches staging",
            input.name
        );
        for name in &input.read_failed {
            let script = input
                .plugins
                .iter()
                .find(|script| &script.name == name)
                .unwrap_or_else(|| panic!("{}: unknown plugin {name}", input.name));
            assert!(
                !script.reads.is_empty(),
                "{}: a failed read needs a declared read",
                input.name
            );
        }
        let scripts: Vec<staging::ScriptWire> = gents_lean_vocab::family::recode(&input.plugins);
        let plugins = text_plugins(&scripts);
        let mut core = input.world.core.clone();
        core.push(input.transition.clone());
        let host = host_of(&input.world, Some(core), &input.read_failed);
        let staged = stage_dropping(&host, &plugins, &(), &[])
            .await
            .unwrap_or_else(|error| panic!("{}: {error:?}", input.name));
        case.check(must_commit::MustCommitOutput {
            committed: Some(must_commit::CommittedWire {
                state: state_of(&host.snapshot()),
                dropped: staged.iter().map(Staged::is_owed).collect(),
            }),
            owed: owed(&staged).into_iter().map(str::to_owned).collect(),
        });
    }
}

fn claim_verdict(wire: &combination::VerdictWire) -> ClaimVerdict {
    let reason = || wire.reason.clone().expect("the verdict carries its reason");
    match wire.kind.as_str() {
        "allow" => ClaimVerdict::Allow,
        "leave_queued" => ClaimVerdict::LeaveQueued,
        "retire" => {
            let successor = wire
                .successor
                .as_ref()
                .expect("a retirement names its successor");
            ClaimVerdict::Retire {
                reason: reason(),
                successor: Successor {
                    request_id: successor.request_id.clone(),
                    request_doc_id: successor.request_doc_id.clone(),
                },
            }
        }
        "reject" => ClaimVerdict::Reject { reason: reason() },
        other => panic!("the model has no verdict {other}"),
    }
}

fn verdict_wire(verdict: &ClaimVerdict) -> combination::VerdictWire {
    let (kind, reason, successor) = match verdict {
        ClaimVerdict::Allow => ("allow", None, None),
        ClaimVerdict::LeaveQueued => ("leave_queued", None, None),
        ClaimVerdict::Retire { reason, successor } => (
            "retire",
            Some(reason.clone()),
            Some(combination::SuccessorWire {
                request_id: successor.request_id.clone(),
                request_doc_id: successor.request_doc_id.clone(),
            }),
        ),
        ClaimVerdict::Reject { reason } => ("reject", Some(reason.clone()), None),
    };
    combination::VerdictWire {
        kind: kind.to_owned(),
        reason,
        successor,
    }
}

#[test]
fn verdict_combination_follows_the_lean_model() {
    gents_lean_vocab::families::hooks_contracts_verdict_combination::replay(
        |input: &combination::CombinationInput| combination::CombinationOutput {
            combined: verdict_wire(&combine(input.verdicts.iter().map(claim_verdict))),
        },
    );
}

fn gate_verdict(verdict: &ClaimVerdict) -> claim_gate::VerdictWire {
    gents_lean_vocab::family::recode(&verdict_wire(verdict))
}

#[tokio::test]
async fn claim_gate_follows_the_lean_model() {
    for case in gents_lean_vocab::families::hooks_contracts_claim_gate::cases() {
        let input = &case.input;
        let host = host_of(&input.world, None, &[]);
        let initial = host.snapshot();
        let scripts: Vec<staging::ScriptWire> = gents_lean_vocab::family::recode(&input.plugins);
        let plugins: Vec<Plugin<(), ClaimVerdict>> = scripts
            .iter()
            .map(|script| {
                scripted(script, |value| {
                    let verdict: combination::VerdictWire = gents_lean_vocab::family::recode(
                        value.verdict.as_ref().expect("a gate value is a verdict"),
                    );
                    claim_verdict(&verdict)
                })
            })
            .collect();
        let result = match gate(&host, &plugins, &()).await {
            Ok(verdicts) => claim_gate::GateResultWire {
                kind: "ok".to_owned(),
                combined: Some(gate_verdict(&combine(verdicts.iter().cloned()))),
                verdicts: verdicts.iter().map(gate_verdict).collect(),
                refusal: None,
            },
            Err(error) => claim_gate::GateResultWire {
                kind: "error".to_owned(),
                verdicts: Vec::new(),
                combined: None,
                refusal: Some(gents_lean_vocab::family::recode(&host.refusal_wire(&error))),
            },
        };
        assert_eq!(
            host.snapshot(),
            initial,
            "{}: a gate writes nothing",
            input.name
        );
        case.check(claim_gate::GateOutput { result });
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
fn scope_answers(asked: &[String], answers: &[staging::AnswerWire]) -> ScopeAnswers {
    let mut scoped = ScopeAnswers::default();
    for (doc, answer) in asked.iter().zip(answers) {
        match answer.kind.as_str() {
            "unanswered" => {}
            "keyed" => {
                scoped.insert(
                    doc.clone(),
                    ScopeAnswer::Keyed(opaque_key(
                        answer.key.as_deref().expect("a keyed answer has its key"),
                    )),
                );
            }
            "failed" => {
                scoped.insert(
                    doc.clone(),
                    ScopeAnswer::Failed(ScopeFailure::of(&anyhow::anyhow!(answer
                        .reason
                        .clone()
                        .expect("a failed answer has its reason")))),
                );
            }
            other => panic!("the model has no scope answer {other}"),
        }
    }
    scoped
}

fn partition_of(scope: anyhow::Result<Partition>) -> queue_partition::PartitionWire {
    match scope {
        Ok(partition) => queue_partition::PartitionWire {
            kind: "ok".to_owned(),
            partition: partition
                .keys()
                .iter()
                .map(|(plugin, key)| queue_partition::ScopeKeyWire {
                    plugin: (*plugin).to_owned(),
                    key: match key {
                        gents_protocol::goal::RequestAssignmentScope::Unresolved {
                            entry_doc_id,
                        } => entry_doc_id.clone(),
                        other => panic!("a generated key is opaque, not {other:?}"),
                    },
                })
                .collect(),
            refusal: None,
        },
        Err(error) => {
            let refused = error
                .downcast_ref::<ScopeRefused>()
                .unwrap_or_else(|| panic!("a request failure is a ScopeRefused: {error:?}"));
            queue_partition::PartitionWire {
                kind: "error".to_owned(),
                partition: Vec::new(),
                refusal: Some(queue_partition::RefusalWire {
                    kind: "refused".to_owned(),
                    plugin: Some(refused.plugin.to_owned()),
                    reason: Some(error.to_string()),
                }),
            }
        }
    }
}

#[tokio::test]
async fn queue_partition_follows_the_lean_model() {
    for case in gents_lean_vocab::families::hooks_contracts_queue_partition::cases() {
        let input = &case.input;
        let host = host_of(&input.world, None, &[]);
        let initial = host.snapshot();
        let scripts: Vec<staging::ScriptWire> = gents_lean_vocab::family::recode(&input.plugins);
        let plugins: Vec<Plugin<(), ScopeAnswers>> = scripts
            .iter()
            .map(|script| scripted(script, |value| scope_answers(&input.asked, &value.answers)))
            .collect();
        let result = match gate(&host, &plugins, &()).await {
            Ok(answers) => {
                let named: Vec<(&'static str, ScopeAnswers)> = plugins
                    .iter()
                    .map(|plugin| plugin.manifest().name)
                    .zip(answers)
                    .collect();
                queue_partition::QueueScopeResultWire {
                    kind: "ok".to_owned(),
                    scopes: input
                        .asked
                        .iter()
                        .map(|doc| partition_of(partition(&named, doc)))
                        .collect(),
                    refusal: None,
                }
            }
            Err(error) => queue_partition::QueueScopeResultWire {
                kind: "error".to_owned(),
                scopes: Vec::new(),
                refusal: Some(gents_lean_vocab::family::recode(&host.refusal_wire(&error))),
            },
        };
        assert_eq!(
            host.snapshot(),
            initial,
            "{}: a returned-value point writes nothing",
            input.name
        );
        case.check(queue_partition::QueueScopeOutput { result });
    }
}

fn record_of(wire: &delivery_step::RecordWire) -> DeliveryRecord<String, String> {
    DeliveryRecord {
        terminal: wire.terminal.clone(),
        owed: wire.owed.clone(),
        delivered: wire.delivered.clone(),
    }
}

#[test]
fn delivery_steps_follow_the_lean_model() {
    gents_lean_vocab::families::hooks_contracts_delivery_step::replay(
        |input: &delivery_step::DeliveryStepInput| {
            let mut record = record_of(&input.before);
            let step = &input.step;
            match step.kind.as_str() {
                "publish" => record.publish(
                    step.outcome
                        .clone()
                        .expect("a publication names its outcome"),
                    step.keys.iter().cloned(),
                    step.commit
                        .expect("a publication says whether it committed"),
                ),
                "deliver" => record.deliver(
                    step.key.as_ref().expect("a delivery names its key"),
                    step.applied.expect("a delivery says whether it applied"),
                ),
                other => panic!("the model has no delivery step {other}"),
            }
            delivery_step::DeliveryStepOutput {
                pending: record.pending(),
                state: delivery_step::RecordWire {
                    terminal: record.terminal,
                    owed: record.owed,
                    delivered: record.delivered,
                },
            }
        },
    );
}

fn owed_key_of(wire: &owed_trace::OwedKeyWire) -> owed::Key {
    owed::Key {
        point: wire.point.clone(),
        plugin: wire.plugin.clone(),
        subject: wire.subject.clone(),
    }
}

fn owed_write_of(wire: &owed_trace::OwedWriteWire) -> owed::Write {
    owed::Write {
        subject: wire.subject.clone(),
        doc: wire.doc.clone(),
    }
}

fn owed_record_of(wire: &owed_trace::OwedRecordWire) -> owed::Record {
    owed::Record {
        key: owed_key_of(&wire.key),
        kind: match wire.kind.as_str() {
            "staging" => owed::Kind::Staging,
            "on_behalf" => owed::Kind::OnBehalf,
            other => panic!("the model has no owed kind {other}"),
        },
        actor: wire.actor.clone(),
        runner: wire.runner.clone(),
        attempts: wire.attempts,
        last_error: wire.last_error.clone(),
        settled: wire.settled.as_deref().map(|settled| match settled {
            "delivered" => owed::Settlement::Delivered,
            "dropped" => owed::Settlement::Dropped,
            other => panic!("the model has no settlement {other}"),
        }),
    }
}

fn owed_record_wire(record: &owed::Record) -> owed_trace::OwedRecordWire {
    owed_trace::OwedRecordWire {
        key: owed_trace::OwedKeyWire {
            point: record.key.point.clone(),
            plugin: record.key.plugin.clone(),
            subject: record.key.subject.clone(),
        },
        kind: match record.kind {
            owed::Kind::Staging => "staging",
            owed::Kind::OnBehalf => "on_behalf",
        }
        .to_owned(),
        actor: record.actor.clone(),
        runner: record.runner.clone(),
        attempts: record.attempts,
        last_error: record.last_error.clone(),
        settled: record.settled.map(|settled| {
            match settled {
                owed::Settlement::Delivered => "delivered",
                owed::Settlement::Dropped => "dropped",
            }
            .to_owned()
        }),
    }
}

/// Lean `Hooks.OwedDelivery.owe`, `publish`, `redrive` and `performs` drive
/// the production record the periodic sweep settles (`owed::redrive_pending`).
#[test]
fn owed_delivery_traces_follow_the_lean_model() {
    gents_lean_vocab::families::hooks_contracts_owed_trace::replay(
        |input: &owed_trace::OwedTraceInput| {
            let mut records: Vec<owed::Record> = Vec::new();
            let mut observed = Vec::new();
            for step in &input.steps {
                let (runner, performed) = match step.kind.as_str() {
                    "owe" => {
                        let point = step.point.as_deref().expect("an owe names its point");
                        let kind = owed::Kind::of_point(point);
                        let actor = step.actor.as_deref();
                        let runner = owed::owe_runner(kind, actor, step.owner.as_deref(), |did| {
                            step.signable.iter().any(|signable| signable == did)
                        });
                        if let Some(runner) = &runner {
                            owed::publish(
                                &mut records,
                                |key| owed::Record::fresh(kind, actor, runner, key),
                                step.keys.iter().map(owed_key_of),
                                step.commit.expect("an owe says whether it committed"),
                            );
                        }
                        (runner, false)
                    }
                    "redrive" => {
                        let key = owed_key_of(step.key.as_ref().expect("a redrive names its key"));
                        let observation = owed::Observation {
                            authorized: step.authorized.expect("a redrive observes authorization"),
                            current: step.current.expect("a redrive observes currency"),
                        };
                        let attempt = step.attempt.as_ref().expect("a redrive attempts");
                        let attempt = match attempt.kind.as_str() {
                            "applied" => owed::Attempt::Applied(
                                attempt.writes.iter().map(owed_write_of).collect(),
                            ),
                            "failed" => owed::Attempt::Failed(
                                attempt
                                    .error
                                    .clone()
                                    .expect("a failed attempt carries its error"),
                            ),
                            other => panic!("the model has no attempt {other}"),
                        };
                        let refused: Vec<owed::Write> =
                            step.refused.iter().map(owed_write_of).collect();
                        let may_write = |write: &owed::Write| !refused.contains(write);
                        let mut performed = false;
                        for record in records.iter_mut().filter(|record| record.key == key) {
                            performed |= record.performs(observation, &attempt, may_write);
                            *record = record.redrive(observation, &attempt, may_write);
                        }
                        (None, performed)
                    }
                    other => panic!("the model has no owed step {other}"),
                };
                observed.push(owed_trace::OwedObservation {
                    runner,
                    records: records.iter().map(owed_record_wire).collect(),
                    performed,
                });
            }
            owed_trace::OwedTraceOutput { trace: observed }
        },
    );
}

#[test]
fn owed_delivery_sweep_rows_follow_the_lean_model() {
    use gents_lean_vocab::families::recovery_sweeps_owed_delivery::{OwedDeliveryInput, Swept};
    gents_lean_vocab::families::recovery_sweeps_owed_delivery::replay(
        |input: &OwedDeliveryInput| {
            let record = owed::Record::fresh(
                owed::Kind::Staging,
                Some("user"),
                "agent",
                owed::Key {
                    point: "control.session.halt".to_owned(),
                    plugin: "goals".to_owned(),
                    subject: "s".to_owned(),
                },
            );
            let observation = owed::Observation {
                authorized: input.authorized,
                current: input.current,
            };
            let attempt = match input.attempt.as_str() {
                "applied" => owed::Attempt::Applied(
                    input
                        .writes
                        .iter()
                        .map(|write| owed::Write {
                            subject: write.subject.clone(),
                            doc: write.doc.clone(),
                        })
                        .collect(),
                ),
                "failed" => owed::Attempt::Failed(
                    input
                        .error
                        .clone()
                        .expect("a failed attempt carries its error"),
                ),
                other => panic!("the model has no attempt {other}"),
            };
            let refused: Vec<owed::Write> = input
                .refused
                .iter()
                .map(|write| owed::Write {
                    subject: write.subject.clone(),
                    doc: write.doc.clone(),
                })
                .collect();
            let may_write = |write: &owed::Write| !refused.contains(write);
            let state = |record: &owed::Record| {
                match record.settled {
                    None => "running",
                    Some(owed::Settlement::Delivered) => "delivered",
                    Some(owed::Settlement::Dropped) => "dropped",
                }
                .to_owned()
            };
            assert_eq!(state(&record), input.pre_state, "{}", input.name);
            let settles = |record: &owed::Record| {
                u64::from(
                    record.pending() && !record.redrive(observation, &attempt, may_write).pending(),
                )
            };
            let after = record.redrive(observation, &attempt, may_write);
            Swept {
                terminal_state: state(&after),
                measure_before: settles(&record),
                measure_after: settles(&after),
            }
        },
    );
}

/// Lean `Hooks.OwedDelivery.deriveHalts` drives the sweep's derivation.
#[test]
fn owed_halt_derivation_follows_the_lean_model() {
    gents_lean_vocab::families::hooks_contracts_owed_derive::replay(
        |input: &owed_derive::OwedDeriveInput| {
            let sessions: Vec<owed::HaltedSession> = input
                .sessions
                .iter()
                .map(|session| owed::HaltedSession {
                    session: session.session.clone(),
                    owner: session.owner.clone(),
                    latched_at: session.latched_at.clone(),
                    current: session.current,
                })
                .collect();
            let bound = usize::try_from(input.bound).expect("the bound fits");
            owed_derive::OwedDeriveOutput {
                derived: owed::derive_halts(bound, &sessions)
                    .into_iter()
                    .map(|halt| owed_derive::DerivedHaltWire {
                        session: halt.session,
                        halted_at: halt.at,
                        owner: halt.owner,
                    })
                    .collect(),
            }
        },
    );
}

/// Lean `Hooks.OwedDelivery.prune` keeps the records production `prunable`
/// does not delete.
#[test]
fn owed_pruning_follows_the_lean_model() {
    gents_lean_vocab::families::hooks_contracts_owed_prune::replay(
        |input: &owed_prune::OwedPruneInput| owed_prune::OwedPruneOutput {
            kept: input
                .records
                .iter()
                .filter(|aged| {
                    let record = owed_record_of(&gents_lean_vocab::family::recode(&aged.record));
                    !owed::prunable(&record, aged.age, input.retention)
                })
                .cloned()
                .collect(),
        },
    );
}

fn binding_of(wire: &admission::BindingWire) -> Binding<String> {
    Binding {
        actor: wire.actor.clone(),
        point: leak(&wire.point),
        subject: wire.subject.clone(),
        inputs: wire.inputs.clone(),
        configuration: wire.configuration.clone(),
    }
}

fn binding_wire(binding: &Binding<String>) -> admission::BindingWire {
    admission::BindingWire {
        actor: binding.actor.clone(),
        point: binding.point.to_owned(),
        subject: binding.subject.clone(),
        inputs: binding.inputs.clone(),
        configuration: binding.configuration.clone(),
    }
}

fn prepared_verdict_of(wire: &admission::PreparedVerdictWire) -> Verdict<String> {
    match wire.kind.as_str() {
        "allow" => Verdict::Allow(wire.payload.clone().expect("an allow carries its payload")),
        "deny" => Verdict::Deny(wire.reason.clone().expect("a deny carries its reason")),
        "unavailable" => Verdict::Unavailable,
        other => panic!("the model has no prepared verdict {other}"),
    }
}

fn prepared_verdict_wire(verdict: &Verdict<String>) -> admission::PreparedVerdictWire {
    let (kind, payload, reason) = match verdict {
        Verdict::Allow(payload) => ("allow", Some(payload.clone()), None),
        Verdict::Deny(reason) => ("deny", None, Some(reason.clone())),
        Verdict::Unavailable => ("unavailable", None, None),
    };
    admission::PreparedVerdictWire {
        kind: kind.to_owned(),
        payload,
        reason,
    }
}

fn stale_wire(stale: &Stale) -> admission::StaleWire {
    let (kind, reason) = match stale {
        Stale::Unauthorized => ("unauthorized", None),
        Stale::Cancelled => ("cancelled", None),
        Stale::Missing => ("missing", None),
        Stale::Stale => ("stale", None),
        Stale::Denied(reason) => ("denied", Some(reason.clone())),
        Stale::Unavailable => ("unavailable", None),
    };
    admission::StaleWire {
        kind: kind.to_owned(),
        reason,
    }
}

#[test]
fn prepared_admission_follows_the_lean_model() {
    gents_lean_vocab::families::hooks_contracts_prepared_admission::replay(
        |input: &admission::AdmissionInput| {
            let decision = input.captured.as_ref().map(|captured| {
                let verdict = prepared_verdict_of(&captured.verdict);
                prepare(binding_of(&captured.binding), |_: &()| verdict, &())
            });
            let prepared = decision.as_ref().map(|decision| admission::PreparedWire {
                binding: binding_wire(&decision.binding),
                verdict: prepared_verdict_wire(&decision.verdict),
            });
            let result = match admit(
                input.authorized,
                input.cancelled,
                &binding_of(&input.current),
                decision,
            ) {
                Ok(payload) => admission::AdmitResultWire {
                    kind: "ok".to_owned(),
                    payload: Some(payload),
                    stale: None,
                },
                Err(stale) => admission::AdmitResultWire {
                    kind: "error".to_owned(),
                    payload: None,
                    stale: Some(stale_wire(&stale)),
                },
            };
            admission::AdmissionOutput { prepared, result }
        },
    );
}
