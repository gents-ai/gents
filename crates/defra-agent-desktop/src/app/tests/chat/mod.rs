use super::*;

fn build_chat_driver(runtime: Arc<Runtime>, core: ClientCore) -> AuditDriver {
    let mut driver = build_driver(runtime, core, Arc::new(DesktopLogStore::new(64)));
    driver.app.state.onboarding.first_launch_redirect_done = true;
    driver.app.state.activity = Activity::Chat;
    driver
}

mod blocking;
mod multi_turn;
mod switching;
mod tool_loop;
mod transcript;
