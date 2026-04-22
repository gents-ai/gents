pub(crate) fn build_driver(
    runtime: Arc<Runtime>,
    core: ClientCore,
    log_store: Arc<DesktopLogStore>,
) -> AuditDriver {
    build_driver_with_client(runtime, Arc::new(core), log_store)
}

pub(crate) fn build_driver_with_client(
    runtime: Arc<Runtime>,
    core: Arc<ClientCore>,
    log_store: Arc<DesktopLogStore>,
) -> AuditDriver {
    let ctx = egui::Context::default();
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let app = DesktopApp::from_parts(&cc, runtime, Some(core), Vec::new(), log_store);
    AuditDriver::new(app, ctx)
}

pub(crate) fn render_once(app: &mut DesktopApp, ctx: &egui::Context) -> Vec<String> {
    render_frame(app, ctx, 0.0, Vec::new())
        .into_iter()
        .map(|run| run.text)
        .collect()
}

pub(crate) fn audit_screen_rect() -> egui::Rect {
    egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 960.0))
}

pub(crate) fn target_is_interactable(rect: egui::Rect) -> bool {
    let visible = audit_screen_rect().shrink2(egui::vec2(8.0, 8.0));
    visible.intersects(rect) && visible.contains(rect.center())
}

pub(crate) fn is_activity_sidebar_target(target: &str) -> bool {
    target.starts_with("chat.deployment.")
        || target.starts_with("chat.agent.")
        || target.starts_with("chat.conversation.")
        || target.starts_with("manage.deployment.")
        || target.starts_with("manage.agent.")
        || target.starts_with("manage.section.")
        || target.starts_with("setup.deployment.")
        || target.starts_with("setup.agent.")
}

pub(crate) fn is_manage_entity_target(target: &str) -> bool {
    target.starts_with("manage.entity.")
}

pub(crate) fn is_manage_editor_target(target: &str) -> bool {
    target.starts_with("manage.field.")
        || target.starts_with("manage.toggle.")
        || target == audit::targets::MANAGE_APPLY
        || target == audit::targets::MANAGE_DISCARD
        || target == audit::targets::MANAGE_RUN_NOW
}

fn manage_section_from_target(target: &str) -> Option<ManageSection> {
    [
        ManageSection::Behaviors,
        ManageSection::Backends,
        ManageSection::ToolSelections,
        ManageSection::InferenceProfiles,
        ManageSection::ScheduledTasks,
        ManageSection::RequestTimeline,
        ManageSection::RecentFailures,
    ]
    .into_iter()
    .find(|section| audit::targets::manage_section(*section) == target)
}

#[derive(Debug, Clone)]
pub(crate) struct TextRun {
    text: String,
}

pub(crate) struct AuditDriver {
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
        let Some(rect) = self.find_click_rect(target, false) else {
            if let Some(section) = manage_section_from_target(target) {
                return self.select_manage_section(section);
            }
            panic!("unable to find audit target rect: {target}");
        };
        let texts = self.click_pos(rect.center());
        self.post_click_target(target, texts)
    }

    fn click_interactable_target(&mut self, target: &str) -> Result<Vec<String>> {
        let Some(rect) = self.find_click_rect(target, true) else {
            if let Some(section) = manage_section_from_target(target) {
                return Ok(self.select_manage_section(section));
            }
            anyhow::bail!("unable to find audit target rect: {target}");
        };
        anyhow::ensure!(
            target_is_interactable(rect),
            "audit target is not interactable: {target} at {rect:?}"
        );
        let texts = self.click_pos_compact(rect.center());
        Ok(self.post_click_target(target, texts))
    }

    fn click_compact_target(&mut self, target: &str) -> Result<Vec<String>> {
        let Some(rect) = self.find_click_rect(target, false) else {
            if let Some(section) = manage_section_from_target(target) {
                return Ok(self.select_manage_section(section));
            }
            anyhow::bail!("unable to find audit target rect: {target}");
        };
        anyhow::ensure!(
            target_is_interactable(rect),
            "audit target is not interactable: {target} at {rect:?}"
        );
        let texts = self.click_pos_compact(rect.center());
        Ok(self.post_click_target(target, texts))
    }

    fn find_click_rect(&mut self, target: &str, require_interactable: bool) -> Option<egui::Rect> {
        if let Some(rect) = self.current_click_rect(target, require_interactable) {
            return Some(rect);
        }

        self.render();

        if let Some(rect) = self.current_click_rect(target, require_interactable) {
            return Some(rect);
        }

        if is_activity_sidebar_target(target) {
            for delta in [
                -220.0_f32, -220.0, -220.0, -220.0, -220.0, 220.0, 220.0, 220.0, 220.0, 220.0,
            ] {
                self.scroll_activity_sidebar(delta);
                if let Some(rect) = audit::target_interact_rect(&self.ctx, target)
                    .filter(|rect| !require_interactable || target_is_interactable(*rect))
                {
                    return Some(rect);
                }
            }
        }

        if is_manage_entity_target(target) {
            for delta in [
                -260.0_f32, -260.0, -260.0, -260.0, -260.0, 260.0, 260.0, 260.0, 260.0, 260.0,
            ] {
                self.scroll_manage_entity_list(delta);
                if let Some(rect) = audit::target_interact_rect(&self.ctx, target)
                    .filter(|rect| !require_interactable || target_is_interactable(*rect))
                {
                    return Some(rect);
                }
            }
        }

        if is_manage_editor_target(target) {
            for delta in [
                -220.0_f32, -220.0, -220.0, -220.0, 220.0, 220.0, 220.0, 220.0,
            ] {
                self.scroll_manage_editor(delta);
                if let Some(rect) = audit::target_interact_rect(&self.ctx, target)
                    .filter(|rect| !require_interactable || target_is_interactable(*rect))
                {
                    return Some(rect);
                }
            }
        }

        if require_interactable {
            None
        } else {
            audit::target_rect(&self.ctx, target).filter(|rect| target_is_interactable(*rect))
        }
    }

    fn current_click_rect(&self, target: &str, require_interactable: bool) -> Option<egui::Rect> {
        if let Some(rect) = audit::target_interact_rect(&self.ctx, target)
            .filter(|rect| !require_interactable || target_is_interactable(*rect))
        {
            return Some(rect);
        }

        if require_interactable {
            None
        } else {
            audit::target_rect(&self.ctx, target).filter(|rect| target_is_interactable(*rect))
        }
    }

    fn has_target(&mut self, target: &str) -> bool {
        self.render();
        audit::target_rect(&self.ctx, target).is_some()
    }

    fn open_activity(&mut self, activity: Activity) -> Vec<String> {
        if self.app.state.activity != activity {
            match activity {
                Activity::Manage => {
                    if self.has_target(audit::targets::activity(activity)) {
                        let _ = self.click_target(audit::targets::activity(activity));
                    } else if let Some(target) = self.manage_activity_target() {
                        let _ = self.click_target(&target);
                    }
                }
                Activity::Chat => {}
            }
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
            self.find_click_rect(target, false).map(|_| {
            self.last_texts
                    .iter()
                    .map(|run| run.text.clone())
                    .collect::<Vec<_>>()
            })
        })
    }

    fn manage_activity_target(&mut self) -> Option<String> {
        self.render();

        self.app
            .state
            .manage
            .selected_peer_id
            .as_deref()
            .map(audit::targets::manage_deployment)
            .filter(|target| self.current_click_rect(target, false).is_some())
            .or_else(|| {
                self.app
                    .state
                    .chat
                    .shell
                    .selected_peer_id
                    .as_deref()
                    .map(audit::targets::manage_deployment)
                    .filter(|target| self.current_click_rect(target, false).is_some())
            })
            .or_else(|| {
                self.app.client.as_ref().and_then(|client| {
                    client
                        .peer_statuses()
                        .into_iter()
                        .next()
                        .map(|status| audit::targets::manage_deployment(&status.peer_id))
                        .filter(|target| self.current_click_rect(target, false).is_some())
                })
            })
    }

    fn post_click_target(&mut self, target: &str, texts: Vec<String>) -> Vec<String> {
        let Some(section) = manage_section_from_target(target) else {
            return texts;
        };

        for _ in 0..6 {
            if self.app.state.manage.selected_section == section {
                return self.render();
            }
            self.render();
        }

        self.app.state.queue_shell_action(crate::state::PendingShellAction::Manage(
            crate::state::PendingManageAction::SelectSection { section },
        ));
        self.render()
    }

    fn select_manage_section(&mut self, section: ManageSection) -> Vec<String> {
        self.app.state.queue_shell_action(crate::state::PendingShellAction::Manage(
            crate::state::PendingManageAction::SelectSection { section },
        ));
        self.render()
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
        ]);
        self.run_events(Vec::new())
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
        ]);
        self.run_events(Vec::new())
    }

    fn click_pos_compact(&mut self, pos: egui::Pos2) -> Vec<String> {
        self.run_events(vec![egui::Event::PointerMoved(pos)]);
        self.run_events(vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        self.run_events(Vec::new())
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

    fn scroll_activity_sidebar(&mut self, delta_y: f32) -> Vec<String> {
        self.scroll_pos(egui::pos2(180.0, 780.0), delta_y)
    }

    fn scroll_manage_entity_list(&mut self, delta_y: f32) -> Vec<String> {
        self.scroll_pos(egui::pos2(760.0, 520.0), delta_y)
    }

    fn scroll_manage_editor(&mut self, delta_y: f32) -> Vec<String> {
        self.scroll_pos(egui::pos2(1120.0, 520.0), delta_y)
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
            if audit::target_interact_rect(&self.ctx, target).is_some_and(target_is_interactable) {
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

pub(crate) fn render_frame(
    app: &mut DesktopApp,
    ctx: &egui::Context,
    time: f64,
    events: Vec<egui::Event>,
) -> Vec<TextRun> {
    let mut frame = eframe::Frame::_new_kittest();
    app.logic(ctx, &mut frame);

    audit::begin_frame(ctx);
    let output = ctx.run_ui(test_raw_input(time, events), |ui| app.ui(ui, &mut frame));

    collect_text_runs(&output.shapes)
}

pub(crate) fn test_raw_input(time: f64, events: Vec<egui::Event>) -> egui::RawInput {
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

pub(crate) fn collect_text_runs(shapes: &[egui::epaint::ClippedShape]) -> Vec<TextRun> {
    let mut texts = Vec::new();
    for shape in shapes {
        collect_shape_text(&shape.shape, &mut texts);
    }
    texts
}

pub(crate) fn collect_shape_text(shape: &egui::epaint::Shape, texts: &mut Vec<TextRun>) {
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
