use chrono::{DateTime, Utc};
use gents::P2pSyncStatusSnapshot;

use super::{format_metric_labels, rfc3339_timestamp};

/// Per-peer families are labelled by peer, so a node that remembers many
/// historical peers would otherwise grow scrape cardinality without bound.
const MAX_PEER_SERIES: usize = 256;
const SHORT_PEER_ID_CHARS: usize = 8;

/// Both labels identify the same peer: `peer_id` matches the existing
/// `gents_p2p_peer_push_*` series; `peer` is the short form operator
/// dashboards join on.
fn peer_labels(peer_id: &str) -> [(&'static str, String); 2] {
    [
        ("peer_id", peer_id.to_string()),
        ("peer", short_peer_id(peer_id).to_string()),
    ]
}

pub(super) fn short_peer_id(peer_id: &str) -> &str {
    peer_id
        .char_indices()
        .nth(SHORT_PEER_ID_CHARS)
        .map_or(peer_id, |(end, _)| &peer_id[..end])
}

pub(super) fn render_self_info(lines: &mut Vec<String>, peer_id: Option<&str>, transport: &str) {
    let mut f = Families::default();
    f.gauge(
        "gents_p2p_self_info",
        "Constant 1 labelled with this node's P2P peer id and transport.",
    );
    if let Some(peer_id) = peer_id.filter(|peer_id| !peer_id.is_empty()) {
        let [full, short] = peer_labels(peer_id);
        f.sample(
            "gents_p2p_self_info",
            &[full, short, ("transport", transport.to_string())],
            1,
        );
    }
    f.flush(lines);
}

struct Family {
    name: &'static str,
    help: &'static str,
    kind: &'static str,
    samples: Vec<String>,
}

/// The text exposition format requires each family's samples to follow its
/// HELP and TYPE contiguously; collecting per family keeps that true however
/// the snapshot is walked.
#[derive(Default)]
struct Families(Vec<Family>);

impl Families {
    fn declare(&mut self, name: &'static str, help: &'static str, kind: &'static str) {
        self.0.push(Family {
            name,
            help,
            kind,
            samples: Vec::new(),
        });
    }

    fn gauge(&mut self, name: &'static str, help: &'static str) {
        self.declare(name, help, "gauge");
    }

    fn counter(&mut self, name: &'static str, help: &'static str) {
        self.declare(name, help, "counter");
    }

    fn sample(&mut self, name: &str, labels: &[(&str, String)], value: impl std::fmt::Display) {
        let family = self
            .0
            .iter_mut()
            .find(|family| family.name == name)
            .unwrap_or_else(|| panic!("metric family {name} is sampled before it is declared"));
        family
            .samples
            .push(format!("{name}{} {value}", format_metric_labels(labels)));
    }

    fn flush(self, lines: &mut Vec<String>) {
        for family in self.0 {
            lines.push(format!("# HELP {} {}", family.name, family.help));
            lines.push(format!("# TYPE {} {}", family.name, family.kind));
            lines.extend(family.samples);
        }
    }
}

/// Families render HELP and TYPE unconditionally; samples render only when the
/// live snapshot decoded, so an unavailable snapshot reads as absent rather
/// than as counters reset to zero.
pub(super) fn render_sync_diagnostics(
    lines: &mut Vec<String>,
    status: Option<&P2pSyncStatusSnapshot>,
    now: DateTime<Utc>,
) {
    let mut families = Families::default();
    render_request_dispatch(&mut families, status);
    render_pending_dag(&mut families, status);
    render_car_and_head_hints(&mut families, status);
    render_inbound_budget(&mut families, status);
    render_broadcast_and_retry_markers(&mut families, status);
    render_peer_fetches(&mut families, status);
    render_peer_liveness(&mut families, status, now);
    families.flush(lines);
}

fn render_request_dispatch(f: &mut Families, status: Option<&P2pSyncStatusSnapshot>) {
    let gauges = [
        (
            "gents_p2p_request_dispatch_capacity",
            "Inbound P2P request dispatch slots per lane.",
        ),
        (
            "gents_p2p_request_dispatch_active",
            "Inbound P2P requests currently dispatched per lane.",
        ),
        (
            "gents_p2p_request_dispatch_high_water",
            "Highest concurrently dispatched inbound P2P requests per lane since start.",
        ),
    ];
    for (name, help) in gauges {
        f.gauge(name, help);
    }
    f.counter("gents_p2p_request_dispatch_saturated_total", "Inbound P2P requests that found every dispatch slot taken; lane=\"any\" counts all lanes, lane=\"recovery\" the recovery lane alone.");
    f.counter(
        "gents_p2p_request_dispatch_rejection_dropped_total",
        "Inbound P2P rejections dropped because the rejection lane was full.",
    );
    let Some(status) = status else {
        return;
    };
    let dispatch = &status.request_dispatch;
    for (lane, capacity, active, high_water) in [
        (
            "request",
            dispatch.request_capacity,
            dispatch.active_requests,
            dispatch.active_requests_high_water,
        ),
        (
            "recovery",
            dispatch.recovery_capacity,
            dispatch.active_recovery,
            dispatch.active_recovery_high_water,
        ),
        (
            "rejection",
            dispatch.rejection_capacity,
            dispatch.active_rejections,
            dispatch.active_rejections_high_water,
        ),
        (
            "completion",
            dispatch.completion_capacity,
            dispatch.active_completions,
            dispatch.active_completions_high_water,
        ),
    ] {
        let labels = &[("lane", lane.to_string())];
        f.sample(gauges[0].0, labels, capacity);
        f.sample(gauges[1].0, labels, active);
        f.sample(gauges[2].0, labels, high_water);
    }
    for (lane, value) in [
        ("any", dispatch.saturated_total),
        ("recovery", dispatch.recovery_saturated_total),
    ] {
        f.sample(
            "gents_p2p_request_dispatch_saturated_total",
            &[("lane", lane.to_string())],
            value,
        );
    }
    f.sample(
        "gents_p2p_request_dispatch_rejection_dropped_total",
        &[],
        dispatch.rejection_dropped_total,
    );
}

/// A family name, its help text and the snapshot field it samples.
type FieldFamily = (
    &'static str,
    &'static str,
    fn(&P2pSyncStatusSnapshot) -> u64,
);

fn render_pending_dag(f: &mut Families, status: Option<&P2pSyncStatusSnapshot>) {
    let counters: [FieldFamily; 6] = [
        (
            "gents_p2p_pending_dag_registered_total",
            "Pending DAG roots registered while missing links are fetched.",
            |s| s.pending_dag_registered,
        ),
        (
            "gents_p2p_pending_dag_fetch_exhausted_total",
            "Pending DAG fetches that failed after exhausting retries and providers.",
            |s| s.pending_dag_fetch_exhausted,
        ),
        (
            "gents_p2p_pending_dag_terminal_merged_total",
            "Pending DAG roots that reached a terminal merge.",
            |s| s.pending_dag_terminal_merged,
        ),
        (
            "gents_p2p_pending_dag_retry_dispatched_total",
            "Pending DAG fetch retries dispatched.",
            |s| s.pending_dag_retry_dispatched,
        ),
        (
            "gents_p2p_pending_dag_retry_suppressed_total",
            "Pending DAG fetch retries suppressed.",
            |s| s.pending_dag_retry_suppressed,
        ),
        (
            "gents_p2p_provider_rotations_total",
            "Times a DAG fetch rotated to another provider.",
            |s| s.provider_rotations,
        ),
    ];
    for (name, help, _) in &counters {
        f.counter(name, help);
    }
    let gauges: [FieldFamily; 2] = [
        (
            "gents_p2p_pending_dag_high_water",
            "Highest in-memory pending DAG registrations since start.",
            |s| s.pending_dag_high_water,
        ),
        (
            "gents_p2p_persisted_pending_dag_high_water",
            "Highest durable pending DAG registrations since start.",
            |s| s.persisted_pending_dag_high_water,
        ),
    ];
    for (name, help, _) in &gauges {
        f.gauge(name, help);
    }
    f.gauge(
        "gents_p2p_next_pending_retry_seconds",
        "Seconds until the next scheduled pending DAG retry; absent when none is scheduled.",
    );
    let Some(status) = status else {
        return;
    };
    for (name, _, read) in counters.iter().chain(gauges.iter()) {
        f.sample(name, &[], read(status));
    }
    if let Some(millis) = status.next_pending_retry_in_ms {
        f.sample(
            "gents_p2p_next_pending_retry_seconds",
            &[],
            millis as f64 / 1_000.0,
        );
    }
}

fn render_car_and_head_hints(f: &mut Families, status: Option<&P2pSyncStatusSnapshot>) {
    f.counter(
        "gents_p2p_car_cids_total",
        "CIDs handled by the CAR block server, by outcome.",
    );
    f.counter("gents_p2p_head_hints_total", "Outbound head hints by outcome; scope is document or collection where DefraDB splits it, otherwise all.");
    let Some(status) = status else {
        return;
    };
    for (outcome, value) in [
        ("requested", status.car_requested_cids),
        ("present", status.car_present_cids),
        ("served", status.car_served_cids),
        ("filtered", status.car_filtered_cids),
    ] {
        f.sample(
            "gents_p2p_car_cids_total",
            &[("outcome", outcome.to_string())],
            value,
        );
    }
    let backlog = &status.push_backlog;
    for (scope, outcome, value) in [
        ("document", "enqueued", backlog.head_hints_enqueued_document),
        (
            "collection",
            "enqueued",
            backlog.head_hints_enqueued_collection,
        ),
        ("document", "sent", backlog.head_hints_sent_document),
        ("collection", "sent", backlog.head_hints_sent_collection),
        ("document", "acked", backlog.head_hints_acked_document),
        ("collection", "acked", backlog.head_hints_acked_collection),
        ("all", "nacked_capacity", backlog.head_hints_nacked_capacity),
        ("all", "nacked_other", backlog.head_hints_nacked_other),
        (
            "all",
            "failed_transport",
            backlog.head_hints_failed_transport,
        ),
        ("all", "failed_local", backlog.head_hints_failed_local),
    ] {
        f.sample(
            "gents_p2p_head_hints_total",
            &[
                ("outcome", outcome.to_string()),
                ("scope", scope.to_string()),
            ],
            value,
        );
    }
}

fn render_inbound_budget(f: &mut Families, status: Option<&P2pSyncStatusSnapshot>) {
    for (name, help) in [
        (
            "gents_p2p_inbound_budget_capacity_bytes",
            "Inbound P2P byte budget per request class.",
        ),
        (
            "gents_p2p_inbound_budget_held_bytes",
            "Inbound P2P bytes currently held per request class.",
        ),
        (
            "gents_p2p_inbound_budget_held_high_water_bytes",
            "Highest inbound P2P bytes held per request class since start.",
        ),
    ] {
        f.gauge(name, help);
    }
    f.counter(
        "gents_p2p_inbound_budget_resets_total",
        "Inbound P2P streams reset by the byte budget, by class and reason.",
    );
    let Some(status) = status else {
        return;
    };
    for class in &status.inbound_budget {
        let labels = &[("class", class.class.clone())];
        f.sample(
            "gents_p2p_inbound_budget_capacity_bytes",
            labels,
            class.capacity_bytes,
        );
        f.sample(
            "gents_p2p_inbound_budget_held_bytes",
            labels,
            class.held_bytes,
        );
        f.sample(
            "gents_p2p_inbound_budget_held_high_water_bytes",
            labels,
            class.held_bytes_high_water,
        );
        for (reason, value) in [
            ("busy", class.busy_resets),
            ("oversized", class.oversized_resets),
        ] {
            f.sample(
                "gents_p2p_inbound_budget_resets_total",
                &[
                    ("class", class.class.clone()),
                    ("reason", reason.to_string()),
                ],
                value,
            );
        }
    }
}

fn render_broadcast_and_retry_markers(f: &mut Families, status: Option<&P2pSyncStatusSnapshot>) {
    for (name, help) in [
        (
            "gents_p2p_non_authoritative_broadcast_tasks",
            "Live non-authoritative gossip broadcast tasks.",
        ),
        (
            "gents_p2p_non_authoritative_broadcast_high_water",
            "Highest live non-authoritative gossip broadcast tasks since start.",
        ),
        (
            "gents_p2p_push_retry_markers",
            "Durable outbound push retry markers by scope.",
        ),
        (
            "gents_p2p_push_retry_scheduled_peers",
            "Peers with a scheduled outbound push retry.",
        ),
        (
            "gents_p2p_push_retry_oldest_scheduled_timestamp_seconds",
            "Unix time of the oldest scheduled outbound push retry; absent when none is scheduled.",
        ),
    ] {
        f.gauge(name, help);
    }
    f.counter(
        "gents_p2p_non_authoritative_broadcast_rejected_total",
        "Non-authoritative gossip broadcasts rejected at the task cap.",
    );
    let Some(status) = status else {
        return;
    };
    f.sample(
        "gents_p2p_non_authoritative_broadcast_tasks",
        &[],
        status.non_authoritative_broadcast_tasks,
    );
    f.sample(
        "gents_p2p_non_authoritative_broadcast_high_water",
        &[],
        status.non_authoritative_broadcast_high_water,
    );
    f.sample(
        "gents_p2p_non_authoritative_broadcast_rejected_total",
        &[],
        status.non_authoritative_broadcast_rejected_total,
    );
    let markers = &status.push_retry_markers;
    for (scope, value) in [
        ("document", markers.document_markers),
        ("collection", markers.collection_markers),
    ] {
        f.sample(
            "gents_p2p_push_retry_markers",
            &[("scope", scope.to_string())],
            value,
        );
    }
    f.sample(
        "gents_p2p_push_retry_scheduled_peers",
        &[],
        markers.scheduled_peers,
    );
    if let Some(unix) = markers.oldest_scheduled_retry_unix {
        f.sample(
            "gents_p2p_push_retry_oldest_scheduled_timestamp_seconds",
            &[],
            unix,
        );
    }
}

fn push_peer_series_dropped(f: &mut Families, family: &str, total: usize) {
    f.sample(
        "gents_p2p_peer_series_dropped",
        &[("family", family.to_string())],
        total.saturating_sub(MAX_PEER_SERIES),
    );
}

/// Keeps the lexicographically first `MAX_PEER_SERIES` peers so the retained
/// set is stable across scrapes.
fn bounded_peers<T, F>(rows: &[T], peer_id: F) -> Vec<&T>
where
    F: Fn(&T) -> &str,
{
    let mut rows = rows
        .iter()
        .filter(|row| !peer_id(row).is_empty())
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| peer_id(left).cmp(peer_id(right)));
    rows.truncate(MAX_PEER_SERIES);
    rows
}

fn render_peer_fetches(f: &mut Families, status: Option<&P2pSyncStatusSnapshot>) {
    f.gauge(
        "gents_p2p_peer_series_dropped",
        "Peers omitted from a per-peer family by the series cap.",
    );
    for (name, help, kind) in [
        (
            "gents_p2p_peer_fetch_total",
            "Block fetches issued to one peer.",
            "counter",
        ),
        (
            "gents_p2p_peer_fetch_bytes_total",
            "Bytes fetched from one peer.",
            "counter",
        ),
        (
            "gents_p2p_peer_fetch_success_ppm",
            "Recent fetch success rate from one peer, in parts per million.",
            "gauge",
        ),
        (
            "gents_p2p_peer_fetch_timeout_ppm",
            "Recent fetch timeout rate from one peer, in parts per million.",
            "gauge",
        ),
        (
            "gents_p2p_peer_fetch_partial_ppm",
            "Recent partial-fetch rate from one peer, in parts per million.",
            "gauge",
        ),
        (
            "gents_p2p_peer_fetch_min_rtt_seconds",
            "Minimum observed fetch round trip to one peer; absent until measured.",
            "gauge",
        ),
        (
            "gents_p2p_peer_fetch_delivery_rate_bytes_per_second",
            "Estimated fetch delivery rate from one peer.",
            "gauge",
        ),
        (
            "gents_p2p_peer_fetch_in_flight",
            "Fetches currently in flight to one peer.",
            "gauge",
        ),
        (
            "gents_p2p_peer_fetch_in_flight_bytes",
            "Bytes currently in flight from one peer.",
            "gauge",
        ),
    ] {
        f.declare(name, help, kind);
    }
    let Some(status) = status else {
        return;
    };
    push_peer_series_dropped(f, "fetch", status.peer_fetches.len());
    for fetch in bounded_peers(&status.peer_fetches, |row| row.peer.as_str()) {
        let labels = &peer_labels(&fetch.peer);
        for (name, value) in [
            ("gents_p2p_peer_fetch_total", fetch.fetches),
            ("gents_p2p_peer_fetch_bytes_total", fetch.bytes),
            ("gents_p2p_peer_fetch_success_ppm", fetch.success_ppm),
            ("gents_p2p_peer_fetch_timeout_ppm", fetch.timeout_ppm),
            ("gents_p2p_peer_fetch_partial_ppm", fetch.partial_ppm),
            (
                "gents_p2p_peer_fetch_delivery_rate_bytes_per_second",
                fetch.delivery_rate_bytes_per_sec,
            ),
            ("gents_p2p_peer_fetch_in_flight", fetch.in_flight),
            (
                "gents_p2p_peer_fetch_in_flight_bytes",
                fetch.in_flight_bytes,
            ),
        ] {
            f.sample(name, labels, value);
        }
        if let Some(micros) = fetch.min_rtt_micros {
            f.sample(
                "gents_p2p_peer_fetch_min_rtt_seconds",
                labels,
                micros as f64 / 1_000_000.0,
            );
        }
    }
}

fn render_peer_liveness(
    f: &mut Families,
    status: Option<&P2pSyncStatusSnapshot>,
    now: DateTime<Utc>,
) {
    f.gauge(
        "gents_p2p_peer_connected",
        "1 when the sync layer reports the peer connected.",
    );
    for (name, help) in [
        (
            "gents_p2p_peer_last_seen_age_seconds",
            "Seconds since the peer was last seen.",
        ),
        (
            "gents_p2p_peer_last_block_received_age_seconds",
            "Seconds since a block was last received from the peer; absent if never.",
        ),
        (
            "gents_p2p_peer_last_head_acked_age_seconds",
            "Seconds since the peer last acknowledged a head; absent if never.",
        ),
    ] {
        f.gauge(name, help);
    }
    let Some(status) = status else {
        return;
    };
    push_peer_series_dropped(f, "peer", status.peers.len());
    for peer in bounded_peers(&status.peers, |row| row.peer_id.as_str()) {
        let labels = &peer_labels(&peer.peer_id);
        f.sample(
            "gents_p2p_peer_connected",
            labels,
            i64::from(peer.connected),
        );
        for (name, stamp) in [
            (
                "gents_p2p_peer_last_seen_age_seconds",
                Some(peer.last_seen_at.as_str()),
            ),
            (
                "gents_p2p_peer_last_block_received_age_seconds",
                peer.last_block_received_at.as_deref(),
            ),
            (
                "gents_p2p_peer_last_head_acked_age_seconds",
                peer.last_head_acked_at.as_deref(),
            ),
        ] {
            if let Some(at) = stamp.and_then(rfc3339_timestamp) {
                let age = (now - at).num_milliseconds().max(0) as f64 / 1_000.0;
                f.sample(name, labels, age);
            }
        }
    }
}
