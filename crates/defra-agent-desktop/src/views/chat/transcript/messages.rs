use defra_agent_protocol::transcript::PresentedMessageRole;
use eframe::egui::{self, RichText, Ui};
use egui_commonmark::CommonMarkCache;

use crate::theme;

use super::markdown::render_markdown;

pub(super) fn message_label_color(role: PresentedMessageRole) -> egui::Color32 {
    match role {
        PresentedMessageRole::Assistant => theme::palette().accent,
        PresentedMessageRole::Tool => theme::palette().warning,
        PresentedMessageRole::User => theme::palette().text_1,
    }
}

pub(super) fn message_block(
    ui: &mut Ui,
    markdown_cache: &mut CommonMarkCache,
    markdown_id: impl std::hash::Hash,
    label: &str,
    label_color: egui::Color32,
    body: &str,
) {
    turn_block(ui, label, label_color, |ui| {
        render_markdown(ui, markdown_cache, markdown_id, body);
    });
}

fn turn_block(ui: &mut Ui, label: &str, label_color: egui::Color32, body: impl FnOnce(&mut Ui)) {
    let palette = theme::palette();

    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_width(66.0);
            ui.with_layout(egui::Layout::top_down_justified(egui::Align::Max), |ui| {
                ui.label(
                    RichText::new(label)
                        .monospace()
                        .size(10.5)
                        .color(label_color),
                );
            });
        });
        egui::Frame::new()
            .fill(palette.background_1)
            .stroke(egui::Stroke::new(1.0, palette.stroke_subtle))
            .corner_radius(4)
            .inner_margin(10)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.vertical(|ui| {
                    body(ui);
                });
            });
    });
}

pub(super) fn transcript_surface(ui: &mut Ui, body: impl FnOnce(&mut Ui)) {
    let palette = theme::palette();
    let available_height = ui.available_height();
    let expected_bounds = egui::Rect::from_min_size(ui.cursor().min, ui.available_size());
    let prev_clip = ui.clip_rect();
    ui.set_clip_rect(prev_clip.intersect(expected_bounds));
    egui::Frame::new()
        .fill(palette.background_0)
        .stroke(egui::Stroke::new(1.0, palette.stroke_subtle))
        .corner_radius(6)
        .inner_margin(14)
        .show(ui, |ui| {
            ui.set_min_height((available_height - 28.0).max(0.0));
            body(ui);
        });
    ui.set_clip_rect(prev_clip);
}

pub(super) fn centered_status_card(ui: &mut Ui, title: &str, body: &str) {
    let available = ui.available_size();
    ui.allocate_ui_with_layout(
        available,
        egui::Layout::top_down(egui::Align::Center),
        |ui| {
            ui.add_space((ui.available_height() * 0.22).max(24.0));
            ui.set_max_width(520.0);
            crate::views::card(ui, title, body);
        },
    );
}

pub(super) fn supporting_block(ui: &mut Ui, body: impl FnOnce(&mut Ui)) {
    let palette = theme::palette();

    ui.horizontal(|ui| {
        ui.add_space(66.0);
        egui::Frame::new()
            .fill(palette.background_0)
            .stroke(egui::Stroke::new(1.0, palette.stroke_subtle))
            .corner_radius(4)
            .inner_margin(8)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.vertical(|ui| {
                    body(ui);
                });
            });
    });
}
