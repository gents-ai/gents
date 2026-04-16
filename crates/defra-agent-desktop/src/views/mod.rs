pub mod chat;
pub mod logs;
pub mod operator;
pub mod peers;

use eframe::egui::{self, Align2, Color32, Response, RichText, Sense, Ui};
use egui_commonmark::CommonMarkCache;

use crate::audit;
use crate::client::{ClientCore, ClientStore};
use crate::state::{Activity, PendingShellAction, ShellState};
use crate::telemetry::DesktopLogStore;
use crate::theme;

pub fn prepare_state(
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
) {
    match state.activity {
        Activity::Chat => chat::prepare_state(state, client, store),
        Activity::Operator => operator::prepare_state(state, client, store),
        Activity::Peers => peers::prepare_state(state, client, store),
        Activity::Logs => {}
    }
}

pub fn show_sidebar(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
    runtime: &tokio::runtime::Runtime,
) {
    ui.add_space(12.0);
    render_shell_identity(ui, state);
    ui.add_space(10.0);
    section_kicker(ui, "NAVIGATION");
    ui.add_space(6.0);
    for activity in Activity::ALL {
        render_activity_button(ui, state, activity);
        ui.add_space(4.0);
    }
    ui.add_space(8.0);
    ui.separator();
    ui.add_space(6.0);

    match state.activity {
        Activity::Chat => chat::show_sidebar(ui, state, client, store),
        Activity::Operator => operator::show_sidebar(ui, state, client, store),
        Activity::Peers => peers::show_sidebar(ui, state, client, store, runtime),
        Activity::Logs => logs::show_sidebar(ui, state),
    }
}

pub fn show_main(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
    log_store: &DesktopLogStore,
    runtime: &tokio::runtime::Runtime,
    markdown_cache: &mut CommonMarkCache,
) {
    match state.activity {
        Activity::Chat => chat::show_main(ui, state, client, store, markdown_cache),
        Activity::Operator => operator::show_main(ui, state, store),
        Activity::Peers => peers::show_main(ui, state, client, store, runtime),
        Activity::Logs => logs::show_main(ui, state, log_store),
    }
}

pub fn show_rail(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
    log_store: &DesktopLogStore,
    runtime: &tokio::runtime::Runtime,
) {
    match state.activity {
        Activity::Chat => {}
        Activity::Operator => operator::show_rail(ui, state, client, store, runtime),
        Activity::Peers => peers::show_rail(ui, state, client, store, runtime),
        Activity::Logs => logs::show_rail(ui, client, store, log_store),
    }
}

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
            RichText::new(state.identity.label.as_str())
                .monospace()
                .size(10.5)
                .color(palette.text_2),
        );
        ui.label(
            RichText::new(state.identity.did_short.as_str())
                .monospace()
                .size(10.5)
                .color(palette.text_1),
        );
        ui.add_space(6.0);
        ui.label(
            RichText::new(format!(
                "peers {}/{}  ·  {}",
                state.status.peered_now, state.status.peered_target, state.status.replication_state
            ))
            .monospace()
            .size(10.5)
            .color(palette.text_2),
        );
    });
}

fn render_activity_button(ui: &mut Ui, state: &mut ShellState, activity: Activity) {
    let palette = theme::palette();
    let selected = state.activity == activity;
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 52.0), Sense::click());
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

    let badge_rect = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 10.0, rect.center().y - 15.0),
        egui::vec2(30.0, 30.0),
    );
    ui.painter().rect(
        badge_rect,
        5.0,
        if selected {
            palette.accent_dim
        } else {
            palette.background_1
        },
        egui::Stroke::new(
            1.0,
            if selected {
                palette.accent
            } else {
                palette.stroke
            },
        ),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        badge_rect.center(),
        Align2::CENTER_CENTER,
        activity.nav_badge(),
        egui::FontId::new(11.0, egui::FontFamily::Monospace),
        if selected {
            palette.accent
        } else {
            palette.text_2
        },
    );

    ui.painter().text(
        egui::pos2(badge_rect.right() + 10.0, rect.top() + 12.0),
        Align2::LEFT_TOP,
        activity.label().to_ascii_uppercase(),
        egui::FontId::new(14.5, theme::stencil_family()),
        if selected {
            palette.text_0
        } else {
            palette.text_1
        },
    );
    ui.painter().text(
        egui::pos2(rect.right() - 10.0, rect.center().y),
        Align2::RIGHT_CENTER,
        activity.nav_hint(),
        egui::FontId::new(10.5, egui::FontFamily::Monospace),
        if selected {
            palette.text_2
        } else {
            palette.text_3
        },
    );

    let response = response.on_hover_text(activity.label());
    audit::record(ui, &audit::targets::activity(activity), &response);
    if response.clicked() {
        state.queue_shell_action(PendingShellAction::Navigate(activity));
    }
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
    ui.painter().circle_filled(dot_center, 3.5, dot_color);

    let text_left = rect.left() + 24.0;
    ui.painter().text(
        egui::pos2(text_left, rect.top() + 10.0),
        Align2::LEFT_TOP,
        title,
        egui::FontId::new(13.0, egui::FontFamily::Proportional),
        palette.text_0,
    );
    ui.painter().text(
        egui::pos2(text_left, rect.top() + 25.0),
        Align2::LEFT_TOP,
        meta,
        egui::FontId::new(10.5, egui::FontFamily::Monospace),
        palette.text_2,
    );

    if let Some(accessory) = accessory {
        ui.painter().text(
            egui::pos2(rect.right() - 10.0, rect.center().y - 1.0),
            Align2::RIGHT_CENTER,
            accessory,
            egui::FontId::new(10.5, egui::FontFamily::Monospace),
            palette.text_2,
        );
    }

    response
}

pub(crate) fn tree_row(ui: &mut Ui, label: &str, tag: &str, selected: bool) -> Response {
    let palette = theme::palette();
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 30.0), Sense::click());

    if response.hovered() || selected {
        ui.painter().rect_filled(
            rect,
            2.0,
            if selected {
                palette.background_2
            } else {
                palette.background_1
            },
        );
    }

    if selected {
        ui.painter().line_segment(
            [rect.left_top(), rect.left_bottom()],
            egui::Stroke::new(2.0, palette.accent),
        );
    }

    let line_x = rect.left() + 15.0;
    ui.painter().line_segment(
        [
            egui::pos2(line_x, rect.top()),
            egui::pos2(line_x, rect.center().y),
        ],
        egui::Stroke::new(1.0, palette.stroke),
    );
    ui.painter().line_segment(
        [
            egui::pos2(line_x, rect.center().y),
            egui::pos2(line_x + 8.0, rect.center().y),
        ],
        egui::Stroke::new(1.0, palette.stroke),
    );
    ui.painter().text(
        egui::pos2(line_x + 14.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        egui::FontId::new(12.5, egui::FontFamily::Proportional),
        if selected {
            palette.text_0
        } else {
            palette.text_1
        },
    );
    ui.painter().text(
        egui::pos2(rect.right() - 8.0, rect.center().y),
        Align2::RIGHT_CENTER,
        tag,
        egui::FontId::new(9.5, egui::FontFamily::Monospace),
        if selected {
            palette.accent
        } else {
            palette.text_3
        },
    );

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
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(badge)
                        .monospace()
                        .size(10.5)
                        .color(palette.text_1),
                );
            });
        },
    );
}
