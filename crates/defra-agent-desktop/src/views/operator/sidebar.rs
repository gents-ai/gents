use eframe::egui::{self, RichText, Ui};

use crate::audit;
use crate::client::{ClientCore, ClientStore};
use crate::operator::section_meta;
use crate::state::{OperatorSection, PendingOperatorAction, PendingShellAction, ShellState};
use crate::theme;
use crate::views;

pub(super) fn show_sidebar(
    ui: &mut Ui,
    state: &mut ShellState,
    _client: Option<&ClientCore>,
    store: Option<&ClientStore>,
) {
    let Some(store) = store else {
        views::card(
            ui,
            "Operator Unavailable",
            "The desktop client must finish bootstrapping before operator documents can render.",
        );
        return;
    };

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(14.0);

        if state.operator.selected_agent_did.is_none() {
            views::card(
                ui,
                "Select Deployment",
                "Choose a deployment above, then open Configure to manage its behaviors and runtime state.",
            );
            return;
        }

        render_section_group(ui, state, store, "Configure", &OperatorSection::MANAGE);
        ui.add_space(10.0);
        render_section_group(ui, state, store, "History", &OperatorSection::INSPECT);
    });
}

fn render_section_group(
    ui: &mut Ui,
    state: &mut ShellState,
    store: &ClientStore,
    title: &str,
    sections: &[OperatorSection],
) {
    let palette = theme::palette();
    let selected_section = state.operator.selected_section;
    let open = sections.contains(&selected_section);

    ui.horizontal(|ui| {
        ui.add_space(14.0);
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            let header = egui::Button::new(
                RichText::new(format!("{} {}", if open { "v" } else { ">" }, title))
                    .family(theme::stencil_family())
                    .size(13.0)
                    .color(palette.text_1)
                    .strong(),
            )
            .min_size(egui::vec2(ui.available_width(), 28.0))
            .fill(palette.background_0)
            .stroke(egui::Stroke::new(1.0, palette.stroke_subtle));
            if ui.add(header).clicked() && !open {
                state.queue_shell_action(PendingShellAction::Operator(
                    PendingOperatorAction::SelectSection {
                        section: sections[0],
                    },
                ));
            }

            if !open {
                return;
            }

            ui.add_space(8.0);
            for section in sections {
                let (title, meta) =
                    section_meta(store, *section, state.operator.selected_agent_did.as_deref());
                let response = views::side_row(
                    ui,
                    title,
                    &meta,
                    selected_section == *section,
                    if selected_section == *section {
                        palette.accent
                    } else {
                        palette.text_3
                    },
                    None,
                );
                audit::record(ui, &audit::targets::operator_section(*section), &response);
                if response.clicked() {
                    state.queue_shell_action(PendingShellAction::Operator(
                        PendingOperatorAction::SelectSection { section: *section },
                    ));
                }
                ui.add_space(6.0);
            }
        });
    });
}
