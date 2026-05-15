use super::*;

pub(super) fn event_delivery_transition_cases_match_contract() {
    let cases = lean_event_delivery_transition_cases();
    let watcher = runtime_event_delivery_source_contract("Watcher");
    assert!(
        cases.len() >= 12,
        "Expected at least 12 transition-case rows; got {}",
        cases.len()
    );
    for case in cases {
        let mut runtime = InMemoryEventDeliverySource::new(watcher, &case.pre);
        runtime
            .apply(&case.action)
            .unwrap_or_else(|err| panic!("case `{}` rejected runtime action: {err}", case.name));
        assert_eq!(
            runtime.world, case.post,
            "case `{}` drifted from in-memory runtime replay",
            case.name
        );
    }
}

pub(super) fn event_delivery_source_instances_match_runtime() {
    let runtime_by_name = runtime_event_delivery_source_contracts()
        .iter()
        .map(|instance| (instance.name, *instance))
        .collect::<HashMap<_, _>>();

    for lean in lean_event_delivery_source_instances() {
        let runtime = runtime_by_name
            .get(lean.name.as_str())
            .unwrap_or_else(|| panic!("runtime source {:?} must be present", lean.name));
        assert_eq!(runtime.dedupe_policy, lean.dedupe_policy);
        assert_eq!(runtime.rescan_bounded_by, lean.rescan_bounded_by);
        assert_eq!(runtime.deviation, lean.deviation.as_deref());
    }
    assert_eq!(
        runtime_by_name.len(),
        lean_event_delivery_source_instances().len(),
        "runtime source introspection should not expose unmodeled sources"
    );
}

pub(super) fn event_delivery_convergence_traces_match_runtime_or_deviation() {
    let traces = lean_event_delivery_convergence_traces();
    assert!(
        traces.len() >= 3,
        "Expected at least one convergence trace per source"
    );

    for trace in traces {
        let source = runtime_event_delivery_source_contract(&trace.instance_name);
        let mut runtime = InMemoryEventDeliverySource::new(source, &trace.initial_world);
        for action in &trace.actions {
            runtime.apply(action).unwrap_or_else(|err| {
                panic!("trace `{}` rejected runtime action: {err}", trace.name)
            });
        }
        assert_eq!(
            runtime.world, trace.final_world,
            "trace `{}` drifted from in-memory runtime replay",
            trace.name
        );

        match trace.status.as_str() {
            "substantive" => {
                assert!(
                    runtime.unhandled_persistent_docs().is_empty(),
                    "substantive trace `{}` left persistent docs unhandled: {:?}",
                    trace.name,
                    runtime.unhandled_persistent_docs()
                );
                assert!(
                    source.deviation.is_none(),
                    "substantive trace `{}` should run against a non-deviation source",
                    trace.name
                );
            }
            "deviation" => {
                assert!(
                    source.deviation.is_some(),
                    "deviation trace `{}` must name a runtime source with a documented deviation",
                    trace.name
                );
                assert_eq!(
                    source.rescan_bounded_by, 0,
                    "deviation trace `{}` must run against an unbounded-rescan source",
                    trace.name
                );
                assert!(
                    !runtime.unhandled_persistent_docs().is_empty(),
                    "deviation trace `{}` did not witness the documented \
                     deviation state (no orphan persistent doc remaining)",
                    trace.name,
                );
            }
            other => panic!(
                "trace `{}` has unknown status `{}` (expected 'substantive' or 'deviation')",
                trace.name, other,
            ),
        }
    }

    let trace_instances: std::collections::HashSet<&str> =
        traces.iter().map(|t| t.instance_name.as_str()).collect();
    for name in &["Watcher", "EventSource", "SubagentSource"] {
        assert!(
            trace_instances.contains(name),
            "Expected a convergence trace for instance `{}`",
            name
        );
    }
}

#[derive(Debug)]
struct InMemoryEventDeliverySource {
    source: EventDeliverySourceContract,
    world: lean_vocab_test::LeanEventDeliveryWorld,
}

impl InMemoryEventDeliverySource {
    fn new(
        source: EventDeliverySourceContract,
        world: &lean_vocab_test::LeanEventDeliveryWorld,
    ) -> Self {
        Self {
            source,
            world: world.clone(),
        }
    }

    fn apply(&mut self, action: &LeanEventDeliveryAction) -> Result<(), String> {
        match action {
            LeanEventDeliveryAction::Persist { doc } => {
                if self.world.persistent_set.contains(doc) {
                    return Err(format!("doc {doc:?} already persisted"));
                }
                self.world.persistent_set.insert(0, doc.clone());
            }
            LeanEventDeliveryAction::Depersist { doc } => {
                erase_first(&mut self.world.persistent_set, doc)
                    .ok_or_else(|| format!("doc {doc:?} is not persistent"))?;
            }
            LeanEventDeliveryAction::Enqueue { doc } => {
                if !self.world.persistent_set.contains(doc) {
                    return Err(format!("doc {doc:?} is not persistent"));
                }
                self.world.subscription_queue.insert(0, doc.clone());
            }
            LeanEventDeliveryAction::Drop { doc }
            | LeanEventDeliveryAction::DeliverFromQueue { doc } => {
                erase_first(&mut self.world.subscription_queue, doc)
                    .ok_or_else(|| format!("doc {doc:?} is not queued"))?;
            }
            LeanEventDeliveryAction::RescanTick => {
                if self.source.rescan_bounded_by == 0 {
                    return Err(format!(
                        "source {} has no bounded live rescan",
                        self.source.name
                    ));
                }
                let mut rescanned = self
                    .world
                    .persistent_set
                    .iter()
                    .filter(|doc| !self.world.processed_set.contains(*doc))
                    .cloned()
                    .collect::<Vec<_>>();
                rescanned.extend(self.world.subscription_queue.clone());
                self.world.subscription_queue = rescanned;
            }
            LeanEventDeliveryAction::Handle { doc } => {
                if self.world.processed_set.contains(doc) {
                    return Err(format!("doc {doc:?} is already processed"));
                }
                erase_first(&mut self.world.subscription_queue, doc)
                    .ok_or_else(|| format!("doc {doc:?} is not queued"))?;
                self.world.processed_set.insert(0, doc.clone());
                self.world.handled.insert(0, doc.clone());
            }
        }
        Ok(())
    }

    fn unhandled_persistent_docs(&self) -> Vec<String> {
        self.world
            .persistent_set
            .iter()
            .filter(|doc| !self.world.handled.contains(*doc))
            .cloned()
            .collect()
    }
}

fn erase_first(values: &mut Vec<String>, target: &str) -> Option<String> {
    values
        .iter()
        .position(|value| value == target)
        .map(|index| values.remove(index))
}

fn runtime_event_delivery_source_contract(name: &str) -> EventDeliverySourceContract {
    runtime_event_delivery_source_contracts()
        .into_iter()
        .find(|source| source.name == name)
        .unwrap_or_else(|| panic!("runtime event-delivery source {name:?} must be present"))
}
