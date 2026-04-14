use std::sync::Arc;

use defra_agent_desktop::app::DesktopApp;
use defra_agent_desktop::telemetry::global_log_layer;
use eframe::egui;
use tracing_subscriber::{prelude::*, EnvFilter};

fn main() -> eframe::Result<()> {
    init_tracing();
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("desktop-client")
            .build()
            .map_err(|error| eframe::Error::AppCreation(Box::new(error)))?,
    );

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("defra-agent desktop")
            .with_inner_size([1480.0, 920.0])
            .with_min_inner_size([1180.0, 720.0]),
        ..Default::default()
    };

    eframe::run_native(
        "defra-agent desktop",
        options,
        Box::new(move |cc| Ok(Box::new(DesktopApp::new(cc, Arc::clone(&runtime))))),
    )
}

fn init_tracing() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(
            "warn,\
             defra_agent_desktop=trace,\
             defra_agent=info,\
             defra_node=info,\
             p2p=info,\
             iroh=info,\
             wgpu=warn,\
             winit=warn,\
             eframe=warn,\
             egui=warn,\
             naga=warn",
        )
    });

    let _ = tracing_subscriber::registry()
        .with(env_filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .compact()
                .without_time(),
        )
        .with(global_log_layer())
        .try_init();

    tracing::info!("launching defra-agent-desktop");
}
