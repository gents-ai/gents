use eframe::egui::{self, Align2, RichText, Sense, Ui};

use crate::audit;
use crate::state::{Activity, PendingShellAction, ShellState};
use crate::theme;

pub(super) fn show_sidebar_chrome(ui: &mut Ui, state: &mut ShellState) {
    ui.add_space(12.0);
    render_shell_identity(ui, state);
    ui.add_space(6.0);
    for activity in Activity::ALL {
        render_activity_button(ui, state, activity);
        ui.add_space(4.0);
    }
    ui.add_space(8.0);
    ui.separator();
    ui.add_space(6.0);
}

fn render_shell_identity(ui: &mut Ui, state: &ShellState) {
    let palette = theme::palette();

    ui.group(|ui| {
        ui.set_width(ui.available_width());
        ui.label(
            RichText::new("DEFRA DESKTOP")
                .family(theme::stencil_family())
                .size(14.0)
                .color(palette.accent)
                .strong(),
        );
        ui.add_space(4.0);
        ui.label(
            RichText::new(state.identity.did_short.as_str())
                .monospace()
                .size(10.5)
                .color(palette.text_1),
        );
    });
}

fn render_activity_button(ui: &mut Ui, state: &mut ShellState, activity: Activity) {
    let palette = theme::palette();
    let selected = state.activity == activity;
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 42.0), Sense::click());
    let fill = if selected {
        palette.background_2
    } else if response.hovered() {
        palette.background_1
    } else {
        palette.background_0
    };
    ui.painter().rect(
        rect,
        6.0,
        fill,
        egui::Stroke::new(
            1.0,
            if selected {
                palette.accent_dim
            } else {
                palette.stroke_subtle
            },
        ),
        egui::StrokeKind::Inside,
    );

    if selected {
        ui.painter().line_segment(
            [
                egui::pos2(rect.left(), rect.top()),
                egui::pos2(rect.left(), rect.bottom()),
            ],
            egui::Stroke::new(2.0, palette.accent),
        );
    }

    ui.painter().text(
        egui::pos2(rect.left() + 14.0, rect.top() + 11.0),
        Align2::LEFT_TOP,
        activity.label().to_ascii_uppercase(),
        egui::FontId::new(14.5, theme::stencil_family()),
        if selected {
            palette.text_0
        } else {
            palette.text_1
        },
    );
    let response = response.on_hover_text(activity.label());
    audit::record(ui, &audit::targets::activity(activity), &response);
    if response.clicked() {
        state.queue_shell_action(PendingShellAction::Navigate(activity));
    }
}
