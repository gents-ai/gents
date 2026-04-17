pub(crate) fn build_driver(
    runtime: Arc<Runtime>,
    core: ClientCore,
    log_store: Arc<DesktopLogStore>,
) -> AuditDriver {
    let ctx = egui::Context::default();
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let app = DesktopApp::from_parts(&cc, runtime, Some(Arc::new(core)), Vec::new(), log_store);
    AuditDriver::new(app, ctx)
}

pub(crate) fn assert_operator_filter_round_trip(
    driver: &mut AuditDriver,
    section: OperatorSection,
    query: &str,
    target_id: &str,
    missing_query: &str,
) -> Result<()> {
    driver.click_target(&audit::targets::operator_section(section));
    driver.wait_for_target(
        "operator filter input",
        Duration::from_secs(10),
        audit::targets::OPERATOR_ENTITY_FILTER,
    )?;
    driver.replace_text_in_target(audit::targets::OPERATOR_ENTITY_FILTER, query);
    let filtered_texts = driver.render();
    assert_eq!(driver.app.state.operator.entity_filter, query);
    assert!(
        !filtered_texts
            .iter()
            .any(|text| text.contains("No Matches")),
        "operator filter unexpectedly hid {target_id} in {section:?}"
    );
    assert!(driver.has_target(&audit::targets::operator_entity(target_id)));

    driver.replace_text_in_target(audit::targets::OPERATOR_ENTITY_FILTER, missing_query);
    let no_match_texts = driver.render();
    assert!(
        no_match_texts
            .iter()
            .any(|text| text.contains("No Matches")),
        "operator filter did not render No Matches for {missing_query}"
    );

    driver.replace_text_in_target(audit::targets::OPERATOR_ENTITY_FILTER, "");
    driver.wait_for_target(
        "operator filtered row after clearing filter",
        Duration::from_secs(10),
        &audit::targets::operator_entity(target_id),
    )?;
    Ok(())
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
        || target.starts_with("operator.deployment.")
        || target.starts_with("operator.agent.")
        || target.starts_with("operator.section.")
        || target.starts_with("peers.peer.")
        || target.starts_with("peers.agent.")
}

pub(crate) fn is_operator_entity_target(target: &str) -> bool {
    target.starts_with("operator.entity.")
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
        let rect = self
            .find_click_rect(target, false)
            .unwrap_or_else(|| panic!("unable to find audit target rect: {target}"));
        self.click_pos(rect.center())
    }

    fn click_interactable_target(&mut self, target: &str) -> Result<Vec<String>> {
        let rect = self
            .find_click_rect(target, true)
            .ok_or_else(|| anyhow!("unable to find audit target rect: {target}"))?;
        anyhow::ensure!(
            target_is_interactable(rect),
            "audit target is not interactable: {target} at {rect:?}"
        );
        Ok(self.click_pos_compact(rect.center()))
    }

    fn click_compact_target(&mut self, target: &str) -> Result<Vec<String>> {
        let rect = self
            .find_click_rect(target, false)
            .ok_or_else(|| anyhow!("unable to find audit target rect: {target}"))?;
        anyhow::ensure!(
            target_is_interactable(rect),
            "audit target is not interactable: {target} at {rect:?}"
        );
        Ok(self.click_pos_compact(rect.center()))
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

        if is_operator_entity_target(target) {
            for delta in [
                -260.0_f32, -260.0, -260.0, -260.0, -260.0, 260.0, 260.0, 260.0, 260.0, 260.0,
            ] {
                self.scroll_operator_entity_list(delta);
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
            self.find_click_rect(target, false).map(|_| {
                self.last_texts
                    .iter()
                    .map(|run| run.text.clone())
                    .collect::<Vec<_>>()
            })
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

    fn scroll_operator_entity_list(&mut self, delta_y: f32) -> Vec<String> {
        self.scroll_pos(egui::pos2(760.0, 520.0), delta_y)
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
