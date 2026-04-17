use defra_agent_protocol::row::AgentResponseRow;
use eframe::egui::{self, Ui};
use egui_commonmark::CommonMarkCache;

use crate::client::ClientStore;
use crate::state::ShellState;
use crate::theme;

use super::markdown::render_markdown;

pub(super) fn latest_reasoning_response<'a>(
    store: &'a ClientStore,
    session_id: &str,
) -> Option<&'a AgentResponseRow> {
    store
        .responses
        .iter()
        .rev()
        .find(|response| response.session_id.as_deref() == Some(session_id))
        .filter(|response| {
            response
                .reasoning
                .as_deref()
                .is_some_and(|reasoning| !reasoning.trim().is_empty())
        })
}

pub(super) fn response_fallback_content(response: &AgentResponseRow) -> Option<&str> {
    response
        .content
        .as_deref()
        .filter(|content| !content.trim().is_empty())
        .or_else(|| {
            response
                .error_message
                .as_deref()
                .filter(|content| !content.trim().is_empty())
        })
}

pub(super) fn reasoning_block(
    ui: &mut Ui,
    state: &mut ShellState,
    markdown_cache: &mut CommonMarkCache,
    response: &AgentResponseRow,
) {
    let palette = theme::palette();
    let card_id = format!("reasoning:{}", response.response_key);
    let expanded = state
        .chat
        .editor
        .expanded_reasoning_cards
        .contains(&card_id);

    egui::Frame::new()
        .fill(palette.background_1)
        .stroke(egui::Stroke::new(1.0, palette.stroke))
        .corner_radius(4)
        .inner_margin(10)
        .show(ui, |ui| {
            let label = format!(
                "REASONING DISCLOSURE  {}",
                response.status.as_deref().unwrap_or("observed")
            );
            let toggle = ui
                .selectable_label(expanded, label)
                .on_hover_text("toggle reasoning disclosure");
            crate::audit::record(
                ui,
                &crate::audit::targets::chat_reasoning(&response.response_key),
                &toggle,
            );
            if toggle.clicked()
                && !state
                    .chat
                    .editor
                    .expanded_reasoning_cards
                    .insert(card_id.clone())
            {
                state.chat.editor.expanded_reasoning_cards.remove(&card_id);
            }

            if expanded {
                ui.add_space(6.0);
                render_markdown(
                    ui,
                    markdown_cache,
                    format!("reasoning:{}", response.response_key),
                    response.reasoning.as_deref().unwrap_or_default(),
                );
            }
        });
}
