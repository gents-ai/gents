use eframe::egui::{self, Align2, Color32, Response, RichText, Sense, Ui};

use crate::theme;

pub(crate) fn sidebar_heading(ui: &mut Ui, title: &str, action: Option<&str>) {
    let palette = theme::palette();

    ui.horizontal(|ui| {
        ui.label(
            RichText::new(title)
                .family(theme::stencil_family())
                .size(13.0)
                .color(palette.text_1)
                .strong(),
        );

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if let Some(action) = action {
                ui.label(
                    RichText::new(action)
                        .monospace()
                        .size(10.5)
                        .color(palette.text_2),
                );
            }
        });
    });
}

pub(crate) fn section_kicker(ui: &mut Ui, title: &str) {
    let palette = theme::palette();
    ui.label(
        RichText::new(title)
            .monospace()
            .size(10.5)
            .color(palette.text_3)
            .strong(),
    );
}

pub(crate) fn side_row(
    ui: &mut Ui,
    title: &str,
    meta: &str,
    selected: bool,
    dot_color: Color32,
    accessory: Option<&str>,
) -> Response {
    let palette = theme::palette();
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 42.0), Sense::click());
    let clipped_painter = ui
        .painter()
        .with_clip_rect(rect.shrink2(egui::vec2(6.0, 2.0)));

    let fill = if selected {
        palette.background_2
    } else if response.hovered() {
        palette.background_1
    } else {
        Color32::TRANSPARENT
    };

    if fill != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 3.0, fill);
    }

    if selected {
        ui.painter().line_segment(
            [rect.left_top(), rect.left_bottom()],
            egui::Stroke::new(2.0, palette.accent),
        );
    }

    let dot_center = egui::pos2(rect.left() + 12.0, rect.center().y);
    clipped_painter.circle_filled(dot_center, 3.5, dot_color);

    let text_left = rect.left() + 24.0;
    let text_right = rect.right() - if accessory.is_some() { 56.0 } else { 10.0 };
    let text_clip_rect = egui::Rect::from_min_max(
        egui::pos2(text_left, rect.top()),
        egui::pos2(text_right.max(text_left), rect.bottom()),
    );
    let text_painter = ui.painter().with_clip_rect(text_clip_rect);

    text_painter.text(
        egui::pos2(text_left, rect.top() + 10.0),
        Align2::LEFT_TOP,
        title,
        egui::FontId::new(13.0, egui::FontFamily::Proportional),
        palette.text_0,
    );
    text_painter.text(
        egui::pos2(text_left, rect.top() + 25.0),
        Align2::LEFT_TOP,
        meta,
        egui::FontId::new(10.5, egui::FontFamily::Monospace),
        palette.text_2,
    );

    if let Some(accessory) = accessory {
        clipped_painter.text(
            egui::pos2(rect.right() - 10.0, rect.center().y - 1.0),
            Align2::RIGHT_CENTER,
            accessory,
            egui::FontId::new(10.5, egui::FontFamily::Monospace),
            palette.text_2,
        );
    }

    response
}

pub(crate) fn card(ui: &mut Ui, title: &str, body: &str) {
    let palette = theme::palette();

    ui.group(|ui| {
        ui.set_width(ui.available_width());
        ui.label(
            RichText::new(title)
                .family(theme::stencil_family())
                .size(12.5)
                .color(palette.text_1)
                .strong(),
        );
        ui.add_space(6.0);
        ui.label(
            RichText::new(body)
                .size(13.0)
                .color(palette.text_1)
                .line_height(Some(18.0)),
        );
    });
}

pub(crate) fn toolbar(ui: &mut Ui, title: &str, breadcrumb: &str, badge: &str) {
    let palette = theme::palette();
    let metrics = theme::metrics();

    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), metrics.toolbar_height),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.label(
                RichText::new(title)
                    .family(theme::stencil_family())
                    .size(18.0)
                    .color(palette.text_0)
                    .strong(),
            );
            ui.add_space(8.0);
            ui.label(
                RichText::new(breadcrumb)
                    .monospace()
                    .size(11.5)
                    .color(palette.text_2),
            );
            if !badge.trim().is_empty() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(badge)
                            .monospace()
                            .size(10.5)
                            .color(palette.text_1),
                    );
                });
            }
        },
    );
}
