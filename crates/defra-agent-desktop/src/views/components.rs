use eframe::egui::{Color32, Response, RichText, Ui};

use crate::theme;
use crate::views;

pub(crate) fn inset_list_item(
    ui: &mut Ui,
    title: &str,
    meta: &str,
    selected: bool,
    dot_color: Color32,
    accessory: Option<&str>,
) -> Response {
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        views::side_row(ui, title, meta, selected, dot_color, accessory)
    })
    .inner
}

pub(crate) fn focus_panel(
    ui: &mut Ui,
    eyebrow: Option<&str>,
    title: &str,
    body: &str,
    add_actions: impl FnOnce(&mut Ui),
) {
    let palette = theme::palette();

    ui.group(|ui| {
        ui.set_width(ui.available_width());
        if let Some(eyebrow) = eyebrow.filter(|eyebrow| !eyebrow.trim().is_empty()) {
            ui.label(
                RichText::new(eyebrow)
                    .monospace()
                    .size(10.5)
                    .color(palette.text_2)
                    .strong(),
            );
            ui.add_space(6.0);
        }
        ui.label(
            RichText::new(title)
                .family(theme::stencil_family())
                .size(18.0)
                .color(palette.text_0)
                .strong(),
        );
        ui.add_space(8.0);
        ui.label(
            RichText::new(body)
                .size(13.0)
                .color(palette.text_1)
                .line_height(Some(18.0)),
        );
        ui.add_space(12.0);
        add_actions(ui);
    });
}

pub(crate) fn info_strip(ui: &mut Ui, values: &[&str]) {
    let palette = theme::palette();

    ui.horizontal_wrapped(|ui| {
        for value in values.iter().filter(|value| !value.trim().is_empty()) {
            ui.label(
                RichText::new(*value)
                    .monospace()
                    .size(10.5)
                    .color(palette.text_2),
            );
        }
    });
}
