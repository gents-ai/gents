pub mod behavior_readiness;
pub mod canonical;
pub mod chatgpt_oauth;
pub mod client_protocol;
pub mod enrollment;
pub mod event_delivery;
pub mod graphql;
pub mod message;
pub mod network_token;
pub mod persona;
pub mod rendered_request;
pub mod request_admission;
pub mod request_input;
pub mod request_lifecycle;
pub mod row;
pub mod schemas;
pub mod session;
pub mod session_hydration;
pub mod timeline;
pub mod tool_service_health;
pub mod transcript;

/// Shared product instructions consumed by CLI, desktop, and live acceptance.
pub const SETUP_STEWARD_PROMPT: &str = include_str!("../prompts/setup.md");

/// Shared first-run configurator grant; decoded using canonical Tools types.
pub const SETUP_SELF_CONFIG_JSON: &str = include_str!("../presets/setup-self-config.json");
