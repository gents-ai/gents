use eframe::egui::{self, Response, Ui, Widget, WidgetText};

use crate::state::{Activity, LogsFilter, OperatorSection};
use crate::telemetry::DesktopLogCategory;

#[cfg(test)]
#[derive(Debug, Clone, Copy)]
struct TargetRecord {
    rect: egui::Rect,
    interact_rect: egui::Rect,
    generation: u64,
}

pub(crate) mod targets {
    use super::*;

    pub(crate) const ACTIVITY_CHAT: &str = "activity.chat";
    pub(crate) const ACTIVITY_OPERATOR: &str = "activity.operator";
    pub(crate) const ACTIVITY_PEERS: &str = "activity.peers";
    pub(crate) const ACTIVITY_LOGS: &str = "activity.logs";
    pub(crate) const CHAT_COMPOSER_TEXT: &str = "chat.composer.text";
    pub(crate) const CHAT_BEHAVIOR_SELECT: &str = "chat.behavior.select";
    pub(crate) const CHAT_CREATE_CONVERSATION: &str = "chat.create_conversation";
    pub(crate) const CHAT_NEW_CONVERSATION: &str = "chat.new_conversation";
    pub(crate) const CHAT_OPEN_PEERS_SETUP: &str = "chat.open_peers_setup";
    pub(crate) const CHAT_RETRY: &str = "chat.retry";
    pub(crate) const CHAT_REASONING_PREFIX: &str = "chat.reasoning";
    pub(crate) const CHAT_SEND: &str = "chat.send";
    pub(crate) const CHAT_TOOL_CARD_PREFIX: &str = "chat.tool_card";
    pub(crate) const CHAT_TOOL_ARGS_PREFIX: &str = "chat.tool_args";
    pub(crate) const CHAT_TOOL_OUTPUT_PREFIX: &str = "chat.tool_output";
    pub(crate) const CHAT_EXPORT: &str = "chat.export";
    pub(crate) const LOGS_FILTER_ALL: &str = "logs.filter.all";
    pub(crate) const LOGS_FILTER_REPLICATION: &str = "logs.filter.replication";
    pub(crate) const LOGS_FILTER_PEERING: &str = "logs.filter.peering";
    pub(crate) const LOGS_FILTER_TURNS: &str = "logs.filter.turns";
    pub(crate) const LOGS_FILTER_WRITES: &str = "logs.filter.writes";
    pub(crate) const LOGS_FILTER_WARNINGS: &str = "logs.filter.warnings";
    pub(crate) const OPERATOR_APPLY: &str = "operator.apply";
    pub(crate) const OPERATOR_DISCARD: &str = "operator.discard";
    pub(crate) const OPERATOR_ENTITY_FILTER: &str = "operator.entity_filter";
    pub(crate) const OPERATOR_NEW: &str = "operator.new";
    pub(crate) const OPERATOR_RUN_NOW: &str = "operator.run_now";
    pub(crate) const PEERS_ADD_ADDR: &str = "peers.add.addr";
    pub(crate) const PEERS_ADD_AGENT_DID: &str = "peers.add.agent_did";
    pub(crate) const PEERS_ADD_LABEL: &str = "peers.add.label";
    pub(crate) const PEERS_CLEAR: &str = "peers.clear";
    pub(crate) const PEERS_MAIN_COPY_DID: &str = "peers.main.copy_did";
    pub(crate) const PEERS_REPAIR_NOW: &str = "peers.repair_now";
    pub(crate) const PEERS_REMOVE: &str = "peers.remove";
    pub(crate) const PEERS_RESTART_CLIENT: &str = "peers.restart_client";
    pub(crate) const PEERS_TOGGLE_ADD_FORM: &str = "peers.toggle_add_form";
    pub(crate) const PEERS_ONBOARDING_COPY_DID: &str = "peers.onboarding.copy_did";
    pub(crate) const PEERS_SAVE: &str = "peers.save";

    pub(crate) fn activity(activity: Activity) -> &'static str {
        match activity {
            Activity::Chat => ACTIVITY_CHAT,
            Activity::Operator => ACTIVITY_OPERATOR,
            Activity::Peers => ACTIVITY_PEERS,
            Activity::Logs => ACTIVITY_LOGS,
        }
    }

    pub(crate) fn logs_filter(filter: LogsFilter) -> &'static str {
        match filter {
            LogsFilter::All => LOGS_FILTER_ALL,
            LogsFilter::Category(DesktopLogCategory::Replication) => LOGS_FILTER_REPLICATION,
            LogsFilter::Category(DesktopLogCategory::Peering) => LOGS_FILTER_PEERING,
            LogsFilter::Category(DesktopLogCategory::Turns) => LOGS_FILTER_TURNS,
            LogsFilter::Category(DesktopLogCategory::Writes) => LOGS_FILTER_WRITES,
            LogsFilter::Category(DesktopLogCategory::Warnings) => LOGS_FILTER_WARNINGS,
        }
    }

    pub(crate) fn chat_conversation(session_id: &str) -> String {
        format!("chat.conversation.{session_id}")
    }

    pub(crate) fn chat_behavior_option(behavior_id: &str) -> String {
        format!("chat.behavior.option.{behavior_id}")
    }

    pub(crate) fn chat_deployment(peer_id: &str) -> String {
        format!("chat.deployment.{peer_id}")
    }

    pub(crate) fn chat_agent(agent_did: &str) -> String {
        format!("chat.agent.{agent_did}")
    }

    pub(crate) fn chat_reasoning(response_key: &str) -> String {
        format!("{CHAT_REASONING_PREFIX}.{response_key}")
    }

    pub(crate) fn chat_tool_card(card_id: &str) -> String {
        format!("{CHAT_TOOL_CARD_PREFIX}.{card_id}")
    }

    pub(crate) fn chat_tool_args(card_id: &str) -> String {
        format!("{CHAT_TOOL_ARGS_PREFIX}.{card_id}")
    }

    pub(crate) fn chat_tool_output(card_id: &str) -> String {
        format!("{CHAT_TOOL_OUTPUT_PREFIX}.{card_id}")
    }

    pub(crate) fn operator_agent(agent_did: &str) -> String {
        format!("operator.agent.{agent_did}")
    }

    pub(crate) fn operator_deployment(peer_id: &str) -> String {
        format!("operator.deployment.{peer_id}")
    }

    pub(crate) fn operator_entity(entity_id: &str) -> String {
        format!("operator.entity.{entity_id}")
    }

    pub(crate) fn operator_field(label: &str) -> String {
        format!("operator.field.{label}")
    }

    pub(crate) fn operator_toggle(label: &str) -> String {
        format!("operator.toggle.{label}")
    }

    pub(crate) fn operator_section(section: OperatorSection) -> String {
        format!("operator.section.{}", section.label())
    }

    pub(crate) fn peers_peer(record_id: &str) -> String {
        format!("peers.peer.{record_id}")
    }

    pub(crate) fn peers_agent(record_id: &str) -> String {
        format!("peers.agent.{record_id}")
    }
}

pub(crate) fn button(
    ui: &mut Ui,
    target: impl AsRef<str>,
    text: impl Into<WidgetText>,
) -> Response {
    let target = target.as_ref();
    let response = ui.push_id(target, |ui| ui.button(text)).inner;
    record(ui, target, &response);
    response
}

pub(crate) fn add<W: Widget>(ui: &mut Ui, target: impl AsRef<str>, widget: W) -> Response {
    let target = target.as_ref();
    let response = ui.push_id(target, |ui| ui.add(widget)).inner;
    record(ui, target, &response);
    response
}

pub(crate) fn add_sized<W: Widget>(
    ui: &mut Ui,
    target: impl AsRef<str>,
    max_size: impl Into<egui::Vec2>,
    widget: W,
) -> Response {
    let target = target.as_ref();
    let response = ui
        .push_id(target, |ui| ui.add_sized(max_size, widget))
        .inner;
    record(ui, target, &response);
    response
}

pub(crate) fn add_enabled<W: Widget>(
    ui: &mut Ui,
    target: impl AsRef<str>,
    enabled: bool,
    widget: W,
) -> Response {
    let target = target.as_ref();
    let response = ui
        .push_id(target, |ui| ui.add_enabled(enabled, widget))
        .inner;
    record(ui, target, &response);
    response
}

#[cfg(test)]
pub(crate) fn target_rect(ctx: &egui::Context, target: &str) -> Option<egui::Rect> {
    ctx.data(|data| {
        let generation = data.get_temp::<u64>(generation_id()).unwrap_or_default();
        data.get_temp::<TargetRecord>(rect_id(target))
            .filter(|record| record.generation == generation)
            .map(|record| record.rect)
    })
}

#[cfg(test)]
pub(crate) fn target_interact_rect(ctx: &egui::Context, target: &str) -> Option<egui::Rect> {
    ctx.data(|data| {
        let generation = data.get_temp::<u64>(generation_id()).unwrap_or_default();
        data.get_temp::<TargetRecord>(rect_id(target))
            .filter(|record| record.generation == generation)
            .map(|record| record.interact_rect)
            .filter(egui::Rect::is_positive)
    })
}

#[cfg(test)]
pub(crate) fn begin_frame(ctx: &egui::Context) {
    ctx.data_mut(|data| {
        let generation = data
            .get_temp::<u64>(generation_id())
            .unwrap_or_default()
            .wrapping_add(1);
        data.insert_temp(generation_id(), generation);
    });
}

pub(crate) fn record(ui: &Ui, target: &str, response: &Response) {
    #[cfg(test)]
    {
        ui.ctx().data_mut(|data| {
            let generation = data.get_temp::<u64>(generation_id()).unwrap_or_default();
            data.insert_temp(
                rect_id(target),
                TargetRecord {
                    rect: response.rect,
                    interact_rect: if target.starts_with("activity.") {
                        response.rect
                    } else {
                        response.interact_rect
                    },
                    generation,
                },
            );
        });
    }

    #[cfg(not(test))]
    {
        ui.ctx()
            .data_mut(|data| data.insert_temp(rect_id(target), response.rect));
    }
}

fn rect_id(target: &str) -> egui::Id {
    egui::Id::new(("desktop-audit-target", target))
}

#[cfg(test)]
fn generation_id() -> egui::Id {
    egui::Id::new("desktop-audit-target-generation")
}
