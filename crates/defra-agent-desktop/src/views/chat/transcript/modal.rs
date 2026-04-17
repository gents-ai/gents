use eframe::egui::{self, RichText};
use egui_commonmark::CommonMarkCache;

use crate::state::{ShellState, ToolDetailModalState};
use crate::theme;

use super::markdown::render_markdown;

pub(super) fn open_tool_detail_modal(
    state: &mut ShellState,
    card_id: &str,
    title: &str,
    body: &str,
    language: Option<&str>,
) {
    state.chat.editor.tool_detail_modal = Some(ToolDetailModalState {
        card_id: card_id.to_string(),
        title: title.to_string(),
        body: body.to_string(),
        language: language.map(str::to_string),
    });
}

pub(super) fn show_tool_detail_modal(
    ctx: &egui::Context,
    state: &mut ShellState,
    markdown_cache: &mut CommonMarkCache,
) {
    let Some(modal) = state.chat.editor.tool_detail_modal.clone() else {
        return;
    };

    let mut open = true;
    egui::Window::new(modal.title.clone())
        .id(egui::Id::new(("tool_detail_modal", modal.card_id.as_str())))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(760.0)
        .default_height(520.0)
        .show(ctx, |ui| {
            ui.label(
                RichText::new(modal.card_id.as_str())
                    .monospace()
                    .size(10.5)
                    .color(theme::palette().text_2),
            );
            ui.add_space(8.0);
            egui::ScrollArea::vertical().show(ui, |ui| {
                let content = match modal.language.as_deref() {
                    Some(language) => fenced_code_block(&modal.body, Some(language)),
                    None => fenced_code_block(&modal.body, None),
                };
                render_markdown(
                    ui,
                    markdown_cache,
                    format!("tool-detail:{}", modal.card_id),
                    &content,
                );
            });
        });

    if !open {
        state.chat.editor.tool_detail_modal = None;
    }
}

fn fenced_code_block(content: &str, language: Option<&str>) -> String {
    let language = language.unwrap_or_default();
    format!("```{language}\n{content}\n```")
}
