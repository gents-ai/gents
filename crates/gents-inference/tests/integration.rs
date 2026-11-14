#[cfg(feature = "test-fixtures")]
#[path = "activity_views.rs"]
mod activity_views;
#[cfg(feature = "test-fixtures")]
#[path = "agent_views_conformance.rs"]
mod agent_views_conformance;
#[cfg(feature = "test-fixtures")]
#[path = "configuration_views.rs"]
mod configuration_views;
#[cfg(feature = "test-fixtures")]
#[path = "context_views.rs"]
mod context_views;
#[path = "documents_config.rs"]
mod documents_config;
#[path = "documents_transactions.rs"]
mod documents_transactions;
#[cfg(feature = "test-fixtures")]
#[path = "inference_views_conformance.rs"]
mod inference_views_conformance;
#[cfg(feature = "test-fixtures")]
#[path = "shared_views.rs"]
mod shared_views;
#[path = "skill_persistence.rs"]
mod skill_persistence;
