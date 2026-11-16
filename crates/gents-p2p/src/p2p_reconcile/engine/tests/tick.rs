#[tokio::test]
async fn peer_tick_matches_lean() {
    use gents_lean_vocab::families::pairing_tick_cases as family;

    use crate::p2p_reconcile::templates::filter_conditions;
    use crate::p2p_reconcile::{equality_filter, single_string_eq};

    #[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum LiveForm {
        Scalar,
        Predicate,
    }

    #[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum OpKind {
        Install,
        Teardown,
    }

    fn texts(addresses: &[family::Address]) -> BTreeSet<String> {
        addresses
            .iter()
            .map(|address| address.text.clone())
            .collect()
    }

    fn filters(scopes: &[family::Scope]) -> PairingFilters {
        scopes
            .iter()
            .map(|scope| {
                (
                    scope.collection.clone(),
                    equality_filter(&scope.field, &scope.value),
                )
            })
            .collect()
    }

    fn held(replicator: &family::Replicator<LiveForm>) -> RemoteReplicator {
        RemoteReplicator {
            id: Some(format!("id-{}", replicator.address.text)),
            collections: replicator
                .collections
                .iter()
                .map(|collection| mock_collection_id(collection))
                .collect(),
            address: Some(replicator.address.text.clone()),
            filters: replicator.filter.as_ref().map(|scopes| {
                scopes
                    .iter()
                    .map(|live| {
                        let scope = &live.scope;
                        let filter = match live.form {
                            LiveForm::Scalar => defra_p2p_adapter::ReplicationFilter::eq(
                                scope.field.as_str(),
                                serde_json::Value::String(scope.value.clone()),
                            ),
                            LiveForm::Predicate => defra_p2p_adapter::ReplicationFilter::predicate(
                                filter_conditions(&equality_filter(&scope.field, &scope.value))
                                    .expect("an equality scope has conditions"),
                            ),
                        };
                        (mock_collection_id(&scope.collection), filter)
                    })
                    .collect()
            }),
        }
    }

    fn collection_name(id: &str) -> String {
        id.strip_prefix("col_")
            .and_then(|rest| rest.strip_suffix("_id"))
            .expect("a transport collection id")
            .to_owned()
    }

    fn view(replicator: &RemoteReplicator) -> family::LiveView {
        family::LiveView {
            address: replicator.address.clone().expect("a held address"),
            collections: replicator
                .collections
                .iter()
                .map(|id| collection_name(id))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
            filter: replicator.filters.as_ref().map(|filters| {
                filters
                    .iter()
                    .map(|(collection, filter)| {
                        let (field, value) = match &filter.conditions {
                            Some(conditions) => {
                                let (field, operation) =
                                    conditions.iter().next().expect("one condition");
                                (field.clone(), operation["_eq"].clone())
                            }
                            None => (filter.field.clone(), filter.value.clone()),
                        };
                        family::Scope {
                            collection: collection_name(collection),
                            field,
                            value: value.as_str().expect("a string scope").to_owned(),
                        }
                    })
                    .collect()
            }),
        }
    }

    for case in family::cases::<LiveForm, OpKind>() {
        let input = &case.input;
        let mut admin = MockAdmin::default();
        for replicator in &input.replicators {
            admin
                .replicators
                .lock()
                .unwrap()
                .insert(replicator.address.text.clone(), held(replicator));
        }
        let store = MockStore::default();
        *store.applied.lock().unwrap() = PairingApplied {
            replicator_addresses: texts(&input.applied.routes),
            replicator_filter: filters(&input.applied.filter),
        };

        let mut ticks = Vec::new();
        for sweep in &input.sweeps {
            *store.desired.lock().unwrap() = if sweep.desired_readable {
                Ok(sweep.desired.as_ref().map(|desired| PairingDesired {
                    replicator_addresses: texts(&desired.routes),
                    replicator_collections: desired.collections.iter().cloned().collect(),
                    replicator_filter: filters(&desired.filter),
                    template_ids: BTreeSet::new(),
                }))
            } else {
                Err("desired state is unreadable".into())
            };
            admin.fail_active_peers = !sweep.active_readable;
            *admin.active.lock().unwrap() = sweep.active_peers.clone();
            let dials_before = admin.connects.lock().unwrap().len();

            let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
                .await
                .expect("tick");

            let applied = store.applied.lock().unwrap().clone();
            ticks.push(family::TickOutput {
                read_failed: outcome.desired_read_failed,
                dialed: admin.connects.lock().unwrap().len() > dials_before,
                peer_active: outcome.peer_active,
                ops: outcome
                    .ops_applied
                    .into_iter()
                    .map(|op| match op {
                        DiffOp::InstallReplicator(address) => family::Op {
                            kind: OpKind::Install,
                            address,
                        },
                        DiffOp::TeardownReplicator(address) => family::Op {
                            kind: OpKind::Teardown,
                            address,
                        },
                    })
                    .collect(),
                live_matches: outcome.live_route_matches,
                applied: applied.replicator_addresses.into_iter().collect(),
                applied_filter: applied
                    .replicator_filter
                    .iter()
                    .map(|(collection, filter)| {
                        let (field, value) =
                            single_string_eq(filter).expect("an applied scope is one equality");
                        family::Scope {
                            collection: collection.clone(),
                            field: field.to_owned(),
                            value: value.to_owned(),
                        }
                    })
                    .collect(),
                live: admin
                    .replicators
                    .lock()
                    .unwrap()
                    .values()
                    .map(view)
                    .collect(),
            });
        }
        case.check(family::Output { ticks });
    }
}
