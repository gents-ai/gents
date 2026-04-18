use eframe::egui::{self, Response, Ui, Widget, WidgetText};

#[cfg(test)]
#[derive(Debug, Clone, Copy)]
struct TargetRecord {
    rect: egui::Rect,
    interact_rect: egui::Rect,
    generation: u64,
}

pub(crate) mod targets {
    #[cfg(test)]
    use crate::state::ManageSection;

    #[cfg(test)]
    pub(crate) const ACTIVITY_CHAT: &str = "activity.chat";
    #[cfg(test)]
    pub(crate) const ACTIVITY_MANAGE: &str = "activity.manage";
    pub(crate) const CHAT_COMPOSER_TEXT: &str = "chat.composer.text";
    pub(crate) const CHAT_CREATE_CONVERSATION: &str = "chat.create_conversation";
    pub(crate) const CHAT_NEW_CONVERSATION: &str = "chat.new_conversation";
    pub(crate) const CHAT_OPEN_SETUP: &str = "chat.open_setup";
    pub(crate) const CHAT_RETRY: &str = "chat.retry";
    pub(crate) const CHAT_REASONING_PREFIX: &str = "chat.reasoning";
    pub(crate) const CHAT_SEND: &str = "chat.send";
    pub(crate) const CHAT_TOOL_CARD_PREFIX: &str = "chat.tool_card";
    pub(crate) const CHAT_TOOL_ARGS_PREFIX: &str = "chat.tool_args";
    pub(crate) const CHAT_TOOL_OUTPUT_PREFIX: &str = "chat.tool_output";
    pub(crate) const CHAT_EXPORT: &str = "chat.export";
    pub(crate) const MANAGE_APPLY: &str = "manage.apply";
    pub(crate) const MANAGE_DISCARD: &str = "manage.discard";
    pub(crate) const MANAGE_ENTITY_FILTER: &str = "manage.entity_filter";
    pub(crate) const MANAGE_NEW: &str = "manage.new";
    pub(crate) const MANAGE_RUN_NOW: &str = "manage.run_now";
    pub(crate) const SETUP_ADD_ADDR: &str = "setup.add.addr";
    pub(crate) const SETUP_ADD_AGENT_DID: &str = "setup.add.agent_did";
    pub(crate) const SETUP_BACK_TO_DEPLOYMENTS: &str = "setup.back_to_deployments";
    pub(crate) const SETUP_ADD_LABEL: &str = "setup.add.label";
    pub(crate) const SETUP_CLEAR: &str = "setup.clear";
    pub(crate) const SETUP_REPAIR_NOW: &str = "setup.repair_now";
    pub(crate) const SETUP_REMOVE: &str = "setup.remove";
    pub(crate) const SETUP_RESTART_CLIENT: &str = "setup.restart_client";
    pub(crate) const SETUP_ONBOARDING_COPY_DID: &str = "setup.onboarding.copy_did";
    pub(crate) const SETUP_SAVE: &str = "setup.save";

    #[cfg(test)]
    pub(crate) fn activity(activity: crate::state::Activity) -> &'static str {
        match activity {
            crate::state::Activity::Chat => ACTIVITY_CHAT,
            crate::state::Activity::Manage => ACTIVITY_MANAGE,
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

    pub(crate) fn manage_agent(agent_did: &str) -> String {
        format!("manage.agent.{agent_did}")
    }

    pub(crate) fn manage_deployment(peer_id: &str) -> String {
        format!("manage.deployment.{peer_id}")
    }

    pub(crate) fn manage_entity(entity_id: &str) -> String {
        format!("manage.entity.{entity_id}")
    }

    pub(crate) fn manage_field(label: &str) -> String {
        format!("manage.field.{label}")
    }

    pub(crate) fn manage_toggle(label: &str) -> String {
        format!("manage.toggle.{label}")
    }

    #[cfg(test)]
    pub(crate) fn manage_section(section: ManageSection) -> String {
        format!("manage.section.{}", section.label())
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
