pub mod canonical;
pub mod chatgpt_oauth;
pub mod client_protocol;
pub mod enrollment;
pub mod event_delivery;
pub mod graphql;
pub mod mailbox_question;
pub mod message;
pub mod network_token;
pub mod node_readiness;
pub mod output;
pub mod peer_schema;
pub mod rendered_request;
pub mod request_admission;
pub mod request_input;
pub mod request_lifecycle;
pub mod row;
pub mod schemas;
pub mod serve_lifecycle;
pub mod session;
pub mod session_hydration;
pub mod session_input_edit;
pub mod timeline;
pub mod tool_service_health;
pub mod transcript;
pub mod trigger_delivery;

/// Shared product instructions consumed by CLI, desktop, and live acceptance.
pub const ENGINEER_PROMPT: &str = include_str!("../prompts/engineer.md");

/// Shared first-run configurator grant; decoded using canonical Tools types.
pub const ENGINEER_SELF_CONFIG_JSON: &str = include_str!("../presets/engineer-self-config.json");
