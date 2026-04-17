use eframe::egui::{self, RichText, Ui};
use tracing::Level;

use crate::client::{ClientCore, ClientStore, P2PHealthStatus};
use crate::telemetry::DesktopLogStore;
use crate::theme;
use crate::views;

use super::entry::{format_timestamp, render_fields};

pub(super) fn show_rail(
    ui: &mut Ui,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
    log_store: &DesktopLogStore,
) {
    let palette = theme::palette();
    let snapshot = log_store.snapshot();
    let latest_warning = snapshot
        .entries
        .iter()
        .find(|entry| matches!(entry.level, Level::WARN | Level::ERROR))
        .cloned();
    let connected_peers = client
        .map(ClientCore::dialed_peer_count)
        .unwrap_or_default();
    let configured_peers = client
        .map(ClientCore::configured_peer_count)
        .unwrap_or_default();
    let p2p_health = client.map(ClientCore::p2p_health);

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(14.0);
        views::sidebar_heading(ui, "Diagnostics", Some("live only"));
        ui.add_space(10.0);
        views::card(
            ui,
            "Capture",
            "Tracing is mirrored into a local ring buffer for the Logs activity. History is memory-only in MVP and resets on restart.",
        );
        ui.add_space(10.0);
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new("rolling metrics")
                    .family(theme::stencil_family())
                    .size(13.0)
                    .color(palette.text_1)
                    .strong(),
            );
            ui.add_space(6.0);
            for row in [
                format!(
                    "approx store        {} / {} rows",
                    store
                        .map(|store| format_bytes(store.approx_serialized_bytes()))
                        .unwrap_or_else(|| "offline".to_string()),
                    store.map(ClientStore::row_count).unwrap_or_default()
                ),
                "replication lag     n/a (not instrumented yet)".to_string(),
                format!("peers               {connected_peers}/{configured_peers} connected"),
                format!(
                    "p2p transport       {}",
                    p2p_health
                        .as_ref()
                        .map(|health| health.status_label().to_string())
                        .unwrap_or_else(|| "offline".to_string())
                ),
                format!("events              {:.1}/s", snapshot.events_per_second),
                format!(
                    "buffer              {}/{} live ({} dropped)",
                    snapshot.entries.len(),
                    snapshot.capacity,
                    snapshot.dropped_events
                ),
            ] {
                ui.label(
                    RichText::new(row)
                        .monospace()
                        .size(11.0)
                        .color(palette.text_1),
                );
            }
        });

        if let Some(health) = p2p_health.as_ref() {
            ui.add_space(10.0);
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new("p2p health")
                        .family(theme::stencil_family())
                        .size(13.0)
                        .color(if health.status == P2PHealthStatus::Healthy {
                            palette.text_1
                        } else {
                            palette.warning
                        })
                        .strong(),
                );
                ui.add_space(6.0);
                for row in [
                    format!("status              {}", health.status_label()),
                    format!("connected peers     {}", health.connected_peer_count),
                    format!("replicators         {}", health.replicator_count),
                    format!("consecutive fails   {}", health.consecutive_failures),
                ] {
                    ui.label(
                        RichText::new(row)
                            .monospace()
                            .size(11.0)
                            .color(palette.text_1),
                    );
                }
                if let Some(error) = health.last_error.as_deref() {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(error)
                            .size(12.5)
                            .color(palette.text_1)
                            .line_height(Some(17.0)),
                    );
                }
            });
        }

        if let Some(entry) = latest_warning {
            ui.add_space(10.0);
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new("latest warning")
                        .family(theme::stencil_family())
                        .size(13.0)
                        .color(palette.warning)
                        .strong(),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!(
                        "{} · {}",
                        format_timestamp(entry.timestamp),
                        entry.target
                    ))
                    .monospace()
                    .size(10.5)
                    .color(palette.text_2),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(entry.message)
                        .size(12.5)
                        .color(palette.text_1)
                        .line_height(Some(17.0)),
                );
                if !entry.fields.is_empty() {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(render_fields(&entry.fields))
                            .monospace()
                            .size(10.5)
                            .color(palette.text_2),
                    );
                }
            });
        }
    });
}

pub(super) fn format_bytes(bytes: usize) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;

    let bytes = bytes as f64;
    if bytes >= MIB {
        format!("{:.1} MiB", bytes / MIB)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes / KIB)
    } else {
        format!("{bytes:.0} B")
    }
}
