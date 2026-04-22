mod markdown;
mod messages;
mod modal;
mod reasoning_cards;
mod tool_cards;

use defra_agent_protocol::client_protocol::ClientTurnState;
use defra_agent_protocol::row::AgentRequestRow;
use defra_agent_protocol::transcript::{present_persisted_message, PresentedMessageRole};
use eframe::egui::{self, Ui};
use egui_commonmark::CommonMarkCache;

use crate::client::ClientStore;
use crate::state::ShellState;
use crate::views::components;

use self::messages::{message_block, message_label_color, transcript_surface};
use self::modal::show_tool_detail_modal;
use self::reasoning_cards::{
    latest_reasoning_response, reasoning_block, response_fallback_content,
};
use self::tool_cards::tool_turn_block;

pub use markdown::markdown_theme_names;

pub fn show(
    ui: &mut Ui,
    state: &mut ShellState,
    store: &ClientStore,
    selected_session_id: Option<&str>,
    _turn_state: Option<ClientTurnState>,
    markdown_cache: &mut CommonMarkCache,
) {
    let palette = crate::theme::palette();

    let Some(session_id) = selected_session_id else {
        ui.add_space(4.0);
        ui.set_max_width(560.0);
        components::focus_panel(
            ui,
            Some("Chat"),
            "Start the conversation",
            "Send a message below and the first conversation will be created automatically.",
            |_| {},
        );
        return;
    };

    let transcript = store.transcript(session_id);
    let requests = store.requests_for_session(session_id);
    let latest_reasoning = latest_reasoning_response(store, session_id);

    if transcript.messages.is_empty() && requests.is_empty() {
        ui.add_space(4.0);
        ui.set_max_width(560.0);
        components::focus_panel(
            ui,
            Some("Chat"),
            "Waiting for the first turn",
            "Submitted requests and replies will appear here as soon as they are observed.",
            |_| {},
        );
        show_tool_detail_modal(ui.ctx(), state, markdown_cache);
        return;
    }

    transcript_surface(ui, |ui| {
        let scroll_output = egui::ScrollArea::vertical()
            .stick_to_bottom(state.chat.editor.transcript_stick_to_bottom)
            .show(ui, |ui| {
                if transcript.messages.is_empty() {
                    for chain in collapsed_request_chains(&requests, store) {
                        if let Some(content) = chain
                            .first_visible_request
                            .content
                            .as_deref()
                            .filter(|content| !content.trim().is_empty())
                        {
                            message_block(
                                ui,
                                markdown_cache,
                                format!(
                                    "request:{}:content",
                                    chain.first_visible_request.request_id
                                ),
                                "USER",
                                palette.text_1,
                                content,
                            );
                            ui.add_space(10.0);
                        }
                        if let Some(response) = store
                            .latest_response_for_request(&chain.latest_request.request_id)
                            .or_else(|| {
                                store.responses.iter().rev().find(|response| {
                                    response.session_id.as_deref() == Some(session_id)
                                        && response.request_id.as_deref().is_some_and(
                                            |request_id| {
                                                request_belongs_to_chain(
                                                    request_id,
                                                    &chain.request_ids,
                                                )
                                            },
                                        )
                                })
                            })
                        {
                            if let Some(content) = response_fallback_content(response) {
                                message_block(
                                    ui,
                                    markdown_cache,
                                    format!("response:{}:fallback", response.response_key.as_str()),
                                    "ASSISTANT",
                                    palette.accent,
                                    content,
                                );
                                ui.add_space(10.0);
                            }
                        }
                    }
                } else {
                    for message in &transcript.messages {
                        let presentation = present_persisted_message(
                            message.role.as_deref().unwrap_or("user"),
                            message.content.as_deref().unwrap_or_default(),
                        );
                        let related_tool_calls: Vec<_> = transcript
                            .tool_calls
                            .iter()
                            .copied()
                            .filter(|tool_call| tool_call.message_sequence == message.sequence)
                            .collect();
                        let suppress_tool_message = presentation.role == PresentedMessageRole::Tool
                            && (!transcript.tool_calls.is_empty()
                                || !transcript.tool_results.is_empty());

                        if presentation.has_visible_body() && !suppress_tool_message {
                            message_block(
                                ui,
                                markdown_cache,
                                format!(
                                    "message:{}:{}",
                                    message.sequence.unwrap_or_default(),
                                    presentation.role.label()
                                ),
                                presentation.role.label(),
                                message_label_color(presentation.role),
                                &presentation.body_markdown,
                            );
                            ui.add_space(6.0);
                        }

                        if !related_tool_calls.is_empty() {
                            tool_turn_block(
                                ui,
                                state,
                                &related_tool_calls,
                                &transcript.tool_results,
                            );
                            ui.add_space(8.0);
                        }
                    }
                }

                if let Some(response) = latest_reasoning {
                    ui.add_space(6.0);
                    reasoning_block(ui, state, markdown_cache, response);
                }
            });

        let viewport_bottom = scroll_output.state.offset.y + scroll_output.inner_rect.height();
        let at_bottom = viewport_bottom + 2.0 >= scroll_output.content_size.y;
        state.chat.editor.transcript_stick_to_bottom = at_bottom;
    });

    show_tool_detail_modal(ui.ctx(), state, markdown_cache);
}

struct RequestChain<'a> {
    root_request_id: &'a str,
    first_visible_request: &'a AgentRequestRow,
    latest_request: &'a AgentRequestRow,
    request_ids: std::collections::BTreeSet<&'a str>,
}

fn collapsed_request_chains<'a>(
    requests: &'a [&'a AgentRequestRow],
    _store: &'a ClientStore,
) -> Vec<RequestChain<'a>> {
    let mut chains: Vec<RequestChain<'a>> = Vec::new();

    for request in requests {
        let key = request
            .retry_root_request
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(request.request_id.as_str());
        if let Some(existing) = chains.iter_mut().find(|chain| chain.root_request_id == key) {
            existing.latest_request = request;
            existing.request_ids.insert(request.request_id.as_str());
            continue;
        }

        let mut request_ids = std::collections::BTreeSet::new();
        request_ids.insert(request.request_id.as_str());
        chains.push(RequestChain {
            root_request_id: key,
            first_visible_request: request,
            latest_request: request,
            request_ids,
        });
    }

    chains
}

fn request_belongs_to_chain(
    request_id: &str,
    request_ids: &std::collections::BTreeSet<&str>,
) -> bool {
    request_ids.contains(request_id)
}
