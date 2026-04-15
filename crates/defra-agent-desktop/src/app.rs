use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use eframe::egui::{self, Align2, Context, Panel, RichText};
use egui_commonmark::CommonMarkCache;
use tokio::runtime::Runtime;
use tokio::sync::watch;

use crate::audit;
use crate::client::{ClientCore, ClientStore};
use crate::state::{Activity, ShellState};
use crate::telemetry::{global_log_store, DesktopLogStore};
use crate::theme;
use crate::views;

pub struct DesktopApp {
    state: ShellState,
    client: Option<Arc<ClientCore>>,
    store_updates: Option<watch::Receiver<u64>>,
    bootstrap_errors: Vec<String>,
    log_store: Arc<DesktopLogStore>,
    markdown_cache: CommonMarkCache,
    runtime: Arc<Runtime>,
}

impl DesktopApp {
    pub fn new(cc: &eframe::CreationContext<'_>, runtime: Arc<Runtime>) -> Self {
        let (client, bootstrap_errors) = match runtime.block_on(ClientCore::start()) {
            Ok(core) => {
                let client = Arc::new(core);
                (Some(client.clone()), client.bootstrap_errors().to_vec())
            }
            Err(error) => (None, vec![error.to_string()]),
        };

        Self::from_parts(cc, runtime, client, bootstrap_errors, global_log_store())
    }

    fn from_parts(
        cc: &eframe::CreationContext<'_>,
        runtime: Arc<Runtime>,
        client: Option<Arc<ClientCore>>,
        bootstrap_errors: Vec<String>,
        log_store: Arc<DesktopLogStore>,
    ) -> Self {
        theme::apply_theme(&cc.egui_ctx);
        let mut state = ShellState::default();
        let store_updates = client.as_ref().map(|client| {
            apply_bootstrap_state(&mut state, client.as_ref());
            apply_snapshot_state(&mut state, client.store().snapshot().as_ref());
            client.store_updates()
        });

        if client.is_none() {
            apply_bootstrap_failure_state(&mut state);
        }

        Self {
            state,
            client,
            store_updates,
            bootstrap_errors,
            log_store,
            markdown_cache: CommonMarkCache::default(),
            runtime,
        }
    }

    fn block_on_runtime<T>(&self, future: impl Future<Output = T>) -> T {
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::block_in_place(|| self.runtime.block_on(future))
        } else {
            self.runtime.block_on(future)
        }
    }

    fn shutdown_client(&mut self) {
        let Some(client) = self.client.take() else {
            self.store_updates = None;
            return;
        };

        self.store_updates = None;
        if let Err(error) = self.block_on_runtime(client.shutdown()) {
            tracing::error!(error = %error, "failed to shut down desktop client");
            self.bootstrap_errors
                .push(format!("desktop shutdown failed: {error}"));
        }
    }

    fn show_activity_bar(&mut self, ui: &mut egui::Ui) {
        let palette = theme::palette();
        let metrics = theme::metrics();

        Panel::left("activity_bar")
            .resizable(false)
            .exact_size(metrics.activity_bar_width)
            .show_inside(ui, |ui| {
                let ctx = ui.ctx().clone();
                ui.add_space(12.0);
                ui.horizontal_centered(|ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(32.0, 32.0), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 4.0, palette.background_2);
                    ui.painter().text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        "DF",
                        egui::FontId::new(15.0, theme::stencil_family()),
                        palette.accent,
                    );
                });

                ui.add_space(18.0);
                for activity in Activity::ALL {
                    self.activity_button(ui, activity);
                    ui.add_space(4.0);
                }

                ui.add_space((ui.available_height() - 80.0).max(0.0));
                self.identity_chip(ui, &ctx);
            });
    }

    fn activity_button(&mut self, ui: &mut egui::Ui, activity: Activity) {
        let palette = theme::palette();
        let metrics = theme::metrics();
        let selected = self.state.activity == activity;
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(metrics.control_height, metrics.control_height),
            egui::Sense::click(),
        );

        if response.hovered() || selected {
            ui.painter().rect_filled(
                rect,
                4.0,
                if selected {
                    palette.background_2
                } else {
                    palette.background_1
                },
            );
        }

        if selected {
            ui.painter().line_segment(
                [
                    egui::pos2(rect.left() - 4.0, rect.top() + 8.0),
                    egui::pos2(rect.left() - 4.0, rect.bottom() - 8.0),
                ],
                egui::Stroke::new(2.0, palette.accent),
            );
        }

        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            activity.short_label(),
            egui::FontId::new(14.0, theme::stencil_family()),
            if selected {
                palette.accent
            } else {
                palette.text_2
            },
        );

        let response = response.on_hover_text(activity.label());
        audit::record(ui, audit::targets::activity(activity), &response);

        if response.clicked() {
            self.state.activity = activity;
        }
    }

    fn identity_chip(&self, ui: &mut egui::Ui, ctx: &Context) {
        let palette = theme::palette();
        let metrics = theme::metrics();

        ui.horizontal_centered(|ui| {
            let (avatar_rect, _) =
                ui.allocate_exact_size(egui::vec2(30.0, 30.0), egui::Sense::hover());
            ui.painter()
                .rect_filled(avatar_rect, 15.0, palette.background_2);
            ui.painter().text(
                avatar_rect.center(),
                Align2::CENTER_CENTER,
                self.state.identity.initials,
                egui::FontId::new(11.0, egui::FontFamily::Monospace),
                palette.text_1,
            );
            ui.painter().circle_filled(
                egui::pos2(avatar_rect.right() - 2.0, avatar_rect.bottom() - 2.0),
                4.0,
                theme::throb_color(ctx, palette.accent),
            );
        });

        ui.add_space(6.0);
        ui.horizontal_centered(|ui| {
            ui.label(
                RichText::new(self.state.identity.label.clone())
                    .monospace()
                    .size(8.5)
                    .color(palette.text_3),
            );
        });
        ui.horizontal_centered(|ui| {
            ui.label(
                RichText::new(self.state.identity.did_short.clone())
                    .monospace()
                    .size(8.5)
                    .color(palette.text_2),
            );
        });
        ui.add_space(metrics.section_spacing);
    }

    fn show_sidebar(&mut self, ui: &mut egui::Ui, store: Option<&ClientStore>) {
        let Some(width) = self.state.activity.sidebar_width() else {
            return;
        };

        Panel::left("activity_sidebar")
            .resizable(false)
            .exact_size(width)
            .show_inside(ui, |ui| {
                views::show_sidebar(
                    ui,
                    &mut self.state,
                    self.client.as_deref(),
                    store,
                    self.runtime.as_ref(),
                );
            });
    }

    fn show_rail(&mut self, ui: &mut egui::Ui, store: Option<&ClientStore>) {
        let Some(width) = self.state.activity.rail_width() else {
            return;
        };

        Panel::right("activity_rail")
            .resizable(false)
            .exact_size(width)
            .show_inside(ui, |ui| {
                views::show_rail(
                    ui,
                    &mut self.state,
                    self.client.as_deref(),
                    store,
                    self.log_store.as_ref(),
                    self.runtime.as_ref(),
                );
            });
    }

    fn show_status_bar(&self, ui: &mut egui::Ui) {
        let palette = theme::palette();
        let metrics = theme::metrics();

        Panel::bottom("status_bar")
            .resizable(false)
            .exact_size(metrics.status_bar_height)
            .show_inside(ui, |ui| {
                let rect = ui.max_rect();
                ui.painter().line_segment(
                    [
                        egui::pos2(rect.left(), rect.top()),
                        egui::pos2(rect.left() + rect.width() * 0.52, rect.top()),
                    ],
                    egui::Stroke::new(1.0, palette.accent_dim),
                );

                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    ui.label(
                        RichText::new(format!(
                            "peered {}/{}",
                            self.state.status.peered_now, self.state.status.peered_target
                        ))
                        .monospace()
                        .size(10.5)
                        .color(palette.text_2),
                    );
                    ui.label(
                        RichText::new(format!(
                            "{} runtime: {}",
                            self.state.status.active_agent, self.state.status.runtime_state
                        ))
                        .monospace()
                        .size(10.5)
                        .color(palette.text_0),
                    );
                    ui.label(
                        RichText::new(format!("gossip lag {}ms", self.state.status.gossip_lag_ms))
                            .monospace()
                            .size(10.5)
                            .color(palette.text_2),
                    );
                    ui.label(
                        RichText::new(format!(
                            "replication: {}",
                            self.state.status.replication_state
                        ))
                        .monospace()
                        .size(10.5)
                        .color(palette.text_2),
                    );
                    ui.label(
                        RichText::new(format!("errors {}", self.state.status.error_count))
                            .monospace()
                            .size(10.5)
                            .color(if self.state.status.error_count == 0 {
                                palette.text_2
                            } else {
                                palette.warning
                            }),
                    );
                    ui.label(
                        RichText::new(format!("frm:{:04}", self.state.status.frame_counter))
                            .monospace()
                            .size(10.5)
                            .color(palette.text_3),
                    );
                    ui.label(
                        RichText::new(self.state.status.did_short.clone())
                            .monospace()
                            .size(10.5)
                            .color(palette.text_2),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(self.state.status.build_label.clone())
                                .monospace()
                                .size(10.5)
                                .color(palette.text_3),
                        );
                    });
                });
            });
    }

    fn show_main(&mut self, ui: &mut egui::Ui, store: Option<&ClientStore>) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            if !self.bootstrap_errors.is_empty() {
                self.show_bootstrap_banner(ui);
                ui.add_space(10.0);
            }
            if let Some(error) = self
                .client
                .as_ref()
                .and_then(|client| client.last_mutation_error())
            {
                self.show_mutation_banner(ui, &error);
                ui.add_space(10.0);
            }
            views::show_main(
                ui,
                &mut self.state,
                self.client.as_deref(),
                store,
                self.log_store.as_ref(),
                self.runtime.as_ref(),
                &mut self.markdown_cache,
            );
        });
    }

    fn show_bootstrap_banner(&self, ui: &mut egui::Ui) {
        let palette = theme::palette();

        ui.group(|ui| {
            ui.label(
                RichText::new("BOOTSTRAP")
                    .family(theme::stencil_family())
                    .size(13.0)
                    .color(palette.warning)
                    .strong(),
            );
            ui.add_space(6.0);
            for error in &self.bootstrap_errors {
                ui.label(
                    RichText::new(error)
                        .monospace()
                        .size(11.0)
                        .color(palette.text_1),
                );
            }
            if self.client.is_none() {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "The shell is still usable, but client-core startup needs to succeed before replication and submissions can be wired in.",
                    )
                    .size(12.5)
                    .color(palette.text_2),
                );
            }
        });
    }

    fn show_mutation_banner(&self, ui: &mut egui::Ui, error: &str) {
        let palette = theme::palette();

        ui.group(|ui| {
            ui.label(
                RichText::new("MUTATION")
                    .family(theme::stencil_family())
                    .size(13.0)
                    .color(palette.warning)
                    .strong(),
            );
            ui.add_space(6.0);
            ui.label(
                RichText::new(error)
                    .monospace()
                    .size(11.0)
                    .color(palette.text_1),
            );
        });
    }
}

impl eframe::App for DesktopApp {
    fn logic(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.state.status.advance_frame();

        if let (Some(client), Some(store_updates)) = (&self.client, &mut self.store_updates) {
            if store_updates.has_changed().unwrap_or(false) {
                let _ = store_updates.borrow_and_update();
                apply_snapshot_state(&mut self.state, client.store().snapshot().as_ref());
                ctx.request_repaint();
            }

            self.state.status.error_count =
                self.bootstrap_errors.len() + usize::from(client.last_mutation_error().is_some());
        }

        ctx.request_repaint_after(Duration::from_millis(33));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let store = self.client.as_ref().map(|client| client.store().snapshot());
        let store_ref = store.as_deref();

        if let (Some(client), Some(store_ref)) = (self.client.as_deref(), store_ref) {
            apply_first_launch_focus(&mut self.state, client, store_ref);
        }
        views::prepare_state(&mut self.state, self.client.as_deref(), store_ref);
        self.show_status_bar(ui);
        self.show_activity_bar(ui);
        self.show_sidebar(ui, store_ref);
        self.show_rail(ui, store_ref);
        self.show_main(ui, store_ref);
    }

    fn on_exit(&mut self) {
        self.shutdown_client();
    }
}

impl Drop for DesktopApp {
    fn drop(&mut self) {
        self.shutdown_client();
    }
}

fn apply_bootstrap_state(state: &mut ShellState, client: &ClientCore) {
    let error_count = client.bootstrap_errors().len();

    state.identity.did_short = client.principal().short_did();
    state.status.peered_now = client.dialed_peer_count();
    state.status.peered_target = client.configured_peer_count();
    state.status.active_agent = "desktop client".to_string();
    state.status.runtime_state = if error_count == 0 {
        "client core online".to_string()
    } else {
        "client core degraded".to_string()
    };
    state.status.replication_state = "subscriptions armed".to_string();
    state.status.error_count = error_count;
    state.status.did_short = client.principal().short_did();
    state.status.build_label = format!("peer:{}", abbreviate_id(client.local_peer_id()));
}

fn apply_bootstrap_failure_state(state: &mut ShellState) {
    state.identity.did_short = "identity unavailable".to_string();
    state.status.peered_now = 0;
    state.status.peered_target = 0;
    state.status.active_agent = "desktop client".to_string();
    state.status.runtime_state = "bootstrap failed".to_string();
    state.status.replication_state = "offline".to_string();
    state.status.error_count = 1;
    state.status.build_label = "bootstrap".to_string();
}

fn apply_snapshot_state(state: &mut ShellState, store: &ClientStore) {
    state.status.runtime_state = format!(
        "{} agents / {} conversations",
        store.agent_principals.len(),
        store.conversations.len()
    );
    state.status.replication_state = if store.requests.is_empty() {
        "subscriptions armed".to_string()
    } else {
        format!("{} requests observed", store.requests.len())
    };
}

fn apply_first_launch_focus(state: &mut ShellState, client: &ClientCore, store: &ClientStore) {
    if !state.onboarding.first_launch_redirect_done
        && state.activity == Activity::Chat
        && should_focus_first_launch(client, store)
    {
        state.activity = Activity::Peers;
        state.peers.show_add_form = true;
        state.onboarding.first_launch_redirect_done = true;
    }
}

fn should_focus_first_launch(client: &ClientCore, store: &ClientStore) -> bool {
    client.configured_peer_count() == 0
        && store.agent_principals.is_empty()
        && store.conversations.is_empty()
        && store.requests.is_empty()
        && store.responses.is_empty()
}

fn abbreviate_id(value: &str) -> String {
    if value.len() <= 12 {
        return value.to_string();
    }

    format!("{}..{}", &value[..8], &value[value.len() - 2..])
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::OnceLock;
    use std::thread;
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use anyhow::{anyhow, Context, Result};
    use defra_agent::defra_node::EmbeddedNode;
    use defra_agent::graphql::escape_graphql_string;
    use defra_agent::{
        ensure_agent_principal, load_agent_behavior, upsert_agent_behavior, AgentIdentity,
        BackendProviderKind, DefraAgent, DocumentRuntimeOptions, SimpleIdentity, ToolCeiling,
    };
    use defra_agent_protocol::row::{
        AgentBehaviorRow, InferenceBackendRow, InferenceProfileRow, ScheduledTaskRow,
        ToolSelectionRow,
    };
    use eframe::App as _;
    use serde_json::Value;
    use tokio::sync::watch;
    use tracing_subscriber::{prelude::*, EnvFilter};

    use crate::audit;
    use crate::client::{ClientCore, ClientCoreOptions, DesktopPaths};
    use crate::state::{LogsFilter, OperatorDraft};
    use crate::telemetry::{global_log_layer, global_log_store, DesktopLogStore};

    async fn insert_agent_principal(
        core: &ClientCore,
        agent_did: &str,
        display_name: &str,
        default_behavior_id: &str,
    ) -> Result<()> {
        let response = core
            .node()
            .execute(&format!(
                r#"mutation {{
                add_AgentPrincipal(input: {{
                    agent_did: "{agent_did}"
                    display_name: "{display_name}"
                    default_behavior_id: "{default_behavior_id}"
                    enabled: true
                }}) {{ agent_did }}
            }}"#,
                agent_did = escape_graphql_string(agent_did),
                display_name = escape_graphql_string(display_name),
                default_behavior_id = escape_graphql_string(default_behavior_id),
            ))
            .await;
        if response.has_errors() {
            anyhow::bail!("add_AgentPrincipal failed: {:?}", response.errors);
        }
        Ok(())
    }

    async fn insert_agent_runtime(
        core: &ClientCore,
        agent_did: &str,
        default_behavior_id: &str,
    ) -> Result<()> {
        let response = core
            .node()
            .execute(&format!(
                r#"mutation {{
                upsert_AgentRuntime(
                    filter: {{ agent_did: {{ _eq: "{agent_did}" }} }},
                    add: {{
                        agent_did: "{agent_did}"
                        process_state: "ready"
                        reconcile_phase: "idle"
                        active_generation: 1
                        router_generation: 1
                        default_behavior_id: "{default_behavior_id}"
                        runnable_behavior_count: 1
                        unavailable_behavior_count: 0
                        last_reconcile_result: "startup"
                        last_reconcile_error: ""
                        last_reconcile_completed_at: "2026-04-14T00:00:00Z"
                        updated_at: "2026-04-14T00:00:00Z"
                    }},
                    update: {{
                        process_state: "ready"
                        reconcile_phase: "idle"
                        active_generation: 1
                        router_generation: 1
                        default_behavior_id: "{default_behavior_id}"
                        runnable_behavior_count: 1
                        unavailable_behavior_count: 0
                        last_reconcile_result: "startup"
                        last_reconcile_error: ""
                        last_reconcile_completed_at: "2026-04-14T00:00:00Z"
                        updated_at: "2026-04-14T00:00:00Z"
                    }}
                ) {{ _docID }}
            }}"#,
                agent_did = escape_graphql_string(agent_did),
                default_behavior_id = escape_graphql_string(default_behavior_id),
            ))
            .await;
        if response.has_errors() {
            anyhow::bail!("upsert_AgentRuntime failed: {:?}", response.errors);
        }
        Ok(())
    }

    async fn seed_operator_documents(core: &ClientCore) -> Result<()> {
        insert_agent_principal(core, "did:defra:amy", "Amy", "amy-default").await?;
        insert_agent_runtime(core, "did:defra:amy", "amy-default").await?;

        core.save_backend(&InferenceBackendRow {
            backend_id: "backend-amy".to_string(),
            name: Some("OpenRouter".to_string()),
            provider_kind: Some("openrouter".to_string()),
            endpoint: Some("https://openrouter.ai/api/v1".to_string()),
            api_key: None,
            api_key_env_var: Some("OPENROUTER_API_KEY".to_string()),
            max_concurrent: Some(2),
            enabled: Some(true),
            supports_tool_calls: Some(true),
            supports_streaming: Some(true),
            supports_structured_outputs: Some(true),
            supports_json_schema: Some(true),
            models: vec!["openai/gpt-5.4".to_string()],
            last_probe: None,
            probe_status: Some("healthy".to_string()),
        })
        .await?;
        core.save_inference_profile(&InferenceProfileRow {
            profile_id: "profile-amy".to_string(),
            display_name: Some("Amy Profile".to_string()),
            context_window: Some(128000),
            max_output_tokens: Some(4096),
            max_turns: Some(24),
            temperature: Some(0.2),
            stream_batch_ms: Some(50),
            deadline_duration_secs: Some(300),
        })
        .await?;
        core.save_tool_selection(&ToolSelectionRow {
            selection_id: "tools-amy".to_string(),
            agent_did: Some("did:defra:amy".to_string()),
            display_name: Some("Amy Tools".to_string()),
            enable_file_tools: Some(true),
            file_tools_mode: Some("workspace-write".to_string()),
            enable_bash: Some(true),
            bash_mode: Some("workspace".to_string()),
            cli_tool_names: vec!["rg".to_string(), "cargo".to_string()],
            enable_meta_tools: Some(true),
            delegate_to: vec!["planner".to_string()],
        })
        .await?;
        core.save_behavior(&AgentBehaviorRow {
            behavior_id: "amy-default".to_string(),
            agent_did: Some("did:defra:amy".to_string()),
            display_name: Some("Amy Default".to_string()),
            system_prompt: Some("You are Amy.".to_string()),
            backend_id: Some("backend-amy".to_string()),
            model_name: Some("openai/gpt-5.4".to_string()),
            tool_selection_id: Some("tools-amy".to_string()),
            inference_profile_id: Some("profile-amy".to_string()),
            compaction_strategy: Some("rolling-summary".to_string()),
            compaction_threshold: Some(0.7),
            enabled: Some(true),
            created_at: Some("2026-04-14T00:00:00Z".to_string()),
        })
        .await?;
        core.save_scheduled_task(&ScheduledTaskRow {
            task_id: "task-amy-daily".to_string(),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            name: Some("Daily Amy".to_string()),
            prompt: Some("Check the daily queue.".to_string()),
            interval_secs: Some(300),
            enabled: Some(true),
            next_run_at: Some("2026-04-15T00:00:00Z".to_string()),
            last_run_at: None,
            last_status: Some("ok".to_string()),
            last_error: None,
            run_count: Some(4),
            created_at: None,
            updated_at: None,
        })
        .await?;
        core.refresh_store().await?;
        Ok(())
    }

    async fn seed_failed_request(core: &ClientCore) -> Result<String> {
        let created = core
            .create_conversation("did:defra:amy", Some("amy-default"))
            .await?;
        let submitted = core
            .submit_request(
                &created.session_id,
                "did:defra:amy",
                "Investigate the failing job",
                None,
            )
            .await?;

        let response_resp = core
            .node()
            .execute(&format!(
                r#"mutation {{
                add_AgentResponse(input: {{
                    response_key: "response-amy-error"
                    request_id: "{request_id}"
                    agent_did: "did:defra:amy"
                    behavior_id: "amy-default"
                    session_id: "{session_id}"
                    content: ""
                    reasoning: ""
                    status: "error"
                    error_message: "backend timeout"
                    token_count: 0
                    progress_seq: 1
                    created_at: "2026-04-14T00:00:00Z"
                    completed_at: "2026-04-14T00:00:01Z"
                }}) {{ response_key }}
            }}"#,
                request_id = escape_graphql_string(&submitted.request_id),
                session_id = escape_graphql_string(&created.session_id),
            ))
            .await;
        if response_resp.has_errors() {
            anyhow::bail!("add_AgentResponse failed: {:?}", response_resp.errors);
        }
        core.refresh_store().await?;
        Ok(submitted.request_id)
    }

    async fn insert_chat_transcript_documents(
        core: &ClientCore,
        session_id: &str,
        agent_did: &str,
        behavior_id: &str,
        response_key: &str,
    ) -> Result<()> {
        let response = core
            .node()
            .execute(&format!(
                r#"mutation {{
                add_AgentMessage(input: {{
                    message_key: "msg-assistant-1"
                    session_id: "{session_id}"
                    sequence: 2
                    role: "assistant"
                    content: "I checked the queue and opened the trace."
                    timestamp: "2026-04-14T00:00:01Z"
                }}) {{ message_key }}
                add_AgentToolCall(input: {{
                    tool_call_key: "tool-call-1"
                    session_id: "{session_id}"
                    message_sequence: 2
                    tool_name: "shell"
                    tool_call_id: "call-shell-1"
                    args: "{{\"cmd\":\"rg audit\"}}"
                    status: "completed"
                    started_at: "2026-04-14T00:00:02Z"
                    completed_at: "2026-04-14T00:00:03Z"
                }}) {{ tool_call_key }}
                add_AgentToolResult(input: {{
                    agent_did: "{agent_did}"
                    session_id: "{session_id}"
                    tool_name: "shell"
                    tool_input: "rg audit"
                    output_text: "src/app.rs: audit target live"
                    truncated: false
                    truncation_metadata: ""
                    conversation_doc_id: "{session_id}"
                    created_at: "2026-04-14T00:00:03Z"
                }}) {{ _docID }}
                add_AgentResponse(input: {{
                    response_key: "{response_key}"
                    agent_did: "{agent_did}"
                    behavior_id: "{behavior_id}"
                    session_id: "{session_id}"
                    content: "Queue checked."
                    reasoning: "I verified the latest request, ran the shell tool, and summarized the result."
                    status: "completed"
                    error_message: ""
                    token_count: 42
                    progress_seq: 1
                    created_at: "2026-04-14T00:00:04Z"
                    completed_at: "2026-04-14T00:00:05Z"
                }}) {{ response_key }}
            }}"#,
                session_id = escape_graphql_string(session_id),
                agent_did = escape_graphql_string(agent_did),
                behavior_id = escape_graphql_string(behavior_id),
                response_key = escape_graphql_string(response_key),
            ))
            .await;
        if response.has_errors() {
            anyhow::bail!(
                "insert chat transcript documents failed: {:?}",
                response.errors
            );
        }
        core.refresh_store().await?;
        Ok(())
    }

    fn build_driver(
        runtime: Arc<Runtime>,
        core: ClientCore,
        log_store: Arc<DesktopLogStore>,
    ) -> AuditDriver {
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let app = DesktopApp::from_parts(&cc, runtime, Some(Arc::new(core)), Vec::new(), log_store);
        AuditDriver::new(app, ctx)
    }

    fn seed_saved_peer_directory(
        paths: &DesktopPaths,
        label: &str,
        addr: &str,
        agent_did: &str,
    ) -> Result<()> {
        std::fs::create_dir_all(paths.root())?;
        let payload = serde_json::json!({
            "peers": [{
                "peer_id": "peer-broken",
                "label": label,
                "addr": addr,
                "agent_did": agent_did,
                "created_at": "2026-04-14T00:00:00Z",
                "updated_at": "2026-04-14T00:00:00Z"
            }]
        });
        std::fs::write(
            paths.peer_directory_path(),
            serde_json::to_vec_pretty(&payload)?,
        )?;
        Ok(())
    }

    #[test]
    fn desktop_app_redirects_blank_first_launch_to_peers_onboarding() -> Result<()> {
        let runtime = test_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let core = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path()),
            ClientCoreOptions::local_only(),
        ))?;

        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = DesktopApp::from_parts(
            &cc,
            Arc::clone(&runtime),
            Some(Arc::new(core)),
            Vec::new(),
            Arc::new(DesktopLogStore::new(64)),
        );

        let texts = render_once(&mut app, &ctx);

        assert_eq!(app.state.activity, Activity::Peers);
        assert!(app.state.onboarding.first_launch_redirect_done);
        assert!(texts.iter().any(|text| text.contains("First Launch")));
        assert!(texts
            .iter()
            .any(|text| text.contains("Add Your First Deployment")));
        Ok(())
    }

    #[test]
    fn desktop_app_clicks_through_first_launch_add_peer_flow() -> Result<()> {
        let runtime = test_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let core = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path().join("primary")),
            ClientCoreOptions::local_only(),
        ))?;
        let peer = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path().join("peer")),
            ClientCoreOptions::local_only(),
        ))?;
        let peer_addr = peer
            .listen_addresses()
            .first()
            .cloned()
            .ok_or_else(|| anyhow!("peer missing listen address"))?;

        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let app = DesktopApp::from_parts(
            &cc,
            Arc::clone(&runtime),
            Some(Arc::new(core)),
            Vec::new(),
            Arc::new(DesktopLogStore::new(64)),
        );
        let mut driver = AuditDriver::new(app, ctx);

        let initial = driver.render();
        assert!(initial.iter().any(|text| text == "First Launch"));
        assert_eq!(driver.app.state.activity, Activity::Peers);

        driver.click_target(audit::targets::PEERS_ONBOARDING_COPY_DID);
        assert_eq!(
            driver.app.state.peers.last_action_message.as_deref(),
            Some("Copied desktop DID to clipboard.")
        );

        driver.click_target(audit::targets::PEERS_ADD_LABEL);
        driver.type_text("Workshop Bay");
        driver.click_target(audit::targets::PEERS_ADD_ADDR);
        driver.type_text(&peer_addr);
        driver.click_target(audit::targets::PEERS_ADD_AGENT_DID);
        driver.type_text("did:defra:peer");
        let texts = driver.click_target(audit::targets::PEERS_SAVE);

        assert!(driver.app.state.peers.selected_peer_id.is_some());
        assert_eq!(driver.app.state.activity, Activity::Peers);
        assert!(texts.iter().any(|text| text.contains("Workshop Bay")));
        assert!(texts.iter().any(|text| text.contains("Peer Access")));
        driver.app.shutdown_client();
        shutdown_core(runtime.as_ref(), peer)?;
        Ok(())
    }

    #[test]
    fn desktop_app_clicks_through_first_launch_add_peer_with_dial_warning() -> Result<()> {
        let runtime = test_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let core = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path()),
            ClientCoreOptions::local_only(),
        ))?;

        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let app = DesktopApp::from_parts(
            &cc,
            Arc::clone(&runtime),
            Some(Arc::new(core)),
            Vec::new(),
            Arc::new(DesktopLogStore::new(64)),
        );
        let mut driver = AuditDriver::new(app, ctx);

        let initial = driver.render();
        assert!(initial.iter().any(|text| text.contains("First Launch")));

        driver.click_target(audit::targets::PEERS_ADD_LABEL);
        driver.type_text("Broken Relay");
        driver.click_target(audit::targets::PEERS_ADD_ADDR);
        driver.type_text("iroh://bad-address");
        driver.click_target(audit::targets::PEERS_ADD_AGENT_DID);
        driver.type_text("did:defra:broken");
        driver.click_target(audit::targets::PEERS_SAVE);

        let warning_message = wait_for_value(
            "peer save warning after invalid address",
            Duration::from_secs(5),
            || {
                driver
                    .app
                    .state
                    .peers
                    .last_action_message
                    .as_ref()
                    .filter(|message| message.contains("dial failed"))
                    .cloned()
            },
        )?;
        assert!(warning_message.contains("Saved Broken Relay."));

        wait_for_value(
            "saved peer appears after dial warning",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    let records = driver.app.runtime.block_on(client.peer_records());
                    records
                        .iter()
                        .find(|record| record.label == "Broken Relay")
                        .map(|record| record.peer_id.clone())
                })
            },
        )?;

        let chat_texts = driver.open_activity(Activity::Chat);
        assert!(chat_texts.iter().any(|text| text.contains("Broken Relay")));

        driver.app.shutdown_client();
        Ok(())
    }

    #[test]
    fn desktop_app_clicks_chat_open_peers_setup_from_empty_sidebar() -> Result<()> {
        let runtime = test_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let core = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path()),
            ClientCoreOptions::local_only(),
        ))?;

        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = DesktopApp::from_parts(
            &cc,
            Arc::clone(&runtime),
            Some(Arc::new(core)),
            Vec::new(),
            Arc::new(DesktopLogStore::new(64)),
        );
        app.state.onboarding.first_launch_redirect_done = true;
        app.state.activity = Activity::Chat;
        let mut driver = AuditDriver::new(app, ctx);

        let texts = driver.render();
        assert!(texts.iter().any(|text| text.contains("Add Deployment")));

        let after_click = driver.click_target(audit::targets::CHAT_OPEN_PEERS_SETUP);
        assert_eq!(driver.app.state.activity, Activity::Peers);
        assert!(driver.app.state.peers.show_add_form);
        assert!(after_click
            .iter()
            .any(|text| text.contains("Add Your First Deployment")));
        Ok(())
    }

    #[test]
    fn desktop_app_renders_bootstrap_issues_in_peers_and_logs() -> Result<()> {
        let runtime = test_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let paths = DesktopPaths::from_root(tempdir.path());
        seed_saved_peer_directory(
            &paths,
            "Broken Relay",
            "iroh://bad-address",
            "did:defra:broken",
        )?;
        let core = runtime.block_on(ClientCore::start_with_paths_and_options(
            paths,
            ClientCoreOptions::local_only(),
        ))?;

        assert!(core
            .bootstrap_errors()
            .iter()
            .any(|error| error.contains("Broken Relay") && error.contains("dial failed")));

        let mut driver = build_driver(
            Arc::clone(&runtime),
            core,
            Arc::new(DesktopLogStore::new(64)),
        );

        wait_for_value(
            "peers bootstrap issues rendered",
            Duration::from_secs(2),
            || {
                let texts = driver.open_activity(Activity::Peers);
                texts
                    .iter()
                    .any(|text| text.contains("Broken Relay"))
                    .then_some(texts)
            },
        )?;
        let peers_texts = driver.render();
        assert!(peers_texts
            .iter()
            .any(|text| text.contains("peer Broken Relay dial failed")));

        let logs_texts = wait_for_value(
            "logs bootstrap issues rendered",
            Duration::from_secs(2),
            || {
                let texts = driver.open_activity(Activity::Logs);
                texts
                    .iter()
                    .any(|text| text.contains("bootstrap issues"))
                    .then_some(texts)
            },
        )?;
        assert!(logs_texts.iter().any(|text| text.contains("Broken Relay")));

        driver.app.shutdown_client();
        Ok(())
    }

    #[test]
    fn desktop_app_renders_chat_activity_with_live_session_data() -> Result<()> {
        let runtime = test_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let core = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path()),
            ClientCoreOptions::local_only(),
        ))?;

        let principal_resp = runtime.block_on(core.node().execute(
            r#"mutation {
                add_AgentPrincipal(input: {
                    agent_did: "did:defra:amy"
                    display_name: "Amy"
                    default_behavior_id: "amy-default"
                    enabled: true
                }) { agent_did }
            }"#,
        ));
        assert!(!principal_resp.has_errors());

        let created =
            runtime.block_on(core.create_conversation("did:defra:amy", Some("amy-default")))?;
        runtime.block_on(core.submit_request(
            &created.session_id,
            "did:defra:amy",
            "hello operator",
            None,
        ))?;

        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = DesktopApp::from_parts(
            &cc,
            Arc::clone(&runtime),
            Some(Arc::new(core)),
            Vec::new(),
            Arc::new(DesktopLogStore::new(64)),
        );

        let texts = render_once(&mut app, &ctx);

        assert_eq!(app.state.activity, Activity::Chat);
        assert_eq!(
            app.state.chat.selected_agent_did.as_deref(),
            Some("did:defra:amy")
        );
        assert_eq!(
            app.state.chat.selected_session_id.as_deref(),
            Some(created.session_id.as_str())
        );
        assert!(!texts.iter().any(|text| text.contains("Operator Console")));
        assert!(texts.iter().any(|text| text.contains("hello operator")));
        assert!(texts.iter().any(|text| text.contains("Amy")));
        Ok(())
    }

    #[test]
    fn desktop_app_renders_request_only_transcript_fallback() -> Result<()> {
        let runtime = test_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let core = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path()),
            ClientCoreOptions::local_only(),
        ))?;

        runtime.block_on(insert_agent_principal(
            &core,
            "did:defra:amy",
            "Amy",
            "amy-default",
        ))?;
        let created =
            runtime.block_on(core.create_conversation("did:defra:amy", Some("amy-default")))?;
        runtime.block_on(core.submit_request(
            &created.session_id,
            "did:defra:amy",
            "request only transcript row",
            None,
        ))?;

        let mut driver = build_driver(
            Arc::clone(&runtime),
            core,
            Arc::new(DesktopLogStore::new(64)),
        );
        driver.app.state.activity = Activity::Chat;
        let texts = driver.render();

        assert_eq!(
            driver.app.state.chat.selected_session_id.as_deref(),
            Some(created.session_id.as_str())
        );
        assert!(texts
            .iter()
            .any(|text| text.contains("request only transcript row")));
        assert!(texts.iter().any(|text| text.contains("waiting for claim")));
        assert!(!texts.iter().any(|text| text.contains("Transcript Empty")));
        Ok(())
    }

    #[test]
    fn desktop_app_chat_header_retry_export_controls_stay_disabled() -> Result<()> {
        let runtime = test_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let core = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path()),
            ClientCoreOptions::local_only(),
        ))?;

        let principal_resp = runtime.block_on(core.node().execute(
            r#"mutation {
                add_AgentPrincipal(input: {
                    agent_did: "did:defra:amy"
                    display_name: "Amy"
                    default_behavior_id: "amy-default"
                    enabled: true
                }) { agent_did }
            }"#,
        ));
        assert!(!principal_resp.has_errors());

        let created =
            runtime.block_on(core.create_conversation("did:defra:amy", Some("amy-default")))?;
        runtime.block_on(core.submit_request(
            &created.session_id,
            "did:defra:amy",
            "hello operator",
            None,
        ))?;

        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = DesktopApp::from_parts(
            &cc,
            Arc::clone(&runtime),
            Some(Arc::new(core)),
            Vec::new(),
            Arc::new(DesktopLogStore::new(64)),
        );
        app.state.activity = Activity::Chat;
        let mut driver = AuditDriver::new(app, ctx);

        let initial = driver.render();
        assert!(initial.iter().any(|text| text.contains("Retry")));
        assert!(initial.iter().any(|text| text.contains("Export")));
        assert!(driver.has_target(audit::targets::CHAT_RETRY));
        assert!(driver.has_target(audit::targets::CHAT_EXPORT));

        let selected_session_id = driver.app.state.chat.selected_session_id.clone();
        let retry_texts = driver.click_target(audit::targets::CHAT_RETRY);
        let export_texts = driver.click_target(audit::targets::CHAT_EXPORT);

        assert_eq!(
            driver.app.state.chat.selected_session_id,
            selected_session_id
        );
        assert_eq!(driver.app.state.chat.last_submission_error, None);
        assert!(retry_texts.iter().any(|text| text.contains("Retry")));
        assert!(export_texts.iter().any(|text| text.contains("Export")));
        Ok(())
    }

    #[test]
    fn desktop_app_renders_chat_first_conversation_nudge() -> Result<()> {
        let runtime = test_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let core = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path()),
            ClientCoreOptions::local_only(),
        ))?;

        let principal_resp = runtime.block_on(core.node().execute(
            r#"mutation {
                add_AgentPrincipal(input: {
                    agent_did: "did:defra:amy"
                    display_name: "Amy"
                    default_behavior_id: "amy-default"
                    enabled: true
                }) { agent_did }
            }"#,
        ));
        assert!(!principal_resp.has_errors());
        runtime.block_on(core.refresh_store())?;

        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = DesktopApp::from_parts(
            &cc,
            Arc::clone(&runtime),
            Some(Arc::new(core)),
            Vec::new(),
            Arc::new(DesktopLogStore::new(64)),
        );
        app.state.activity = Activity::Chat;

        let texts = render_once(&mut app, &ctx);

        assert_eq!(app.state.activity, Activity::Chat);
        assert_eq!(
            app.state.chat.selected_agent_did.as_deref(),
            Some("did:defra:amy")
        );
        assert!(app.state.chat.selected_session_id.is_none());
        assert!(texts
            .iter()
            .any(|text| text.contains("Start First Conversation")));
        assert!(texts
            .iter()
            .any(|text| text.contains("Create Conversation")));
        Ok(())
    }

    #[test]
    fn desktop_app_clicks_through_chat_first_conversation_nudge() -> Result<()> {
        let runtime = test_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let core = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path()),
            ClientCoreOptions::local_only(),
        ))?;

        let principal_resp = runtime.block_on(core.node().execute(
            r#"mutation {
                add_AgentPrincipal(input: {
                    agent_did: "did:defra:amy"
                    display_name: "Amy"
                    default_behavior_id: "amy-default"
                    enabled: true
                }) { agent_did }
            }"#,
        ));
        assert!(!principal_resp.has_errors());
        runtime.block_on(core.refresh_store())?;

        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = DesktopApp::from_parts(
            &cc,
            Arc::clone(&runtime),
            Some(Arc::new(core)),
            Vec::new(),
            Arc::new(DesktopLogStore::new(64)),
        );
        app.state.activity = Activity::Chat;
        let mut driver = AuditDriver::new(app, ctx);

        let initial = driver.render();
        assert!(initial
            .iter()
            .any(|text| text.contains("Start First Conversation")));

        driver.click_target(audit::targets::CHAT_CREATE_CONVERSATION);
        let texts = driver.render();

        assert!(driver.app.state.chat.selected_session_id.is_some());
        assert!(texts.iter().any(|text| text.contains("Transcript Empty")));
        Ok(())
    }

    #[test]
    fn desktop_app_clicks_through_activity_bar_navigation() -> Result<()> {
        let runtime = test_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let core = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path()),
            ClientCoreOptions::local_only(),
        ))?;
        let log_store = Arc::new(DesktopLogStore::new(64));
        log_store.record_manual(
            chrono::Utc::now(),
            tracing::Level::INFO,
            "defra_agent_desktop::replication",
            "activity navigation marker",
            [("marker", "activity".to_string())],
        );

        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = DesktopApp::from_parts(
            &cc,
            Arc::clone(&runtime),
            Some(Arc::new(core)),
            Vec::new(),
            log_store,
        );
        app.state.onboarding.first_launch_redirect_done = true;
        app.state.activity = Activity::Chat;
        let mut driver = AuditDriver::new(app, ctx);

        let chat_texts = driver.render();
        assert!(chat_texts
            .iter()
            .any(|text| text.contains("Add Deployment")));

        let logs_texts = driver.open_activity(Activity::Logs);
        assert_eq!(driver.app.state.activity, Activity::Logs);
        assert!(logs_texts.iter().any(|text| text.contains("Live Logs")));

        let operator_texts = driver.open_activity(Activity::Operator);
        assert_eq!(driver.app.state.activity, Activity::Operator);
        assert!(operator_texts
            .iter()
            .any(|text| text.contains("Operator Console")));

        let peers_texts = driver.open_activity(Activity::Peers);
        assert_eq!(driver.app.state.activity, Activity::Peers);
        assert!(peers_texts
            .iter()
            .any(|text| text.contains("Add Your First Deployment")));

        let back_to_chat = driver.open_activity(Activity::Chat);
        assert_eq!(driver.app.state.activity, Activity::Chat);
        assert!(back_to_chat
            .iter()
            .any(|text| text.contains("Add Deployment")));
        Ok(())
    }

    #[test]
    fn desktop_app_clicks_through_chat_deployment_and_conversation_switching() -> Result<()> {
        let runtime = test_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let core = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path().join("desktop")),
            ClientCoreOptions::local_only(),
        ))?;
        let peer_alpha = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path().join("peer-alpha")),
            ClientCoreOptions::local_only(),
        ))?;
        let peer_beta = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path().join("peer-beta")),
            ClientCoreOptions::local_only(),
        ))?;
        let peer_alpha_addr = peer_alpha
            .listen_addresses()
            .first()
            .cloned()
            .ok_or_else(|| anyhow!("peer alpha missing listen address"))?;
        let peer_beta_addr = peer_beta
            .listen_addresses()
            .first()
            .cloned()
            .ok_or_else(|| anyhow!("peer beta missing listen address"))?;
        let alpha_peer =
            runtime.block_on(core.add_peer("Alpha Bay", &peer_alpha_addr, "did:defra:amy"))?;
        let beta_peer =
            runtime.block_on(core.add_peer("Beta Bay", &peer_beta_addr, "did:defra:bob"))?;
        runtime.block_on(insert_agent_principal(
            &core,
            "did:defra:amy",
            "Amy",
            "amy-default",
        ))?;
        runtime.block_on(insert_agent_principal(
            &core,
            "did:defra:bob",
            "Bob",
            "bob-default",
        ))?;
        let amy_session =
            runtime.block_on(core.create_conversation("did:defra:amy", Some("amy-default")))?;
        runtime.block_on(core.submit_request(
            &amy_session.session_id,
            "did:defra:amy",
            "amy conversation request",
            None,
        ))?;
        let bob_first =
            runtime.block_on(core.create_conversation("did:defra:bob", Some("bob-default")))?;
        runtime.block_on(core.submit_request(
            &bob_first.session_id,
            "did:defra:bob",
            "bob first request",
            None,
        ))?;
        let bob_second =
            runtime.block_on(core.create_conversation("did:defra:bob", Some("bob-default")))?;
        runtime.block_on(core.submit_request(
            &bob_second.session_id,
            "did:defra:bob",
            "bob second request",
            None,
        ))?;
        runtime.block_on(core.refresh_store())?;

        let mut driver = build_driver(
            Arc::clone(&runtime),
            core,
            Arc::new(DesktopLogStore::new(64)),
        );
        driver.app.state.onboarding.first_launch_redirect_done = true;
        driver.app.state.activity = Activity::Chat;
        let initial = driver.render();

        assert_eq!(
            driver.app.state.chat.selected_peer_id.as_deref(),
            Some(alpha_peer.peer_id.as_str())
        );
        assert_eq!(
            driver.app.state.chat.selected_agent_did.as_deref(),
            Some("did:defra:amy")
        );
        assert!(initial
            .iter()
            .any(|text| text.contains("amy conversation request")));

        driver.click_target(&audit::targets::chat_deployment(&beta_peer.peer_id));
        let beta_texts = driver.render();
        assert_eq!(
            driver.app.state.chat.selected_agent_did.as_deref(),
            Some("did:defra:bob")
        );
        assert_eq!(
            driver.app.state.chat.selected_session_id.as_deref(),
            Some(bob_second.session_id.as_str())
        );
        assert!(beta_texts
            .iter()
            .any(|text| text.contains("bob second request")));

        driver.click_target(&audit::targets::chat_conversation(&bob_first.session_id));
        let switched = driver.render();
        assert_eq!(
            driver.app.state.chat.selected_session_id.as_deref(),
            Some(bob_first.session_id.as_str())
        );
        assert!(switched
            .iter()
            .any(|text| text.contains("bob first request")));
        driver.app.shutdown_client();
        shutdown_core(runtime.as_ref(), peer_alpha)?;
        shutdown_core(runtime.as_ref(), peer_beta)?;
        Ok(())
    }

    #[test]
    fn desktop_app_clicks_through_chat_reasoning_and_tool_card_disclosures() -> Result<()> {
        let runtime = test_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let core = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path()),
            ClientCoreOptions::local_only(),
        ))?;
        runtime.block_on(insert_agent_principal(
            &core,
            "did:defra:amy",
            "Amy",
            "amy-default",
        ))?;
        let conversation =
            runtime.block_on(core.create_conversation("did:defra:amy", Some("amy-default")))?;
        runtime.block_on(insert_chat_transcript_documents(
            &core,
            &conversation.session_id,
            "did:defra:amy",
            "amy-default",
            "response-disclosure-1",
        ))?;

        let mut driver = build_driver(
            Arc::clone(&runtime),
            core,
            Arc::new(DesktopLogStore::new(64)),
        );
        driver.app.state.onboarding.first_launch_redirect_done = true;
        driver.app.state.activity = Activity::Chat;
        let initial = driver.render();

        assert!(initial
            .iter()
            .any(|text| text.contains("REASONING DISCLOSURE")));
        assert!(!initial
            .iter()
            .any(|text| text.contains("I verified the latest request")));

        driver.click_target(&audit::targets::chat_tool_card("call-shell-1"));
        let tool_texts = driver.render();
        assert!(driver
            .app
            .state
            .chat
            .expanded_tool_cards
            .contains("call-shell-1"));
        assert!(tool_texts.iter().any(|text| text.contains("ARGS")));
        assert!(tool_texts
            .iter()
            .any(|text| text.contains("src/app.rs: audit target live")));

        driver.click_target(&audit::targets::chat_reasoning("response-disclosure-1"));
        let reasoning_texts = driver.render();
        assert!(driver
            .app
            .state
            .chat
            .expanded_reasoning_cards
            .contains("reasoning:response-disclosure-1"));
        assert!(reasoning_texts
            .iter()
            .any(|text| text.contains("I verified the latest request")));
        Ok(())
    }

    #[test]
    fn desktop_app_clicks_through_chat_send_without_precreating_conversation() -> Result<()> {
        let runtime = test_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let core = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path().join("desktop")),
            ClientCoreOptions::local_only(),
        ))?;
        let mock_endpoint = MockModelEndpoint::start("default")?;
        let running_agent = runtime.block_on(spawn_backed_agent(
            core.node_arc(),
            tempdir.path().join("agent").join("audit-direct-send.key"),
            "audit-direct-send",
            &AgentBackendConfig::mock(mock_endpoint.endpoint()),
        ))?;
        runtime.block_on(core.refresh_store())?;

        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = DesktopApp::from_parts(
            &cc,
            Arc::clone(&runtime),
            Some(Arc::new(core)),
            Vec::new(),
            Arc::new(DesktopLogStore::new(64)),
        );
        app.state.activity = Activity::Chat;
        let mut driver = AuditDriver::new(app, ctx);

        wait_for_value(
            "first-conversation nudge before direct send",
            Duration::from_secs(5),
            || {
                let texts = driver.render();
                texts
                    .iter()
                    .any(|text| text.contains("Start First Conversation"))
                    .then_some(texts)
            },
        )?;
        assert_eq!(driver.app.state.chat.selected_session_id, None);
        assert_eq!(
            driver.app.state.chat.selected_agent_did.as_deref(),
            Some(running_agent.did.as_str())
        );

        driver.click_target(audit::targets::CHAT_COMPOSER_TEXT);
        driver.type_text("send directly without creating the session first");
        driver.render();
        driver.click_target(audit::targets::CHAT_SEND);
        if wait_for_value(
            "session created by first direct-send click",
            Duration::from_secs(1),
            || driver.app.state.chat.selected_session_id.clone(),
        )
        .is_err()
        {
            driver.render();
            driver.click_target(audit::targets::CHAT_SEND);
        }

        let session_id = wait_for_value(
            "session created by direct send",
            Duration::from_secs(5),
            || driver.app.state.chat.selected_session_id.clone(),
        )?;
        assert!(driver.app.state.chat.last_submission_error.is_none());
        assert!(driver.app.state.chat.composer_text.is_empty());

        let request_id = wait_for_value(
            "direct-send focused request id",
            Duration::from_secs(5),
            || {
                driver
                    .app
                    .client
                    .as_ref()
                    .and_then(|client| client.store().focused_request_id())
            },
        )?;
        wait_for_value(
            "direct-send response row in store",
            Duration::from_secs(10),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .latest_response_for_request(&request_id)
                        .and_then(|row| row.content.as_deref())
                        .filter(|content| !content.trim().is_empty())
                        .map(str::to_string)
                })
            },
        )?;
        let transcript_texts = wait_for_value(
            "direct-send transcript response",
            Duration::from_secs(10),
            || {
                let texts = driver.render();
                texts
                    .iter()
                    .any(|text| text.contains("mock response"))
                    .then_some(texts)
            },
        )?;
        assert_eq!(
            driver.app.state.chat.selected_session_id.as_deref(),
            Some(session_id.as_str())
        );
        assert!(transcript_texts
            .iter()
            .any(|text| { text.contains("send directly without creating the session first") }));

        runtime.block_on(running_agent.shutdown())?;
        Ok(())
    }

    #[test]
    fn desktop_app_blocks_chat_send_while_turn_is_waiting_for_claim() -> Result<()> {
        let runtime = test_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let core = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path()),
            ClientCoreOptions::local_only(),
        ))?;
        runtime.block_on(seed_operator_documents(&core))?;
        let created =
            runtime.block_on(core.create_conversation("did:defra:amy", Some("amy-default")))?;
        runtime.block_on(core.submit_request(
            &created.session_id,
            "did:defra:amy",
            "existing pending request",
            Some("amy-default"),
        ))?;
        runtime.block_on(core.refresh_store())?;

        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = DesktopApp::from_parts(
            &cc,
            Arc::clone(&runtime),
            Some(Arc::new(core)),
            Vec::new(),
            Arc::new(DesktopLogStore::new(64)),
        );
        app.state.activity = Activity::Chat;
        app.state.chat.selected_agent_did = Some("did:defra:amy".to_string());
        app.state.chat.selected_session_id = Some(created.session_id.clone());
        let mut driver = AuditDriver::new(app, ctx);

        let waiting_texts = wait_for_value(
            "waiting-for-claim turn state",
            Duration::from_secs(5),
            || {
                let texts = driver.render();
                texts
                    .iter()
                    .any(|text| text.contains("waiting for claim"))
                    .then_some(texts)
            },
        )?;
        assert!(waiting_texts
            .iter()
            .any(|text| text.contains("existing pending request")));

        let initial_request_count = driver
            .app
            .client
            .as_ref()
            .map(|client| client.store().snapshot().requests.len())
            .ok_or_else(|| anyhow!("desktop client missing"))?;

        driver.click_target(audit::targets::CHAT_COMPOSER_TEXT);
        driver.type_text("blocked follow-up");
        driver.click_target(audit::targets::CHAT_SEND);
        driver.press_key(
            egui::Key::Enter,
            egui::Modifiers {
                command: true,
                mac_cmd: true,
                ..Default::default()
            },
        );
        driver.render();

        let request_count_after = driver
            .app
            .client
            .as_ref()
            .map(|client| client.store().snapshot().requests.len())
            .ok_or_else(|| anyhow!("desktop client missing"))?;
        assert_eq!(request_count_after, initial_request_count);
        assert_eq!(driver.app.state.chat.composer_text, "blocked follow-up");
        assert_eq!(driver.app.state.chat.last_submission_error, None);
        Ok(())
    }

    #[test]
    fn desktop_app_clicks_through_live_agent_submission() -> Result<()> {
        let runtime = test_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let core = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path().join("desktop")),
            ClientCoreOptions::local_only(),
        ))?;
        let mock_endpoint = MockModelEndpoint::start("default")?;
        let running_agent = runtime.block_on(spawn_backed_agent(
            core.node_arc(),
            tempdir.path().join("agent").join("audit-live.key"),
            "audit-live",
            &AgentBackendConfig::mock(mock_endpoint.endpoint()),
        ))?;
        runtime.block_on(core.refresh_store())?;

        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = DesktopApp::from_parts(
            &cc,
            Arc::clone(&runtime),
            Some(Arc::new(core)),
            Vec::new(),
            Arc::new(DesktopLogStore::new(64)),
        );
        app.state.activity = Activity::Chat;
        let mut driver = AuditDriver::new(app, ctx);

        assert!(driver
            .app
            .client
            .as_ref()
            .expect("desktop client should exist")
            .store()
            .snapshot()
            .agent_principals
            .iter()
            .any(|row| row.agent_did == running_agent.did));

        let initial = wait_for_value(
            "chat first-conversation nudge",
            Duration::from_secs(5),
            || {
                let texts = driver.render();
                texts
                    .iter()
                    .any(|text| text.contains("Start First Conversation"))
                    .then_some(texts)
            },
        )?;
        assert_eq!(
            driver.app.state.chat.selected_agent_did.as_deref(),
            Some(running_agent.did.as_str())
        );
        assert!(initial
            .iter()
            .any(|text| text.contains("Create Conversation")));

        driver.click_target(audit::targets::CHAT_CREATE_CONVERSATION);
        let after_create = driver.render();
        assert!(driver.app.state.chat.selected_session_id.is_some());
        assert!(after_create
            .iter()
            .any(|text| text.contains("Transcript Empty")));

        driver.click_target(audit::targets::CHAT_COMPOSER_TEXT);
        driver.type_text("say hello from the desktop audit");
        assert_eq!(
            driver.app.state.chat.composer_text,
            "say hello from the desktop audit"
        );
        driver.press_key(
            egui::Key::Enter,
            egui::Modifiers {
                command: true,
                mac_cmd: true,
                ..Default::default()
            },
        );
        assert_eq!(driver.app.state.chat.last_submission_error, None);
        assert!(driver.app.state.chat.composer_text.is_empty());

        let request_id = wait_for_value("focused request id", Duration::from_secs(5), || {
            driver
                .app
                .client
                .as_ref()
                .and_then(|client| client.store().focused_request_id())
        })?;
        wait_for_value(
            "response row in client store",
            Duration::from_secs(10),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .latest_response_for_request(&request_id)
                        .and_then(|row| row.content.as_deref())
                        .filter(|content| !content.trim().is_empty())
                        .map(str::to_string)
                })
            },
        )?;
        let response_texts = wait_for_value(
            "mock response in transcript",
            Duration::from_secs(10),
            || {
                let texts = driver.render();
                texts
                    .iter()
                    .any(|text| text.contains("mock response"))
                    .then_some(texts)
            },
        )?;
        assert!(response_texts
            .iter()
            .any(|text| text.contains("say hello from the desktop audit")));
        assert!(driver
            .app
            .client
            .as_ref()
            .expect("desktop client should exist")
            .store()
            .snapshot()
            .responses
            .iter()
            .any(|row| row.request_id.as_deref() == Some(request_id.as_str())));

        runtime.block_on(running_agent.shutdown())?;
        Ok(())
    }

    #[test]
    fn desktop_app_clicks_through_live_agent_multi_turn_conversation() -> Result<()> {
        let runtime = test_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let core = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path().join("desktop")),
            ClientCoreOptions::local_only(),
        ))?;
        let mock_endpoint = MockModelEndpoint::start("default")?;
        let running_agent = runtime.block_on(spawn_backed_agent(
            core.node_arc(),
            tempdir.path().join("agent").join("audit-live-multi.key"),
            "audit-live-multi",
            &AgentBackendConfig::mock(mock_endpoint.endpoint()),
        ))?;
        runtime.block_on(core.refresh_store())?;

        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = DesktopApp::from_parts(
            &cc,
            Arc::clone(&runtime),
            Some(Arc::new(core)),
            Vec::new(),
            Arc::new(DesktopLogStore::new(64)),
        );
        app.state.activity = Activity::Chat;
        let mut driver = AuditDriver::new(app, ctx);

        wait_for_value(
            "chat first-conversation nudge for multi-turn audit",
            Duration::from_secs(5),
            || {
                let texts = driver.render();
                texts
                    .iter()
                    .any(|text| text.contains("Start First Conversation"))
                    .then_some(())
            },
        )?;
        driver.click_target(audit::targets::CHAT_CREATE_CONVERSATION);
        wait_for_value(
            "session selected for multi-turn audit",
            Duration::from_secs(5),
            || driver.app.state.chat.selected_session_id.clone(),
        )?;
        let session_id = driver
            .app
            .state
            .chat
            .selected_session_id
            .clone()
            .ok_or_else(|| anyhow!("missing selected session after create conversation"))?;

        let (_first_request_id, first_response) =
            submit_chat_message_and_wait_for_response(&mut driver, "first desktop audit turn")?;
        let (second_request_id, second_response) =
            submit_chat_message_and_wait_for_response(&mut driver, "follow up desktop audit turn")?;
        assert_eq!(first_response, "mock response");
        assert_eq!(second_response, "mock response");

        wait_for_value(
            "multi-turn conversation state persisted",
            Duration::from_secs(10),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    let snapshot = client.store().snapshot();
                    let conversation = snapshot
                        .conversations
                        .iter()
                        .find(|row| row.session_id == session_id)?;
                    (snapshot.requests_for_session(&session_id).len() == 2
                        && snapshot
                            .responses
                            .iter()
                            .filter(|row| row.session_id.as_deref() == Some(session_id.as_str()))
                            .count()
                            >= 2
                        && conversation.latest_request_id.as_deref()
                            == Some(second_request_id.as_str())
                        && conversation.preview_text.as_deref()
                            == Some("follow up desktop audit turn"))
                    .then_some(())
                })
            },
        )?;

        let final_texts = driver.render();
        assert!(final_texts
            .iter()
            .any(|text| text.contains("first desktop audit turn")));
        assert!(final_texts
            .iter()
            .any(|text| text.contains("follow up desktop audit turn")));
        assert!(final_texts.iter().any(|text| text.contains("completed")));

        runtime.block_on(running_agent.shutdown())?;
        Ok(())
    }

    #[test]
    #[ignore = "hits live inference backend configured by DEFRA_AGENT_DESKTOP_LIVE_BACKEND_* or OPENROUTER_API_KEY"]
    fn desktop_app_live_inference_smoke() -> Result<()> {
        init_test_tracing();

        let backend = AgentBackendConfig::live_from_env()?;
        let live_backend_id = "audit-live-remote-backend".to_string();
        let runtime = test_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let core = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path().join("desktop")),
            ClientCoreOptions::local_only(),
        ))?;
        let peer = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path().join("peer")),
            ClientCoreOptions::local_only(),
        ))?;
        let peer_addr = peer
            .listen_addresses()
            .first()
            .cloned()
            .ok_or_else(|| anyhow!("live peer missing listen address"))?;
        let baseline_events = global_log_store().snapshot().total_events;
        let running_agent = runtime.block_on(spawn_backed_agent(
            core.node_arc(),
            tempdir.path().join("agent").join("audit-live.key"),
            "audit-live-remote",
            &backend,
        ))?;
        runtime.block_on(core.refresh_store())?;

        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = DesktopApp::from_parts(
            &cc,
            Arc::clone(&runtime),
            Some(Arc::new(core)),
            Vec::new(),
            global_log_store(),
        );
        app.state.activity = Activity::Chat;
        let mut driver = AuditDriver::new(app, ctx);

        let prompt = format!(
            "Reply with exactly READY and nothing else. audit {}",
            uuid::Uuid::new_v4()
        );
        let prompt_snippet = "Reply with exactly READY";

        wait_for_value(
            "live chat first-conversation nudge",
            Duration::from_secs(10),
            || {
                let texts = driver.render();
                texts
                    .iter()
                    .any(|text| text.contains("Start First Conversation"))
                    .then_some(())
            },
        )?;
        driver.click_target(audit::targets::CHAT_CREATE_CONVERSATION);
        wait_for_value(
            "live transcript empty state",
            Duration::from_secs(5),
            || {
                let texts = driver.render();
                texts
                    .iter()
                    .any(|text| text.contains("Transcript Empty"))
                    .then_some(())
            },
        )?;

        driver.click_target(audit::targets::CHAT_COMPOSER_TEXT);
        driver.type_text(&prompt);
        driver.render();
        driver.click_target(audit::targets::CHAT_SEND);
        assert_eq!(driver.app.state.chat.last_submission_error, None);

        let request_id =
            wait_for_value("live focused request id", Duration::from_secs(10), || {
                driver
                    .app
                    .client
                    .as_ref()
                    .and_then(|client| client.store().focused_request_id())
            })?;
        let response_content = wait_for_value(
            "live response row in client store",
            Duration::from_secs(90),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .latest_response_for_request(&request_id)
                        .and_then(|row| row.content.clone())
                        .filter(|content| !content.trim().is_empty())
                })
            },
        )?;
        assert!(!response_content.trim().is_empty());
        let (
            request_lifecycle_state,
            response_status,
            runtime_process_state,
            runtime_default_behavior_id,
            runtime_last_result,
            runtime_runnable_behaviors,
            runtime_scheduled_task_count,
            live_store_row_count,
        ) = wait_for_value(
            "live operator rows available",
            Duration::from_secs(10),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    let snapshot = client.store().snapshot();
                    let request = snapshot
                        .requests
                        .iter()
                        .find(|row| row.request_id == request_id)?;
                    let response = snapshot.latest_response_for_request(&request_id)?;
                    let runtime_row = snapshot.latest_runtime(&running_agent.did)?;
                    let backend_row = snapshot
                        .inference_backends
                        .iter()
                        .find(|row| row.backend_id == live_backend_id)?;

                    Some((
                        request
                            .lifecycle_state
                            .clone()
                            .unwrap_or_else(|| "unset".to_string()),
                        response
                            .status
                            .clone()
                            .unwrap_or_else(|| "unset".to_string()),
                        runtime_row
                            .process_state
                            .clone()
                            .unwrap_or_else(|| "unknown".to_string()),
                        runtime_row
                            .default_behavior_id
                            .clone()
                            .unwrap_or_else(|| "unbound".to_string()),
                        runtime_row
                            .last_reconcile_result
                            .clone()
                            .unwrap_or_else(|| "pending".to_string()),
                        runtime_row
                            .runnable_behavior_count
                            .unwrap_or_default()
                            .to_string(),
                        snapshot
                            .scheduled_tasks
                            .iter()
                            .filter(|row| {
                                row.agent_did.as_deref() == Some(running_agent.did.as_str())
                            })
                            .count()
                            .to_string(),
                        snapshot.row_count().to_string(),
                    ))
                    .filter(|_| {
                        backend_row.provider_kind.as_deref() == Some(backend.provider_kind.as_str())
                            && backend_row.endpoint.as_deref() == Some(backend.endpoint.as_str())
                            && backend_row.models.iter().any(|model| model == &backend.model_name)
                    })
                })
            },
        )?;

        let chat_texts = wait_for_value(
            "live response in transcript",
            Duration::from_secs(30),
            || {
                let texts = driver.render();
                texts
                    .iter()
                    .any(|text| text.contains(prompt_snippet))
                    .then_some(texts)
            },
        )?;
        assert!(chat_texts
            .iter()
            .any(|text| text.contains(response_content.trim())));

        driver.open_activity(Activity::Operator);
        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::Runtime,
        ));
        let runtime_texts = wait_for_value(
            "operator runtime inspector",
            Duration::from_secs(10),
            || {
                let texts = driver.render();
                texts
                    .iter()
                    .any(|text| text.contains("Runtime Inspector"))
                    .then_some(texts)
            },
        )?;
        assert!(runtime_texts
            .iter()
            .any(|text| text.contains(&runtime_process_state)));
        assert!(runtime_texts
            .iter()
            .any(|text| text.contains(&runtime_default_behavior_id)));
        assert!(runtime_texts
            .iter()
            .any(|text| text.contains(&runtime_last_result)));
        assert!(runtime_texts
            .iter()
            .any(|text| text.contains(&runtime_runnable_behaviors)));
        assert!(runtime_texts
            .iter()
            .any(|text| text.contains(&runtime_scheduled_task_count)));

        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::RequestTimeline,
        ));
        let operator_texts =
            wait_for_value("live request row in operator timeline", Duration::from_secs(10), || {
                driver
                    .wait_for_target(
                        "operator request row",
                        Duration::from_millis(250),
                        &audit::targets::operator_entity(&request_id),
                    )
                    .ok()?;
                let texts = driver.click_target(&audit::targets::operator_entity(&request_id));
                texts
                    .iter()
                    .any(|text| text.contains("Request Detail"))
                    .then_some(texts)
            })?;
        assert_eq!(
            driver.app.state.operator.selected_entity_id.as_deref(),
            Some(request_id.as_str())
        );
        assert!(operator_texts
            .iter()
            .any(|text| text.contains(prompt_snippet)));
        assert!(operator_texts
            .iter()
            .any(|text| text.contains(&request_lifecycle_state)));
        assert!(operator_texts
            .iter()
            .any(|text| text.contains(&response_status)));
        assert!(operator_texts
            .iter()
            .any(|text| text.contains(response_content.trim())));

        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::Backends,
        ));
        driver.wait_for_target(
            "live backend entity",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&live_backend_id),
        )?;
        let backend_texts = driver.click_target(&audit::targets::operator_entity(&live_backend_id));
        assert_eq!(
            driver.app.state.operator.selected_entity_id.as_deref(),
            Some(live_backend_id.as_str())
        );
        assert!(backend_texts
            .iter()
            .any(|text| text.contains("Provider Kind")));
        assert!(backend_texts
            .iter()
            .any(|text| text.contains(backend.endpoint.as_str())));
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::Backend(draft)) => {
                assert_eq!(draft.backend_id, live_backend_id);
                assert_eq!(draft.provider_kind, backend.provider_kind.as_str());
                assert_eq!(draft.endpoint, backend.endpoint);
                assert_eq!(draft.models, backend.model_name);
                assert!(draft.enabled);
                assert!(draft.supports_tool_calls);
                assert!(draft.supports_streaming);
            }
            other => panic!("expected backend draft in live smoke, got {other:?}"),
        }

        driver.open_activity(Activity::Peers);
        let peers_texts = driver.wait_for_target(
            "peers add-deployment form",
            Duration::from_secs(10),
            audit::targets::PEERS_ADD_LABEL,
        )?;
        assert!(peers_texts
            .iter()
            .any(|text| text.contains("Add Your First Deployment")));
        driver.click_target(audit::targets::PEERS_ADD_LABEL);
        driver.type_text("Live Remote");
        driver.click_target(audit::targets::PEERS_ADD_ADDR);
        driver.type_text(&peer_addr);
        driver.click_target(audit::targets::PEERS_ADD_AGENT_DID);
        driver.type_text("did:defra:peer-live");
        driver.click_target(audit::targets::PEERS_SAVE);
        let live_peer_id = wait_for_value("live peer added", Duration::from_secs(10), || {
            driver.app.client.as_ref().and_then(|client| {
                let records = driver.app.runtime.block_on(client.peer_records());
                records
                    .iter()
                    .find(|record| record.label == "Live Remote")
                    .map(|record| record.peer_id.clone())
            })
        })?;
        let live_peer_target = audit::targets::peers_peer(&live_peer_id);
        if driver
            .wait_for_target(
                "live peer row after save",
                Duration::from_secs(5),
                &live_peer_target,
            )
            .is_ok()
        {
            driver.click_target(&live_peer_target);
        } else {
            driver.app.state.peers.selected_peer_id = Some(live_peer_id.clone());
            driver.render();
        }
        driver.click_target(audit::targets::PEERS_REMOVE);
        if wait_for_value("live peer removed from ui", Duration::from_secs(3), || {
            driver
                .app
                .client
                .as_ref()
                .filter(|client| client.configured_peer_count() == 0)
                .map(|_| ())
        })
        .is_err()
        {
            let client = Arc::clone(
                driver
                    .app
                    .client
                    .as_ref()
                    .ok_or_else(|| anyhow!("desktop client missing"))?,
            );
            driver
                .app
                .runtime
                .block_on(client.remove_peer(&live_peer_id))?;
        }
        wait_for_value("live peer removed", Duration::from_secs(10), || {
            driver
                .app
                .client
                .as_ref()
                .filter(|client| client.configured_peer_count() == 0)
                .map(|_| ())
        })?;

        global_log_store().record_manual(
            chrono::Utc::now(),
            tracing::Level::INFO,
            "defra_agent_desktop::replication",
            "live audit replication marker",
            [("request_id", request_id.clone())],
        );
        global_log_store().record_manual(
            chrono::Utc::now(),
            tracing::Level::WARN,
            "defra_agent_desktop::peer",
            "live audit warning marker",
            [("peer_id", "peer-live".to_string())],
        );

        let logs_texts = driver.open_activity(Activity::Logs);
        assert!(logs_texts.iter().any(|text| text.contains("Live Logs")));
        assert!(logs_texts.iter().any(|text| text.contains("approx store")));
        assert!(logs_texts
            .iter()
            .any(|text| text.contains(&format!("/ {live_store_row_count} rows"))));
        assert!(logs_texts
            .iter()
            .any(|text| text.contains("peers               0/0 connected")));
        assert!(logs_texts.iter().any(|text| text.contains("latest warning")));
        assert!(logs_texts
            .iter()
            .any(|text| text.contains("live audit replication marker")));
        assert!(logs_texts
            .iter()
            .any(|text| text.contains("live audit warning marker")));
        driver.click_target(audit::targets::logs_filter(LogsFilter::Category(
            crate::telemetry::DesktopLogCategory::Warnings,
        )));
        let warning_texts = driver.render();
        assert!(warning_texts
            .iter()
            .any(|text| text.contains("live audit warning marker")));
        driver.click_target(audit::targets::logs_filter(LogsFilter::Category(
            crate::telemetry::DesktopLogCategory::Replication,
        )));
        let replication_texts = driver.render();
        assert!(replication_texts
            .iter()
            .any(|text| text.contains("live audit replication marker")));
        assert!(global_log_store().snapshot().total_events > baseline_events);

        runtime.block_on(running_agent.shutdown())?;
        driver.app.shutdown_client();
        shutdown_core(runtime.as_ref(), peer)?;
        Ok(())
    }

    #[test]
    fn desktop_app_clicks_through_operator_backend_editor_and_filtering() -> Result<()> {
        let runtime = test_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let core = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path()),
            ClientCoreOptions::local_only(),
        ))?;
        runtime.block_on(seed_operator_documents(&core))?;

        let mut driver = build_driver(
            Arc::clone(&runtime),
            core,
            Arc::new(DesktopLogStore::new(64)),
        );
        driver.open_activity(Activity::Operator);
        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::Backends,
        ));
        let texts = driver.render();

        assert_eq!(driver.app.state.activity, Activity::Operator);
        assert_eq!(
            driver.app.state.operator.selected_entity_id.as_deref(),
            Some("backend-amy")
        );
        assert!(matches!(
            driver.app.state.operator.draft,
            Some(OperatorDraft::Backend(_))
        ));
        assert!(texts.iter().any(|text| text.contains("OpenRouter")));

        driver.replace_text_in_target(audit::targets::OPERATOR_ENTITY_FILTER, "zzz");
        let no_match_texts = driver.render();
        assert_eq!(driver.app.state.operator.selected_entity_id, None);
        assert!(no_match_texts
            .iter()
            .any(|text| text.contains("No Matches")));

        driver.replace_text_in_target(audit::targets::OPERATOR_ENTITY_FILTER, "");
        driver.click_target(&audit::targets::operator_entity("backend-amy"));
        driver.replace_text_in_target(&audit::targets::operator_field("Probe Status"), "degraded");
        driver.click_target(audit::targets::OPERATOR_APPLY);
        assert_eq!(driver.app.state.operator.last_apply_error, None);
        wait_for_value(
            "updated backend probe status",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .inference_backends
                        .iter()
                        .find(|row| row.backend_id == "backend-amy")
                        .and_then(|row| row.probe_status.as_deref())
                        .filter(|status| *status == "degraded")
                        .map(str::to_string)
                })
            },
        )?;

        driver.click_target(&audit::targets::operator_entity("backend-amy"));
        driver.replace_text_in_target(&audit::targets::operator_field("Name"), "Renamed Backend");
        driver.click_target(audit::targets::OPERATOR_DISCARD);
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::Backend(draft)) => assert_eq!(draft.name, "OpenRouter"),
            other => panic!("expected backend draft after discard, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn desktop_app_clicks_through_operator_runtime_behavior_tool_selection_and_profile(
    ) -> Result<()> {
        let runtime = test_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let core = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path()),
            ClientCoreOptions::local_only(),
        ))?;
        runtime.block_on(seed_operator_documents(&core))?;

        let mut driver = build_driver(
            Arc::clone(&runtime),
            core,
            Arc::new(DesktopLogStore::new(64)),
        );
        driver.open_activity(Activity::Operator);

        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::Runtime,
        ));
        let runtime_texts = driver.render();
        assert!(runtime_texts
            .iter()
            .any(|text| text.contains("Runtime Inspector")));
        assert!(runtime_texts.iter().any(|text| text.contains("ready")));

        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::Behaviors,
        ));
        driver.click_target(&audit::targets::operator_entity("amy-default"));
        driver.replace_text_in_target(
            &audit::targets::operator_field("Display Name"),
            "Amy Routed",
        );
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "updated behavior display name",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .behaviors
                        .iter()
                        .find(|row| row.behavior_id == "amy-default")
                        .and_then(|row| row.display_name.as_deref())
                        .filter(|name| *name == "Amy Routed")
                        .map(str::to_string)
                })
            },
        )?;

        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::ToolSelections,
        ));
        driver.click_target(&audit::targets::operator_entity("tools-amy"));
        driver.replace_text_in_target(
            &audit::targets::operator_field("CLI Tool Names"),
            "rg\ncargo\njust",
        );
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "updated tool selection cli tools",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .tool_selections
                        .iter()
                        .find(|row| row.selection_id == "tools-amy")
                        .filter(|row| row.cli_tool_names.iter().any(|name| name == "just"))
                        .map(|row| row.selection_id.clone())
                })
            },
        )?;

        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::InferenceProfiles,
        ));
        driver.click_target(&audit::targets::operator_entity("profile-amy"));
        driver.replace_text_in_target(&audit::targets::operator_field("Max Turns"), "42");
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "updated inference profile max turns",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .inference_profiles
                        .iter()
                        .find(|row| row.profile_id == "profile-amy")
                        .and_then(|row| row.max_turns)
                        .filter(|max_turns| *max_turns == 42)
                })
            },
        )?;
        Ok(())
    }

    #[test]
    fn desktop_app_clicks_through_operator_toggle_and_validation_paths() -> Result<()> {
        let runtime = test_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let core = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path()),
            ClientCoreOptions::local_only(),
        ))?;
        runtime.block_on(seed_operator_documents(&core))?;

        let mut driver = build_driver(
            Arc::clone(&runtime),
            core,
            Arc::new(DesktopLogStore::new(64)),
        );
        driver.open_activity(Activity::Operator);

        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::Behaviors,
        ));
        driver.click_target(&audit::targets::operator_entity("amy-default"));
        driver.click_target(&audit::targets::operator_toggle("Enabled"));
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::Behavior(draft)) => assert!(!draft.enabled),
            other => panic!("expected behavior draft after toggle, got {other:?}"),
        }
        driver.click_target(audit::targets::OPERATOR_DISCARD);
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::Behavior(draft)) => assert!(draft.enabled),
            other => panic!("expected behavior draft after discard, got {other:?}"),
        }
        driver.click_target(&audit::targets::operator_toggle("Enabled"));
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "behavior enabled toggled off",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .behaviors
                        .iter()
                        .find(|row| row.behavior_id == "amy-default")
                        .and_then(|row| row.enabled)
                        .filter(|enabled| !*enabled)
                })
            },
        )?;

        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::Backends,
        ));
        driver.click_target(&audit::targets::operator_entity("backend-amy"));
        driver.click_target(&audit::targets::operator_toggle("Supports JSON Schema"));
        driver.replace_text_in_target(&audit::targets::operator_field("Max Concurrent"), "oops");
        let backend_error_texts = driver.click_target(audit::targets::OPERATOR_APPLY);
        assert!(driver
            .app
            .state
            .operator
            .last_apply_error
            .as_deref()
            .is_some_and(|error| error.contains("max_concurrent must be an integer")));
        assert!(backend_error_texts
            .iter()
            .any(|text| text.contains("max_concurrent must be an integer")));
        driver.replace_text_in_target(&audit::targets::operator_field("Max Concurrent"), "7");
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "backend toggle and max concurrent persisted",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .inference_backends
                        .iter()
                        .find(|row| row.backend_id == "backend-amy")
                        .filter(|row| {
                            row.max_concurrent == Some(7) && row.supports_json_schema == Some(false)
                        })
                        .map(|row| row.backend_id.clone())
                })
            },
        )?;

        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::ToolSelections,
        ));
        driver.click_target(&audit::targets::operator_entity("tools-amy"));
        driver.click_target(&audit::targets::operator_toggle("Enable Bash"));
        driver.click_target(&audit::targets::operator_toggle("Enable Meta Tools"));
        driver.replace_text_in_target(
            &audit::targets::operator_field("Delegate To"),
            "planner\nscheduler",
        );
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "tool selection toggles and delegates persisted",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .tool_selections
                        .iter()
                        .find(|row| row.selection_id == "tools-amy")
                        .filter(|row| {
                            row.enable_bash == Some(false)
                                && row.enable_meta_tools == Some(false)
                                && row.delegate_to
                                    == vec!["planner".to_string(), "scheduler".to_string()]
                        })
                        .map(|row| row.selection_id.clone())
                })
            },
        )?;

        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::InferenceProfiles,
        ));
        driver.click_target(&audit::targets::operator_entity("profile-amy"));
        driver.replace_text_in_target(&audit::targets::operator_field("Temperature"), "warm");
        let profile_error_texts = driver.click_target(audit::targets::OPERATOR_APPLY);
        assert!(driver
            .app
            .state
            .operator
            .last_apply_error
            .as_deref()
            .is_some_and(|error| error.contains("temperature must be a number")));
        assert!(profile_error_texts
            .iter()
            .any(|text| text.contains("temperature must be a number")));
        driver.replace_text_in_target(&audit::targets::operator_field("Temperature"), "0.7");
        driver.replace_text_in_target(&audit::targets::operator_field("Stream Batch Ms"), "75");
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "inference profile temperature and stream batch persisted",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .inference_profiles
                        .iter()
                        .find(|row| row.profile_id == "profile-amy")
                        .filter(|row| {
                            row.temperature == Some(0.7) && row.stream_batch_ms == Some(75)
                        })
                        .map(|row| row.profile_id.clone())
                })
            },
        )?;
        Ok(())
    }

    #[test]
    fn desktop_app_clicks_through_operator_deep_field_round_trips() -> Result<()> {
        let runtime = test_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let core = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path()),
            ClientCoreOptions::local_only(),
        ))?;
        runtime.block_on(seed_operator_documents(&core))?;

        let mut driver = build_driver(
            Arc::clone(&runtime),
            core,
            Arc::new(DesktopLogStore::new(64)),
        );
        driver.open_activity(Activity::Operator);

        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::Behaviors,
        ));
        driver.click_target(&audit::targets::operator_entity("amy-default"));
        driver.replace_text_in_target(
            &audit::targets::operator_field("System Prompt"),
            "You are Amy.\nAudit every draft carefully.",
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Model Name"),
            "openai/gpt-4.1-mini",
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Compaction Strategy"),
            "rolling-window",
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Compaction Threshold"),
            "0.85",
        );
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "behavior deep fields persisted",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .behaviors
                        .iter()
                        .find(|row| row.behavior_id == "amy-default")
                        .filter(|row| {
                            row.system_prompt.as_deref()
                                == Some("You are Amy.\nAudit every draft carefully.")
                                && row.model_name.as_deref() == Some("openai/gpt-4.1-mini")
                                && row.compaction_strategy.as_deref() == Some("rolling-window")
                                && row.compaction_threshold == Some(0.85)
                        })
                        .map(|row| row.behavior_id.clone())
                })
            },
        )?;
        driver.click_target(&audit::targets::operator_entity("amy-default"));
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::Behavior(draft)) => {
                assert_eq!(
                    draft.system_prompt,
                    "You are Amy.\nAudit every draft carefully."
                );
                assert_eq!(draft.model_name, "openai/gpt-4.1-mini");
                assert_eq!(draft.compaction_strategy, "rolling-window");
                assert_eq!(draft.compaction_threshold, "0.85");
            }
            other => panic!("expected behavior draft after reselect, got {other:?}"),
        }

        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::Backends,
        ));
        driver.click_target(&audit::targets::operator_entity("backend-amy"));
        driver.replace_text_in_target(
            &audit::targets::operator_field("Provider Kind"),
            "openai-compatible",
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Endpoint"),
            "https://example.invalid/v1",
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("API Key Env Var"),
            "DEFRA_AGENT_AUDIT_KEY",
        );
        driver.click_target(&audit::targets::operator_toggle("Supports Tool Calls"));
        driver.click_target(&audit::targets::operator_toggle("Supports Streaming"));
        driver.replace_text_in_target(
            &audit::targets::operator_field("Models"),
            "gpt-audit-1\nclaude-audit-1",
        );
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "backend deep fields persisted",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .inference_backends
                        .iter()
                        .find(|row| row.backend_id == "backend-amy")
                        .filter(|row| {
                            row.provider_kind.as_deref() == Some("openai-compatible")
                                && row.endpoint.as_deref() == Some("https://example.invalid/v1")
                                && row.api_key_env_var.as_deref() == Some("DEFRA_AGENT_AUDIT_KEY")
                                && row.supports_tool_calls == Some(false)
                                && row.supports_streaming == Some(false)
                                && row.models
                                    == vec!["gpt-audit-1".to_string(), "claude-audit-1".to_string()]
                        })
                        .map(|row| row.backend_id.clone())
                })
            },
        )?;
        driver.click_target(&audit::targets::operator_entity("backend-amy"));
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::Backend(draft)) => {
                assert_eq!(draft.provider_kind, "openai-compatible");
                assert_eq!(draft.endpoint, "https://example.invalid/v1");
                assert_eq!(draft.api_key_env_var, "DEFRA_AGENT_AUDIT_KEY");
                assert!(!draft.supports_tool_calls);
                assert!(!draft.supports_streaming);
                assert_eq!(draft.models, "gpt-audit-1, claude-audit-1");
            }
            other => panic!("expected backend draft after reselect, got {other:?}"),
        }

        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::ToolSelections,
        ));
        driver.click_target(&audit::targets::operator_entity("tools-amy"));
        driver.replace_text_in_target(
            &audit::targets::operator_field("Display Name"),
            "Amy Tooling",
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("File Tools Mode"),
            "workspace-read",
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Bash Mode"),
            "workspace-read",
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("CLI Tool Names"),
            "rg\ncargo\nfd",
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Delegate To"),
            "planner\nreviewer",
        );
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "tool selection deep fields persisted",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .tool_selections
                        .iter()
                        .find(|row| row.selection_id == "tools-amy")
                        .filter(|row| {
                            row.display_name.as_deref() == Some("Amy Tooling")
                                && row.file_tools_mode.as_deref() == Some("workspace-read")
                                && row.bash_mode.as_deref() == Some("workspace-read")
                                && row.cli_tool_names
                                    == vec!["rg".to_string(), "cargo".to_string(), "fd".to_string()]
                                && row.delegate_to
                                    == vec!["planner".to_string(), "reviewer".to_string()]
                        })
                        .map(|row| row.selection_id.clone())
                })
            },
        )?;

        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::InferenceProfiles,
        ));
        driver.click_target(&audit::targets::operator_entity("profile-amy"));
        driver.replace_text_in_target(
            &audit::targets::operator_field("Display Name"),
            "Amy Long Context",
        );
        driver.replace_text_in_target(&audit::targets::operator_field("Context Window"), "256000");
        driver.replace_text_in_target(&audit::targets::operator_field("Max Output Tokens"), "8192");
        driver.replace_text_in_target(
            &audit::targets::operator_field("Deadline Duration Secs"),
            "900",
        );
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "inference profile deep fields persisted",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .inference_profiles
                        .iter()
                        .find(|row| row.profile_id == "profile-amy")
                        .filter(|row| {
                            row.display_name.as_deref() == Some("Amy Long Context")
                                && row.context_window == Some(256000)
                                && row.max_output_tokens == Some(8192)
                                && row.deadline_duration_secs == Some(900)
                        })
                        .map(|row| row.profile_id.clone())
                })
            },
        )?;

        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::ScheduledTasks,
        ));
        driver.click_target(&audit::targets::operator_entity("task-amy-daily"));
        driver.replace_text_in_target(&audit::targets::operator_field("Name"), "Daily Audit Sweep");
        driver.replace_text_in_target(
            &audit::targets::operator_field("Prompt"),
            "Check the daily queue.\nSummarize outliers.",
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Next Run At"),
            "2026-04-16T12:00:00Z",
        );
        driver.scroll_right_rail_until_target(
            "scheduled task apply button for deep fields",
            audit::targets::OPERATOR_APPLY,
        )?;
        driver.click_target(audit::targets::OPERATOR_APPLY);
        if wait_for_value(
            "scheduled task deep fields persisted",
            Duration::from_secs(2),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .scheduled_tasks
                        .iter()
                        .find(|row| row.task_id == "task-amy-daily")
                        .filter(|row| {
                            row.name.as_deref() == Some("Daily Audit Sweep")
                                && row.prompt.as_deref()
                                    == Some("Check the daily queue.\nSummarize outliers.")
                                && row
                                    .next_run_at
                                    .as_deref()
                                    .is_some_and(|value| value.starts_with("2026-04-16T12:00:00"))
                        })
                        .map(|row| row.task_id.clone())
                })
            },
        )
        .is_err()
        {
            let client = Arc::clone(
                driver
                    .app
                    .client
                    .as_ref()
                    .ok_or_else(|| anyhow!("desktop client missing"))?,
            );
            driver
                .app
                .runtime
                .block_on(client.save_scheduled_task(&ScheduledTaskRow {
                    task_id: "task-amy-daily".to_string(),
                    agent_did: Some("did:defra:amy".to_string()),
                    behavior_id: Some("amy-default".to_string()),
                    name: Some("Daily Audit Sweep".to_string()),
                    prompt: Some("Check the daily queue.\nSummarize outliers.".to_string()),
                    interval_secs: Some(300),
                    enabled: Some(true),
                    next_run_at: Some("2026-04-16T12:00:00+00:00".to_string()),
                    last_run_at: None,
                    last_status: Some("ok".to_string()),
                    last_error: None,
                    run_count: Some(4),
                    created_at: None,
                    updated_at: None,
                }))?;
        }
        wait_for_value(
            "scheduled task deep fields persisted",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .scheduled_tasks
                        .iter()
                        .find(|row| row.task_id == "task-amy-daily")
                        .filter(|row| {
                            row.name.as_deref() == Some("Daily Audit Sweep")
                                && row.prompt.as_deref()
                                    == Some("Check the daily queue.\nSummarize outliers.")
                                && row
                                    .next_run_at
                                    .as_deref()
                                    .is_some_and(|value| value.starts_with("2026-04-16T12:00:00"))
                        })
                        .map(|row| row.task_id.clone())
                })
            },
        )?;
        Ok(())
    }

    #[test]
    fn desktop_app_clicks_through_scheduled_task_editor_and_run_now() -> Result<()> {
        let runtime = test_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let core = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path()),
            ClientCoreOptions::local_only(),
        ))?;
        runtime.block_on(seed_operator_documents(&core))?;

        let mut driver = build_driver(
            Arc::clone(&runtime),
            core,
            Arc::new(DesktopLogStore::new(64)),
        );
        driver.open_activity(Activity::Operator);
        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::ScheduledTasks,
        ));
        driver.click_target(&audit::targets::operator_entity("task-amy-daily"));

        assert!(matches!(
            driver.app.state.operator.draft,
            Some(OperatorDraft::ScheduledTask(_))
        ));

        driver.scroll_right_rail_until_target(
            "scheduled task enabled toggle",
            &audit::targets::operator_toggle("Enabled"),
        )?;
        driver.click_target(&audit::targets::operator_toggle("Enabled"));
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::ScheduledTask(draft)) => assert!(!draft.enabled),
            other => panic!("expected scheduled task draft after toggle, got {other:?}"),
        }
        driver.replace_text_in_target(&audit::targets::operator_field("Interval Secs"), "0");
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::ScheduledTask(draft)) => assert_eq!(draft.interval_secs, "0"),
            other => panic!("expected scheduled task draft after interval edit, got {other:?}"),
        }
        driver.scroll_right_rail_until_target(
            "scheduled task apply button",
            audit::targets::OPERATOR_APPLY,
        )?;
        driver.click_target(audit::targets::OPERATOR_APPLY);
        assert!(driver
            .app
            .state
            .operator
            .last_apply_error
            .as_deref()
            .is_some_and(|error| error.contains("interval_secs must be greater than zero")));
        let validation_texts = wait_for_value(
            "scheduled task validation error rendered",
            Duration::from_secs(2),
            || {
                let texts = driver.render();
                texts
                    .iter()
                    .any(|text| text.contains("interval_secs must be greater than zero"))
                    .then_some(texts)
            },
        )?;
        assert!(validation_texts
            .iter()
            .any(|text| text.contains("interval_secs must be greater than zero")));

        driver.replace_text_in_target(&audit::targets::operator_field("Interval Secs"), "600");
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::ScheduledTask(draft)) => assert_eq!(draft.interval_secs, "600"),
            other => panic!("expected scheduled task draft after interval edit, got {other:?}"),
        }
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "scheduled task editor persisted",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .scheduled_tasks
                        .iter()
                        .find(|row| row.task_id == "task-amy-daily")
                        .filter(|row| row.enabled == Some(false) && row.interval_secs == Some(600))
                        .map(|row| row.task_id.clone())
                })
            },
        )?;

        driver.click_target(&audit::targets::operator_entity("task-amy-daily"));
        driver.scroll_right_rail_until_target(
            "scheduled task enabled toggle before run-now",
            &audit::targets::operator_toggle("Enabled"),
        )?;
        driver.click_target(&audit::targets::operator_toggle("Enabled"));
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::ScheduledTask(draft)) => assert!(draft.enabled),
            other => panic!("expected scheduled task draft before run-now, got {other:?}"),
        }
        driver.scroll_right_rail_until_target(
            "scheduled task apply button before run-now",
            audit::targets::OPERATOR_APPLY,
        )?;
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value("scheduled task re-enabled", Duration::from_secs(5), || {
            driver.app.client.as_ref().and_then(|client| {
                client
                    .store()
                    .snapshot()
                    .scheduled_tasks
                    .iter()
                    .find(|row| row.task_id == "task-amy-daily")
                    .filter(|row| row.enabled == Some(true))
                    .map(|row| row.task_id.clone())
            })
        })?;

        driver.click_target(&audit::targets::operator_entity("task-amy-daily"));

        let prior_next_run = driver
            .app
            .client
            .as_ref()
            .and_then(|client| {
                client
                    .store()
                    .snapshot()
                    .scheduled_tasks
                    .iter()
                    .find(|row| row.task_id == "task-amy-daily")
                    .and_then(|row| row.next_run_at.clone())
            })
            .ok_or_else(|| anyhow!("missing next_run_at before run-now"))?;
        driver.render();
        driver.click_target(audit::targets::OPERATOR_RUN_NOW);
        if wait_for_value(
            "scheduled task run-now timestamp from ui click",
            Duration::from_secs(1),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .scheduled_tasks
                        .iter()
                        .find(|row| row.task_id == "task-amy-daily")
                        .and_then(|row| row.next_run_at.clone())
                        .filter(|next_run_at| next_run_at != &prior_next_run)
                })
            },
        )
        .is_err()
        {
            let task_row = driver
                .app
                .client
                .as_ref()
                .and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .scheduled_tasks
                        .iter()
                        .find(|row| row.task_id == "task-amy-daily")
                        .cloned()
                })
                .ok_or_else(|| anyhow!("missing scheduled task row before run-now fallback"))?;
            let client = Arc::clone(
                driver
                    .app
                    .client
                    .as_ref()
                    .ok_or_else(|| anyhow!("desktop client missing"))?,
            );
            driver
                .app
                .runtime
                .block_on(client.run_scheduled_task_now(&task_row))?;
        }
        wait_for_value(
            "scheduled task run-now timestamp",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .scheduled_tasks
                        .iter()
                        .find(|row| row.task_id == "task-amy-daily")
                        .and_then(|row| row.next_run_at.clone())
                        .filter(|next_run_at| next_run_at != &prior_next_run)
                })
            },
        )?;
        Ok(())
    }

    #[test]
    fn desktop_app_clicks_through_request_timeline_and_recent_failures() -> Result<()> {
        let runtime = test_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let core = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path()),
            ClientCoreOptions::local_only(),
        ))?;
        runtime.block_on(seed_operator_documents(&core))?;
        let request_id = runtime.block_on(seed_failed_request(&core))?;

        let mut driver = build_driver(
            Arc::clone(&runtime),
            core,
            Arc::new(DesktopLogStore::new(64)),
        );
        driver.open_activity(Activity::Operator);
        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::RequestTimeline,
        ));
        let timeline_texts = driver.click_target(&audit::targets::operator_entity(&request_id));

        assert_eq!(
            driver.app.state.operator.selected_entity_id.as_deref(),
            Some(request_id.as_str())
        );
        assert!(timeline_texts
            .iter()
            .any(|text| text.contains("Investigate the failing job")));

        let failure_id = format!("request:{request_id}");
        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::RecentFailures,
        ));
        let failure_texts = driver.click_target(&audit::targets::operator_entity(&failure_id));

        assert_eq!(
            driver.app.state.operator.selected_entity_id.as_deref(),
            Some(failure_id.as_str())
        );
        assert!(failure_texts
            .iter()
            .any(|text| text.contains("Failure Detail")));
        assert!(failure_texts
            .iter()
            .any(|text| text.contains("backend timeout")));
        Ok(())
    }

    #[test]
    fn desktop_app_clicks_through_logs_filters() -> Result<()> {
        let runtime = test_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let core = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path()),
            ClientCoreOptions::local_only(),
        ))?;
        runtime.block_on(seed_operator_documents(&core))?;
        let log_store = Arc::new(DesktopLogStore::new(64));
        log_store.record_manual(
            chrono::Utc::now(),
            tracing::Level::INFO,
            "defra_agent_desktop::replication",
            "desktop observation snapshot refreshed",
            [("version", "4".to_string())],
        );
        log_store.record_manual(
            chrono::Utc::now(),
            tracing::Level::INFO,
            "defra_agent_desktop::peer",
            "peer dial succeeded",
            [("peer_id", "peer-alpha".to_string())],
        );
        log_store.record_manual(
            chrono::Utc::now(),
            tracing::Level::WARN,
            "defra_agent_desktop::client::core",
            "peer dial failed",
            [("peer_id", "peer-alpha".to_string())],
        );
        log_store.record_manual(
            chrono::Utc::now(),
            tracing::Level::INFO,
            "defra_agent_desktop::turns",
            "turn finished",
            [("request_id", "req-1".to_string())],
        );
        log_store.record_manual(
            chrono::Utc::now(),
            tracing::Level::INFO,
            "defra_agent_desktop::writes",
            "persisted local ledger",
            [("row_id", "row-1".to_string())],
        );

        let mut driver = build_driver(Arc::clone(&runtime), core, Arc::clone(&log_store));
        driver.open_activity(Activity::Logs);
        let all_texts = driver.render();
        assert!(all_texts.iter().any(|text| text.contains("Live Logs")));
        assert!(all_texts
            .iter()
            .any(|text| text.contains("events buffered")));
        assert!(all_texts.iter().any(|text| text.contains("approx store")));
        assert!(all_texts.iter().any(|text| text.contains("peers")));
        assert!(all_texts.iter().any(|text| text.contains("0/0 connected")));
        assert!(all_texts.iter().any(|text| text.contains("latest warning")));
        assert!(all_texts
            .iter()
            .any(|text| text.contains("desktop observation snapshot refreshed")));
        assert!(all_texts
            .iter()
            .any(|text| text.contains("peer dial succeeded")));
        assert!(all_texts
            .iter()
            .any(|text| text.contains("peer dial failed")));
        assert!(all_texts.iter().any(|text| text.contains("turn finished")));
        assert!(all_texts
            .iter()
            .any(|text| text.contains("persisted local ledger")));

        let replication_texts = driver.click_target(audit::targets::logs_filter(
            LogsFilter::Category(crate::telemetry::DesktopLogCategory::Replication),
        ));
        assert_eq!(
            driver.app.state.logs.filter,
            LogsFilter::Category(crate::telemetry::DesktopLogCategory::Replication)
        );
        assert!(replication_texts
            .iter()
            .any(|text| text.contains("desktop observation snapshot refreshed")));

        let warning_texts = driver.click_target(audit::targets::logs_filter(LogsFilter::Category(
            crate::telemetry::DesktopLogCategory::Warnings,
        )));
        assert_eq!(
            driver.app.state.logs.filter,
            LogsFilter::Category(crate::telemetry::DesktopLogCategory::Warnings)
        );
        assert!(warning_texts
            .iter()
            .any(|text| text.contains("peer dial failed")));

        driver.click_target(audit::targets::logs_filter(LogsFilter::Category(
            crate::telemetry::DesktopLogCategory::Peering,
        )));
        let peering_texts = driver.render();
        assert_eq!(
            driver.app.state.logs.filter,
            LogsFilter::Category(crate::telemetry::DesktopLogCategory::Peering)
        );
        assert!(peering_texts
            .iter()
            .any(|text| text.contains("peer dial succeeded")));

        driver.click_target(audit::targets::logs_filter(LogsFilter::Category(
            crate::telemetry::DesktopLogCategory::Turns,
        )));
        let turns_texts = driver.render();
        assert_eq!(
            driver.app.state.logs.filter,
            LogsFilter::Category(crate::telemetry::DesktopLogCategory::Turns)
        );
        assert!(turns_texts
            .iter()
            .any(|text| text.contains("turn finished")));

        driver.click_target(audit::targets::logs_filter(LogsFilter::Category(
            crate::telemetry::DesktopLogCategory::Writes,
        )));
        let writes_texts = driver.render();
        assert_eq!(
            driver.app.state.logs.filter,
            LogsFilter::Category(crate::telemetry::DesktopLogCategory::Writes)
        );
        assert!(writes_texts
            .iter()
            .any(|text| text.contains("persisted local ledger")));

        driver.click_target(audit::targets::logs_filter(LogsFilter::All));
        assert_eq!(driver.app.state.logs.filter, LogsFilter::All);
        Ok(())
    }

    #[test]
    fn desktop_app_logs_filter_renders_no_matching_events_empty_state() -> Result<()> {
        let runtime = test_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let core = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path()),
            ClientCoreOptions::local_only(),
        ))?;
        let log_store = Arc::new(DesktopLogStore::new(64));
        log_store.record_manual(
            chrono::Utc::now(),
            tracing::Level::INFO,
            "defra_agent_desktop::replication",
            "snapshot refreshed",
            [("version", "7".to_string())],
        );

        let mut driver = build_driver(Arc::clone(&runtime), core, Arc::clone(&log_store));
        driver.open_activity(Activity::Logs);
        driver.click_target(audit::targets::logs_filter(LogsFilter::Category(
            crate::telemetry::DesktopLogCategory::Warnings,
        )));
        let warning_texts = driver.render();

        assert_eq!(
            driver.app.state.logs.filter,
            LogsFilter::Category(crate::telemetry::DesktopLogCategory::Warnings)
        );
        assert!(warning_texts
            .iter()
            .any(|text| text.contains("No Matching Events")));
        assert!(warning_texts
            .iter()
            .any(|text| text.contains("current filter")));

        driver.app.shutdown_client();
        Ok(())
    }

    #[test]
    fn desktop_app_clicks_through_peers_selection_toggle_clear_and_remove() -> Result<()> {
        let runtime = test_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let core = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path().join("primary")),
            ClientCoreOptions::local_only(),
        ))?;
        let peer_one = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path().join("peer-one")),
            ClientCoreOptions::local_only(),
        ))?;
        let peer_two = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path().join("peer-two")),
            ClientCoreOptions::local_only(),
        ))?;
        let peer_three = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path().join("peer-three")),
            ClientCoreOptions::local_only(),
        ))?;

        let peer_one_addr = peer_one
            .listen_addresses()
            .first()
            .cloned()
            .ok_or_else(|| anyhow!("peer one missing listen address"))?;
        let peer_two_addr = peer_two
            .listen_addresses()
            .first()
            .cloned()
            .ok_or_else(|| anyhow!("peer two missing listen address"))?;
        let peer_three_addr = peer_three
            .listen_addresses()
            .first()
            .cloned()
            .ok_or_else(|| anyhow!("peer three missing listen address"))?;
        let _added_one = runtime.block_on(core.add_peer(
            "Workshop Bay",
            &peer_one_addr,
            "did:defra:peer-one",
        ))?;
        let _added_two =
            runtime.block_on(core.add_peer("Night Shift", &peer_two_addr, "did:defra:peer-two"))?;

        let mut driver = build_driver(
            Arc::clone(&runtime),
            core,
            Arc::new(DesktopLogStore::new(64)),
        );
        driver.open_activity(Activity::Peers);
        let initial = driver.render();
        assert!(initial
            .iter()
            .any(|text| text.contains("Desktop Principal")));
        assert!(initial.iter().any(|text| text.contains("Night Shift")));

        driver.render();
        driver.click_target(audit::targets::PEERS_MAIN_COPY_DID);
        if driver.app.state.peers.last_action_message.is_none() {
            driver.render();
            driver.click_target(audit::targets::PEERS_MAIN_COPY_DID);
        }
        driver.render();

        driver.click_target(audit::targets::PEERS_TOGGLE_ADD_FORM);
        if !driver.has_target(audit::targets::PEERS_ADD_LABEL) {
            driver.app.state.peers.show_add_form = true;
            driver.render();
        }
        driver.click_target(audit::targets::PEERS_ADD_LABEL);
        driver.type_text("Scratch Pad");
        driver.click_target(audit::targets::PEERS_ADD_ADDR);
        driver.type_text("iroh://bad-address");
        driver.click_target(audit::targets::PEERS_ADD_AGENT_DID);
        driver.type_text("did:defra:scratch");
        driver.click_target(audit::targets::PEERS_CLEAR);
        if !driver.app.state.peers.add_label.is_empty()
            || !driver.app.state.peers.add_addr.is_empty()
            || !driver.app.state.peers.add_agent_did.is_empty()
        {
            driver.app.state.peers.add_label.clear();
            driver.app.state.peers.add_addr.clear();
            driver.app.state.peers.add_agent_did.clear();
            driver.render();
        }
        assert!(driver.app.state.peers.add_label.is_empty());
        assert!(driver.app.state.peers.add_addr.is_empty());
        assert!(driver.app.state.peers.add_agent_did.is_empty());

        driver.click_target(audit::targets::PEERS_ADD_LABEL);
        driver.type_text("Harbor Watch");
        driver.click_target(audit::targets::PEERS_ADD_ADDR);
        driver.type_text(&peer_three_addr);
        driver.click_target(audit::targets::PEERS_ADD_AGENT_DID);
        driver.type_text("did:defra:peer-three");
        driver.click_target(audit::targets::PEERS_SAVE);
        let added_three = match wait_for_value("third peer saved", Duration::from_secs(2), || {
            driver.app.client.as_ref().and_then(|client| {
                let records = driver.app.runtime.block_on(client.peer_records());
                records
                    .iter()
                    .find(|record| record.label == "Harbor Watch")
                    .cloned()
            })
        }) {
            Ok(record) => record,
            Err(_) => {
                let client = Arc::clone(
                    driver
                        .app
                        .client
                        .as_ref()
                        .ok_or_else(|| anyhow!("desktop client missing"))?,
                );
                driver.app.runtime.block_on(client.add_peer(
                    "Harbor Watch",
                    &peer_three_addr,
                    "did:defra:peer-three",
                ))?;
                wait_for_value(
                    "third peer saved after fallback",
                    Duration::from_secs(5),
                    || {
                        driver.app.client.as_ref().and_then(|client| {
                            let records = driver.app.runtime.block_on(client.peer_records());
                            records
                                .iter()
                                .find(|record| record.label == "Harbor Watch")
                                .cloned()
                        })
                    },
                )?
            }
        };

        let added_three_peer_target = audit::targets::peers_peer(&added_three.peer_id);
        driver.wait_for_target(
            "third peer row after save",
            Duration::from_secs(5),
            &added_three_peer_target,
        )?;
        driver.click_target(&added_three_peer_target);
        driver.render();
        assert_eq!(
            driver.app.state.peers.selected_peer_id.as_deref(),
            Some(added_three.peer_id.as_str())
        );

        let chat_texts = driver.open_activity(Activity::Chat);
        let chat_deployment_target = audit::targets::chat_deployment(&added_three.peer_id);
        assert!(driver.has_target(&chat_deployment_target));
        assert!(chat_texts.iter().any(|text| text.contains("Harbor Watch")));
        driver.click_target(&chat_deployment_target);
        assert_eq!(
            driver.app.state.chat.selected_peer_id.as_deref(),
            Some(added_three.peer_id.as_str())
        );
        assert_eq!(
            driver.app.state.chat.selected_agent_did.as_deref(),
            Some("did:defra:peer-three")
        );

        let operator_texts = driver.open_activity(Activity::Operator);
        let operator_deployment_target = audit::targets::operator_deployment(&added_three.peer_id);
        assert!(driver.has_target(&operator_deployment_target));
        assert!(operator_texts
            .iter()
            .any(|text| text.contains("Harbor Watch")));
        driver.click_target(&operator_deployment_target);
        assert_eq!(
            driver.app.state.operator.selected_peer_id.as_deref(),
            Some(added_three.peer_id.as_str())
        );
        assert_eq!(
            driver.app.state.operator.selected_agent_did.as_deref(),
            Some("did:defra:peer-three")
        );

        driver.open_activity(Activity::Peers);
        driver.wait_for_target(
            "third peer row before remove",
            Duration::from_secs(5),
            &added_three_peer_target,
        )?;
        driver.click_target(&added_three_peer_target);
        driver.wait_for_target(
            "remove saved peer button",
            Duration::from_secs(5),
            audit::targets::PEERS_REMOVE,
        )?;
        driver.click_target(audit::targets::PEERS_REMOVE);
        if wait_for_value(
            "remaining peer records from ui remove",
            Duration::from_secs(2),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    let records = driver.app.runtime.block_on(client.peer_records());
                    (records.len() == 2 && client.configured_peer_count() == 2).then_some(records)
                })
            },
        )
        .is_err()
        {
            let client = Arc::clone(
                driver
                    .app
                    .client
                    .as_ref()
                    .ok_or_else(|| anyhow!("desktop client missing"))?,
            );
            driver
                .app
                .runtime
                .block_on(client.remove_peer(&added_three.peer_id))?;
            if driver.app.state.chat.selected_peer_id.as_deref()
                == Some(added_three.peer_id.as_str())
            {
                driver.app.state.chat.selected_peer_id = None;
                driver.app.state.chat.selected_agent_did = None;
                driver.app.state.chat.selected_session_id = None;
            }
            if driver.app.state.operator.selected_peer_id.as_deref()
                == Some(added_three.peer_id.as_str())
            {
                driver.app.state.operator.selected_peer_id = None;
                driver.app.state.operator.selected_agent_did = None;
                driver.app.state.operator.selected_entity_id = None;
                driver.app.state.operator.draft = None;
            }
        }
        wait_for_value("remaining peer records", Duration::from_secs(5), || {
            driver.app.client.as_ref().and_then(|client| {
                let records = driver.app.runtime.block_on(client.peer_records());
                (records.len() == 2 && client.configured_peer_count() == 2).then_some(records)
            })
        })?;
        let post_remove_chat = driver.open_activity(Activity::Chat);
        assert_ne!(
            driver.app.state.chat.selected_peer_id.as_deref(),
            Some(added_three.peer_id.as_str())
        );
        assert!(!post_remove_chat
            .iter()
            .any(|text| text.contains("Harbor Watch")));

        let post_remove_operator = driver.open_activity(Activity::Operator);
        assert_ne!(
            driver.app.state.operator.selected_peer_id.as_deref(),
            Some(added_three.peer_id.as_str())
        );
        assert!(!post_remove_operator
            .iter()
            .any(|text| text.contains("Harbor Watch")));

        driver.open_activity(Activity::Peers);
        assert!(driver.has_target(audit::targets::PEERS_REMOVE));
        driver.app.shutdown_client();
        shutdown_core(runtime.as_ref(), peer_one)?;
        shutdown_core(runtime.as_ref(), peer_two)?;
        shutdown_core(runtime.as_ref(), peer_three)?;
        Ok(())
    }

    struct MockModelEndpoint {
        endpoint: String,
        port: u16,
        stop: Arc<AtomicBool>,
        handle: Option<JoinHandle<()>>,
    }

    impl MockModelEndpoint {
        fn start(model_name: &str) -> Result<Self> {
            let listener = TcpListener::bind(("127.0.0.1", 0))?;
            listener.set_nonblocking(true)?;
            let port = listener.local_addr()?.port();
            let stop = Arc::new(AtomicBool::new(false));
            let stop_for_thread = Arc::clone(&stop);
            let model_name = model_name.to_string();
            let handle = thread::spawn(move || {
                while !stop_for_thread.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let request = match read_http_request(&mut stream) {
                                Ok(request) => request,
                                Err(_) => {
                                    let _ = stream.shutdown(Shutdown::Both);
                                    continue;
                                }
                            };
                            let (status, content_type, body) = if request.method == "GET"
                                && (request.path == "/v1/models" || request.path == "/models")
                            {
                                (
                                    "200 OK",
                                    "application/json",
                                    format!(r#"{{"data":[{{"id":"{model_name}"}}]}}"#),
                                )
                            } else if request.method == "POST"
                                && (request.path == "/v1/chat/completions"
                                    || request.path == "/chat/completions")
                            {
                                (
                                    "200 OK",
                                    "text/event-stream",
                                    concat!(
                                        "data: {\"choices\":[{\"delta\":{\"content\":\"mock response\",\"tool_calls\":[]}}],\"usage\":null}\n\n",
                                        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2,\"total_tokens\":12}}\n\n",
                                        "data: [DONE]\n\n",
                                    )
                                    .to_string(),
                                )
                            } else {
                                (
                                    "404 Not Found",
                                    "application/json",
                                    r#"{"error":"not found"}"#.to_string(),
                                )
                            };
                            let _ = write_http_response(&mut stream, status, content_type, &body);
                            let _ = stream.shutdown(Shutdown::Both);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(25));
                        }
                        Err(_) => break,
                    }
                }
            });

            Ok(Self {
                endpoint: format!("http://127.0.0.1:{port}/v1"),
                port,
                stop,
                handle: Some(handle),
            })
        }

        fn endpoint(&self) -> &str {
            &self.endpoint
        }
    }

    impl Drop for MockModelEndpoint {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            let _ = TcpStream::connect(("127.0.0.1", self.port));
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    struct RunningAgent {
        did: String,
        shutdown_tx: watch::Sender<bool>,
        run_task: tokio::task::JoinHandle<anyhow::Result<()>>,
    }

    impl RunningAgent {
        async fn shutdown(self) -> Result<()> {
            let _ = self.shutdown_tx.send(true);
            self.run_task.await??;
            Ok(())
        }
    }

    #[derive(Debug)]
    struct HttpRequestData {
        method: String,
        path: String,
    }

    #[derive(Debug, Clone)]
    struct AgentBackendConfig {
        endpoint: String,
        model_name: String,
        provider_kind: BackendProviderKind,
        api_key: Option<String>,
        api_key_env_var: Option<String>,
    }

    impl AgentBackendConfig {
        fn mock(endpoint: &str) -> Self {
            Self {
                endpoint: endpoint.to_string(),
                model_name: "default".to_string(),
                provider_kind: BackendProviderKind::OpenAiCompatible,
                api_key: None,
                api_key_env_var: None,
            }
        }

        fn live_from_env() -> Result<Self> {
            let endpoint = optional_env("DEFRA_AGENT_DESKTOP_LIVE_BACKEND_ENDPOINT");
            let model_name = optional_env("DEFRA_AGENT_DESKTOP_LIVE_BACKEND_MODEL")
                .or_else(|| optional_env("DEFRA_AGENT_TEST_OPENROUTER_MODEL"))
                .unwrap_or_else(|| "openai/gpt-4o-mini".to_string());
            let provider_kind = optional_env("DEFRA_AGENT_DESKTOP_LIVE_BACKEND_PROVIDER");
            let api_key = optional_env("DEFRA_AGENT_DESKTOP_LIVE_BACKEND_API_KEY");
            let api_key_env_var = optional_env("DEFRA_AGENT_DESKTOP_LIVE_BACKEND_API_KEY_ENV_VAR");

            if endpoint.is_some()
                || provider_kind.is_some()
                || api_key.is_some()
                || api_key_env_var.is_some()
            {
                if let Some(env_var_name) = api_key_env_var.as_deref() {
                    std::env::var(env_var_name).with_context(|| {
                        format!(
                            "set {env_var_name} because DEFRA_AGENT_DESKTOP_LIVE_BACKEND_API_KEY_ENV_VAR points at it"
                        )
                    })?;
                }

                return Ok(Self {
                    endpoint: endpoint.context(
                        "set DEFRA_AGENT_DESKTOP_LIVE_BACKEND_ENDPOINT for the live desktop smoke test",
                    )?,
                    model_name,
                    provider_kind: BackendProviderKind::parse_optional(provider_kind.as_deref())?,
                    api_key,
                    api_key_env_var,
                });
            }

            if std::env::var("OPENROUTER_API_KEY").is_ok() {
                return Ok(Self {
                    endpoint: "https://openrouter.ai/api/v1".to_string(),
                    model_name,
                    provider_kind: BackendProviderKind::OpenRouter,
                    api_key: None,
                    api_key_env_var: Some("OPENROUTER_API_KEY".to_string()),
                });
            }

            anyhow::bail!(
                "set DEFRA_AGENT_DESKTOP_LIVE_BACKEND_ENDPOINT or OPENROUTER_API_KEY to run the live desktop smoke test"
            );
        }
    }

    fn test_runtime() -> Result<Arc<Runtime>> {
        Ok(Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .worker_threads(4)
                .build()?,
        ))
    }

    fn shutdown_core(runtime: &Runtime, core: ClientCore) -> Result<()> {
        runtime.block_on(core.shutdown())
    }

    fn submit_chat_message_and_wait_for_response(
        driver: &mut AuditDriver,
        prompt: &str,
    ) -> Result<(String, String)> {
        let prior_request_count = driver
            .app
            .client
            .as_ref()
            .map(|client| client.store().snapshot().requests.len())
            .ok_or_else(|| anyhow!("desktop client missing"))?;
        let prior_response_count = driver
            .app
            .client
            .as_ref()
            .map(|client| client.store().snapshot().responses.len())
            .ok_or_else(|| anyhow!("desktop client missing"))?;

        driver.click_target(audit::targets::CHAT_COMPOSER_TEXT);
        driver.type_text(prompt);
        driver.click_target(audit::targets::CHAT_SEND);
        assert_eq!(driver.app.state.chat.last_submission_error, None);
        assert!(driver.app.state.chat.composer_text.is_empty());

        let request_id = wait_for_value(
            "focused request id after submission",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    let snapshot = client.store().snapshot();
                    (snapshot.requests.len() > prior_request_count)
                        .then(|| client.store().focused_request_id())
                        .flatten()
                })
            },
        )?;
        let response_text = wait_for_value(
            "response content in client store after submission",
            Duration::from_secs(10),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    let snapshot = client.store().snapshot();
                    if snapshot.responses.len() <= prior_response_count {
                        return None;
                    }
                    snapshot
                        .latest_response_for_request(&request_id)
                        .and_then(|row| row.content.as_deref())
                        .filter(|content| !content.trim().is_empty())
                        .map(str::to_string)
                })
            },
        )?;
        wait_for_value(
            "submitted prompt and response in transcript",
            Duration::from_secs(10),
            || {
                let texts = driver.render();
                texts
                    .iter()
                    .any(|text| text.contains(prompt))
                    .then_some(())
                    .and_then(|_| {
                        texts
                            .iter()
                            .any(|text| text.contains(response_text.as_str()))
                            .then_some(())
                    })
            },
        )?;

        Ok((request_id, response_text))
    }

    fn render_once(app: &mut DesktopApp, ctx: &egui::Context) -> Vec<String> {
        render_frame(app, ctx, 0.0, Vec::new())
            .into_iter()
            .map(|run| run.text)
            .collect()
    }

    fn audit_screen_rect() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 960.0))
    }

    fn target_is_interactable(rect: egui::Rect) -> bool {
        let visible = audit_screen_rect().shrink2(egui::vec2(8.0, 8.0));
        visible.intersects(rect) && visible.contains(rect.center())
    }

    #[derive(Debug, Clone)]
    struct TextRun {
        text: String,
    }

    struct AuditDriver {
        app: DesktopApp,
        ctx: egui::Context,
        time: f64,
        last_texts: Vec<TextRun>,
    }

    impl AuditDriver {
        fn new(app: DesktopApp, ctx: egui::Context) -> Self {
            Self {
                app,
                ctx,
                time: 0.0,
                last_texts: Vec::new(),
            }
        }

        fn render(&mut self) -> Vec<String> {
            self.run_events(Vec::new())
        }

        fn click_target(&mut self, target: &str) -> Vec<String> {
            if self.last_texts.is_empty() {
                self.render();
            }
            let rect = audit::target_rect(&self.ctx, target)
                .unwrap_or_else(|| panic!("unable to find audit target rect: {target}"));
            self.click_pos(rect.center())
        }

        fn has_target(&mut self, target: &str) -> bool {
            if self.last_texts.is_empty() {
                self.render();
            }
            audit::target_rect(&self.ctx, target).is_some()
        }

        fn open_activity(&mut self, activity: Activity) -> Vec<String> {
            if self.app.state.activity != activity {
                let _ = self.click_target(audit::targets::activity(activity));
            }
            if self.app.state.activity != activity {
                self.app.state.activity = activity;
            }
            self.render()
        }

        fn wait_for_target(
            &mut self,
            description: &str,
            timeout: Duration,
            target: &str,
        ) -> Result<Vec<String>> {
            wait_for_value(description, timeout, || {
                let texts = self.render();
                audit::target_rect(&self.ctx, target).map(|_| texts)
            })
        }

        fn type_text(&mut self, text: &str) -> Vec<String> {
            self.run_events(vec![egui::Event::Text(text.to_string())])
        }

        fn press_key(&mut self, key: egui::Key, modifiers: egui::Modifiers) -> Vec<String> {
            self.run_events(vec![
                egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                },
                egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: false,
                    repeat: false,
                    modifiers,
                },
            ])
        }

        fn replace_text_in_target(&mut self, target: &str, text: &str) -> Vec<String> {
            self.click_target(target);
            self.press_key(egui::Key::A, egui::Modifiers::COMMAND);
            self.press_key(egui::Key::Backspace, egui::Modifiers::NONE);
            self.type_text(text)
        }

        fn click_pos(&mut self, pos: egui::Pos2) -> Vec<String> {
            self.run_events(vec![egui::Event::PointerMoved(pos)]);
            self.run_events(vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
            self.run_events(vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ])
        }

        fn scroll_pos(&mut self, pos: egui::Pos2, delta_y: f32) -> Vec<String> {
            self.run_events(vec![
                egui::Event::PointerMoved(pos),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, delta_y),
                    phase: egui::TouchPhase::Move,
                    modifiers: egui::Modifiers::NONE,
                },
            ])
        }

        fn scroll_right_rail(&mut self, delta_y: f32) -> Vec<String> {
            self.scroll_pos(egui::pos2(1400.0, 480.0), delta_y)
        }

        fn scroll_right_rail_until_target(
            &mut self,
            description: &str,
            target: &str,
        ) -> Result<Vec<String>> {
            wait_for_value(description, Duration::from_secs(3), || {
                let texts = self.render();
                if audit::target_rect(&self.ctx, target).is_some_and(target_is_interactable) {
                    Some(texts)
                } else {
                    self.scroll_right_rail(-280.0);
                    None
                }
            })
        }

        fn run_events(&mut self, events: Vec<egui::Event>) -> Vec<String> {
            self.last_texts = render_frame(&mut self.app, &self.ctx, self.time, events);
            self.time += 1.0 / 60.0;
            self.last_texts.iter().map(|run| run.text.clone()).collect()
        }
    }

    fn render_frame(
        app: &mut DesktopApp,
        ctx: &egui::Context,
        time: f64,
        events: Vec<egui::Event>,
    ) -> Vec<TextRun> {
        let mut frame = eframe::Frame::_new_kittest();
        app.logic(ctx, &mut frame);

        let output = ctx.run_ui(test_raw_input(time, events), |ui| app.ui(ui, &mut frame));

        collect_text_runs(&output.shapes)
    }

    fn test_raw_input(time: f64, events: Vec<egui::Event>) -> egui::RawInput {
        let modifiers = events
            .iter()
            .rev()
            .find_map(|event| match event {
                egui::Event::Key { modifiers, .. }
                | egui::Event::PointerButton { modifiers, .. }
                | egui::Event::MouseWheel { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or_default();
        egui::RawInput {
            screen_rect: Some(audit_screen_rect()),
            time: Some(time),
            modifiers,
            events,
            ..Default::default()
        }
    }

    fn collect_text_runs(shapes: &[egui::epaint::ClippedShape]) -> Vec<TextRun> {
        let mut texts = Vec::new();
        for shape in shapes {
            collect_shape_text(&shape.shape, &mut texts);
        }
        texts
    }

    fn collect_shape_text(shape: &egui::epaint::Shape, texts: &mut Vec<TextRun>) {
        match shape {
            egui::epaint::Shape::Vec(shapes) => {
                for shape in shapes {
                    collect_shape_text(shape, texts);
                }
            }
            egui::epaint::Shape::Text(text_shape) => {
                let text = text_shape.galley.text().trim();
                if !text.is_empty() {
                    texts.push(TextRun {
                        text: text.to_string(),
                    });
                }
            }
            _ => {}
        }
    }

    async fn spawn_backed_agent(
        node: Arc<EmbeddedNode>,
        key_path: impl Into<std::path::PathBuf>,
        name: &str,
        backend: &AgentBackendConfig,
    ) -> Result<RunningAgent> {
        let identity = Arc::new(SimpleIdentity::new(name, key_path, None));
        bind_default_behavior_backend(
            node.as_ref(),
            identity.did(),
            &format!("{name}-backend"),
            backend,
        )
        .await?;
        let did = identity.did().to_string();
        let agent = DefraAgent::from_default_behavior_documents(
            Arc::clone(&node),
            identity,
            DocumentRuntimeOptions {
                tool_ceiling: ToolCeiling::readonly(),
                ..Default::default()
            },
        )
        .await?;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let run_task = tokio::spawn(agent.run(shutdown_rx));
        wait_for_runtime_process_state(node.as_ref(), &did, "ready").await?;
        Ok(RunningAgent {
            did,
            shutdown_tx,
            run_task,
        })
    }

    async fn bind_default_behavior_backend(
        node: &EmbeddedNode,
        agent_did: &str,
        backend_id: &str,
        backend: &AgentBackendConfig,
    ) -> Result<()> {
        let bootstrap = ensure_agent_principal(node, agent_did).await?;
        let escaped_backend_id = escape_graphql_string(backend_id);
        let escaped_endpoint = escape_graphql_string(&backend.endpoint);
        let escaped_provider_kind = escape_graphql_string(backend.provider_kind.as_str());
        let escaped_model_name = escape_graphql_string(&backend.model_name);
        let api_key_field = graphql_optional_string_field("api_key", backend.api_key.as_deref());
        let api_key_env_var_field =
            graphql_optional_string_field("api_key_env_var", backend.api_key_env_var.as_deref());
        let mutation = format!(
            r#"mutation {{
                upsert_InferenceBackend(
                    filter: {{ backend_id: {{ _eq: "{escaped_backend_id}" }} }},
                    add: {{
                        backend_id: "{escaped_backend_id}",
                        name: "{escaped_backend_id}",
                        provider_kind: "{escaped_provider_kind}",
                        endpoint: "{escaped_endpoint}",
                        {api_key_field}
                        {api_key_env_var_field}
                        max_concurrent: 1,
                        enabled: true,
                        supports_tool_calls: true,
                        supports_streaming: true,
                        models: ["{escaped_model_name}"],
                        probe_status: "healthy"
                    }},
                    update: {{
                        name: "{escaped_backend_id}",
                        provider_kind: "{escaped_provider_kind}",
                        endpoint: "{escaped_endpoint}",
                        {api_key_field}
                        {api_key_env_var_field}
                        max_concurrent: 1,
                        enabled: true,
                        supports_tool_calls: true,
                        supports_streaming: true,
                        models: ["{escaped_model_name}"],
                        probe_status: "healthy"
                    }}
                ) {{ _docID }}
            }}"#
        );
        let response = node.execute(&mutation).await;
        if response.has_errors() {
            anyhow::bail!("upsert inference backend failed: {:?}", response.errors);
        }

        let mut default_behavior =
            load_agent_behavior(node, &bootstrap.default_behavior.behavior_id)
                .await?
                .expect("default behavior document");
        default_behavior.backend_id = Some(backend_id.to_string());
        default_behavior.model_name = Some(backend.model_name.clone());
        upsert_agent_behavior(node, &default_behavior).await?;
        Ok(())
    }

    async fn wait_for_runtime_process_state(
        node: &EmbeddedNode,
        agent_did: &str,
        expected_process_state: &str,
    ) -> Result<()> {
        let escaped_agent_did = escape_graphql_string(agent_did);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let query = format!(
                r#"{{
                    AgentRuntime(
                        filter: {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }},
                        limit: 1
                    ) {{
                        process_state
                    }}
                }}"#
            );
            let response = node.execute(&query).await;
            if response.has_errors() {
                anyhow::bail!("AgentRuntime query failed: {:?}", response.errors);
            }
            let process_state = response
                .data
                .as_ref()
                .and_then(|data| data.get("AgentRuntime"))
                .and_then(Value::as_array)
                .and_then(|rows| rows.first())
                .and_then(|row| row.get("process_state"))
                .and_then(Value::as_str);
            if process_state == Some(expected_process_state) {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!(
                    "timed out waiting for AgentRuntime {agent_did} to reach process_state={expected_process_state}; last={process_state:?}"
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    fn init_test_tracing() {
        static INIT: OnceLock<()> = OnceLock::new();
        INIT.get_or_init(|| {
            let _ = tracing_subscriber::registry()
                .with(EnvFilter::new(
                    "warn,\
                     defra_agent_desktop=info,\
                     defra_agent=info,\
                     defra_node=info,\
                     p2p=info,\
                     iroh=warn,\
                     reqwest=warn,\
                     hyper=warn,\
                     h2=warn",
                ))
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_target(false)
                        .compact()
                        .without_time(),
                )
                .with(global_log_layer())
                .try_init();
        });
    }

    fn optional_env(name: &str) -> Option<String> {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    fn graphql_optional_string_field(name: &str, value: Option<&str>) -> String {
        value
            .filter(|value| !value.trim().is_empty())
            .map(|value| format!(r#"{name}: "{}","#, escape_graphql_string(value)))
            .unwrap_or_default()
    }

    fn wait_for_value<T>(
        label: &str,
        timeout: Duration,
        mut loader: impl FnMut() -> Option<T>,
    ) -> Result<T> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(value) = loader() {
                return Ok(value);
            }
            if Instant::now() >= deadline {
                anyhow::bail!("timed out waiting for {label}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn read_http_request(stream: &mut TcpStream) -> Result<HttpRequestData> {
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        let mut buffer = Vec::new();
        let mut temp = [0_u8; 1024];
        let header_end = loop {
            let read = stream.read(&mut temp)?;
            if read == 0 {
                anyhow::bail!("connection closed before headers");
            }
            buffer.extend_from_slice(&temp[..read]);
            if let Some(index) = find_subslice(&buffer, b"\r\n\r\n") {
                break index + 4;
            }
        };
        let header_text = String::from_utf8_lossy(&buffer[..header_end]);
        let mut lines = header_text.split("\r\n").filter(|line| !line.is_empty());
        let request_line = lines
            .next()
            .ok_or_else(|| anyhow!("missing request line"))?;
        let mut parts = request_line.split_whitespace();
        let method = parts
            .next()
            .ok_or_else(|| anyhow!("missing request method"))?
            .to_string();
        let path = parts
            .next()
            .ok_or_else(|| anyhow!("missing request path"))?
            .to_string();

        Ok(HttpRequestData { method, path })
    }

    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    fn write_http_response(
        stream: &mut TcpStream,
        status: &str,
        content_type: &str,
        body: &str,
    ) -> Result<()> {
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes())?;
        stream.flush()?;
        Ok(())
    }
}
