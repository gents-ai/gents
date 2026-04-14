use eframe::egui::{self, Response, Ui, Widget, WidgetText};

use crate::state::{Activity, LogsFilter, OperatorSection};
use crate::telemetry::DesktopLogCategory;

pub(crate) mod targets {
    use super::*;

    pub(crate) const ACTIVITY_CHAT: &str = "activity.chat";
    pub(crate) const ACTIVITY_OPERATOR: &str = "activity.operator";
    pub(crate) const ACTIVITY_PEERS: &str = "activity.peers";
    pub(crate) const ACTIVITY_LOGS: &str = "activity.logs";
    pub(crate) const CHAT_COMPOSER_TEXT: &str = "chat.composer.text";
    pub(crate) const CHAT_CREATE_CONVERSATION: &str = "chat.create_conversation";
    pub(crate) const CHAT_OPEN_PEERS_SETUP: &str = "chat.open_peers_setup";
    pub(crate) const CHAT_RETRY: &str = "chat.retry";
    pub(crate) const CHAT_REASONING_PREFIX: &str = "chat.reasoning";
    pub(crate) const CHAT_SEND: &str = "chat.send";
    pub(crate) const CHAT_TOOL_CARD_PREFIX: &str = "chat.tool_card";
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
    pub(crate) const OPERATOR_RUN_NOW: &str = "operator.run_now";
    pub(crate) const PEERS_ADD_ADDR: &str = "peers.add.addr";
    pub(crate) const PEERS_ADD_AGENT_DID: &str = "peers.add.agent_did";
    pub(crate) const PEERS_ADD_LABEL: &str = "peers.add.label";
    pub(crate) const PEERS_CLEAR: &str = "peers.clear";
    pub(crate) const PEERS_MAIN_COPY_DID: &str = "peers.main.copy_did";
    pub(crate) const PEERS_REMOVE: &str = "peers.remove";
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

    pub(crate) fn chat_deployment(peer_id: &str) -> String {
        format!("chat.deployment.{peer_id}")
    }

    pub(crate) fn chat_reasoning(response_key: &str) -> String {
        format!("{CHAT_REASONING_PREFIX}.{response_key}")
    }

    pub(crate) fn chat_tool_card(card_id: &str) -> String {
        format!("{CHAT_TOOL_CARD_PREFIX}.{card_id}")
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
}

pub(crate) fn button(
    ui: &mut Ui,
    target: impl AsRef<str>,
    text: impl Into<WidgetText>,
) -> Response {
    let response = ui.button(text);
    record(ui, target.as_ref(), &response);
    response
}

pub(crate) fn add<W: Widget>(ui: &mut Ui, target: impl AsRef<str>, widget: W) -> Response {
    let response = ui.add(widget);
    record(ui, target.as_ref(), &response);
    response
}

pub(crate) fn add_sized<W: Widget>(
    ui: &mut Ui,
    target: impl AsRef<str>,
    max_size: impl Into<egui::Vec2>,
    widget: W,
) -> Response {
    let response = ui.add_sized(max_size, widget);
    record(ui, target.as_ref(), &response);
    response
}

pub(crate) fn add_enabled<W: Widget>(
    ui: &mut Ui,
    target: impl AsRef<str>,
    enabled: bool,
    widget: W,
) -> Response {
    let response = ui.add_enabled(enabled, widget);
    record(ui, target.as_ref(), &response);
    response
}

#[cfg(test)]
pub(crate) fn target_rect(ctx: &egui::Context, target: &str) -> Option<egui::Rect> {
    ctx.data(|data| data.get_temp(rect_id(target)))
}

pub(crate) fn record(ui: &Ui, target: &str, response: &Response) {
    ui.ctx()
        .data_mut(|data| data.insert_temp(rect_id(target), response.rect));
}

fn rect_id(target: &str) -> egui::Id {
    egui::Id::new(("desktop-audit-target", target))
}
