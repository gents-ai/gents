use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};

use gents_protocol::row::AgentRequestRow;

pub(super) struct ActorScan<F> {
    pub(super) actor: String,
    pub(super) outcome: Result<Vec<AgentRequestRow>, F>,
}

#[derive(Debug)]
pub(super) enum ScopedScanCause<F> {
    MissingScan,
    Failed(F),
}

#[derive(Debug)]
pub(super) struct ScopedScanFailure<F> {
    pub(super) actor: String,
    pub(super) cause: ScopedScanCause<F>,
}

#[derive(Debug)]
pub(super) struct ScopedScanBatch<F> {
    pub(super) rows: Vec<AgentRequestRow>,
    pub(super) failures: Vec<ScopedScanFailure<F>>,
}

pub(super) fn needs_rescan<F>(failures: &[F]) -> bool {
    !failures.is_empty()
}

pub(super) fn merge_scoped_scans<'a, A, F>(
    actors: A,
    disabled: &RapidHashSet<String>,
    scans: Vec<ActorScan<F>>,
    positions: &RapidHashMap<String, u64>,
) -> ScopedScanBatch<F>
where
    A: IntoIterator<Item = &'a str>,
{
    let mut outcomes = RapidHashMap::with_capacity(scans.len());
    for scan in scans {
        outcomes.entry(scan.actor).or_insert(scan.outcome);
    }
    let mut merged = RapidHashSet::new();
    let mut seen = RapidHashSet::new();
    let mut rows = Vec::new();
    let mut failures = Vec::new();
    for actor in actors {
        if !merged.insert(actor) || disabled.contains(actor) {
            continue;
        }
        let cause = match outcomes.remove(actor) {
            None => ScopedScanCause::MissingScan,
            Some(Err(error)) => ScopedScanCause::Failed(error),
            Some(Ok(observed)) => {
                rows.extend(observed.into_iter().filter(|row| {
                    row.agent_did.as_deref() == Some(actor)
                        && row
                            .doc_id
                            .as_ref()
                            .is_some_and(|doc| seen.insert(doc.clone()))
                }));
                continue;
            }
        };
        failures.push(ScopedScanFailure {
            actor: actor.to_owned(),
            cause,
        });
    }
    rows.sort_by(|a, b| {
        (a.created_at.as_deref(), &a.request_id).cmp(&(b.created_at.as_deref(), &b.request_id))
    });
    rows.sort_by_key(|row| {
        row.doc_id
            .as_ref()
            .and_then(|id| positions.get(id).copied())
    });
    ScopedScanBatch { rows, failures }
}

#[cfg(test)]
mod tests {
    use rapidhash::{HashMapExt, RapidHashMap, RapidHashSet};

    use gents_lean_vocab::families::watcher_scoped_scan_batch::{
        BatchOutput, ScopedFailure, ScopedRow,
    };
    use gents_protocol::row::AgentRequestRow;

    use super::{merge_scoped_scans, needs_rescan, ActorScan, ScopedScanCause};

    #[test]
    fn scoped_scan_merge_follows_the_lean_model() {
        gents_lean_vocab::families::watcher_scoped_scan_batch::replay::<String>(|input| {
            let legacy_rank = |doc: &str| {
                input
                    .legacy_order
                    .iter()
                    .position(|id| id == doc)
                    .unwrap_or(input.legacy_order.len())
            };
            let scans = input
                .scans
                .iter()
                .map(|scan| ActorScan {
                    actor: scan.actor.clone(),
                    outcome: match scan.status.as_str() {
                        "ok" => Ok(scan
                            .rows
                            .iter()
                            .map(|row| AgentRequestRow {
                                doc_id: Some(row.doc_id.clone()),
                                agent_did: Some(row.selected_agent.clone()),
                                created_at: Some(format!("{:020}", legacy_rank(&row.doc_id))),
                                ..AgentRequestRow::default()
                            })
                            .collect()),
                        status => Err(status.to_owned()),
                    },
                })
                .collect();
            let mut positions = RapidHashMap::new();
            for (position, doc) in input.arrival_order.iter().enumerate() {
                positions.entry(doc.clone()).or_insert(position as u64);
            }
            let disabled = input
                .disabled_agents
                .iter()
                .cloned()
                .collect::<RapidHashSet<_>>();
            let batch = merge_scoped_scans(
                input.active_agents.iter().map(String::as_str),
                &disabled,
                scans,
                &positions,
            );
            let rescan = needs_rescan(&batch.failures);
            BatchOutput {
                rows: batch
                    .rows
                    .into_iter()
                    .map(|row| ScopedRow {
                        doc_id: row.doc_id.unwrap_or_default(),
                        selected_agent: row.agent_did.unwrap_or_default(),
                    })
                    .collect(),
                failures: batch
                    .failures
                    .into_iter()
                    .map(|failure| ScopedFailure {
                        actor: failure.actor,
                        reason: match failure.cause {
                            ScopedScanCause::MissingScan => "missing_scan".to_owned(),
                            ScopedScanCause::Failed(status) => status,
                        },
                    })
                    .collect(),
                needs_rescan: rescan,
            }
        });
    }
}
