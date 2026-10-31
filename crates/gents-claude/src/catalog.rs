//! The model catalog baked into Claude Code 2.1.311: aliases, per-provider
//! ids, output limits, context windows, capabilities and default effort.
//!
//! The data is a generated `static` table (`catalog_data.rs`), so lookups are
//! linear scans over 22 entries with no parsing and no allocation. The table
//! mirrors the client's literal exactly: its `defaults` and `alias_migration`
//! sections are empty in this release and so have no representation, and a
//! boolean the literal leaves out reads as `false`.

#[path = "catalog_data.rs"]
mod catalog_data;

/// The `[1m]` model suffix that asks for the one-million-token window.
const SUFFIX_1M: &str = "[1m]";

/// Version of the catalog schema the literal declares.
pub const SCHEMA_VERSION: u32 = catalog_data::SCHEMA_VERSION;

/// The alias the client treats as its best model.
pub const BEST: &str = catalog_data::BEST;

/// Latest model id per family, as `(family, model id)`.
pub const LATEST_PER_FAMILY: &[(&str, &str)] = &catalog_data::LATEST_PER_FAMILY;

/// A provider the catalog carries model ids for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CatalogProvider {
    /// The first-party Anthropic API.
    FirstParty,
    /// AWS Bedrock.
    Bedrock,
    /// Google Vertex AI.
    Vertex,
    /// Microsoft Foundry.
    Foundry,
    /// Anthropic on AWS.
    AnthropicAws,
    /// Anthropic on Google Cloud.
    AnthropicGoogleCloud,
    /// Bedrock Mantle.
    Mantle,
    /// The cloud gateway.
    Gateway,
}

/// The id a model goes by on each provider (`None`: not offered there).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderIds {
    /// First-party API id.
    pub first_party: &'static str,
    /// Bedrock id.
    pub bedrock: Option<&'static str>,
    /// Vertex id.
    pub vertex: Option<&'static str>,
    /// Foundry id.
    pub foundry: Option<&'static str>,
    /// Anthropic-on-AWS id.
    pub anthropic_aws: Option<&'static str>,
    /// Anthropic-on-Google-Cloud id.
    pub anthropic_google_cloud: Option<&'static str>,
    /// Mantle id.
    pub mantle: Option<&'static str>,
    /// Gateway id.
    pub gateway: Option<&'static str>,
}

/// The context window of a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Context {
    /// Default window in tokens.
    pub window: u64,
    /// Largest window reachable by opting in, when above `window`.
    pub maximum_window: Option<u64>,
    /// The one-million window needs no opt-in.
    pub native_1m: bool,
    /// The one-million beta header opts in.
    pub supports_1m_beta: bool,
    /// The `[1m]` model suffix opts in.
    pub supports_1m_suffix: bool,
    /// Long contexts are clamped when the beta is absent.
    pub clamp_long_context_without_beta: bool,
}

/// Default and upper bound of `max_tokens`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputLimit {
    /// Default `max_tokens`.
    pub default: u64,
    /// Largest accepted `max_tokens`.
    pub upper: u64,
}

/// Relative cost of each effort level (`high` is 1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffortCost {
    /// Cost at `low` effort.
    pub low: f64,
    /// Cost at `medium` effort.
    pub medium: f64,
    /// Cost at `high` effort.
    pub high: f64,
    /// Cost at `xhigh` effort.
    pub xhigh: f64,
    /// Cost at `max` effort.
    pub max: f64,
}

/// Providers that stream tool input eagerly for a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EagerStreaming {
    /// Eager input streaming on Bedrock.
    pub bedrock: bool,
    /// Eager input streaming on Vertex.
    pub vertex: bool,
}

impl EagerStreaming {
    /// Whether tool definitions carry `eager_input_streaming` on `provider`
    /// (`k5t`). Only Vertex and Bedrock read the catalog flag, and only on
    /// their default endpoint (no custom base URL).
    pub fn enabled(&self, provider: CatalogProvider, default_endpoint: bool) -> bool {
        default_endpoint
            && match provider {
                CatalogProvider::Vertex => self.vertex,
                CatalogProvider::Bedrock => self.bedrock,
                _ => false,
            }
    }
}

/// Image size limits of a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageLimits {
    /// Widest accepted image in pixels.
    pub max_width: u32,
    /// Tallest accepted image in pixels.
    pub max_height: u32,
    /// Most tokens one image may cost.
    pub max_image_tokens: u32,
}

/// One model of the catalog.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Model {
    /// Catalog id.
    pub id: &'static str,
    /// Model family.
    pub family: &'static str,
    /// Human-readable name.
    pub display_name: &'static str,
    /// Training data cutoff, as the client prints it.
    pub knowledge_cutoff: Option<&'static str>,
    /// Ids per provider.
    pub provider_ids: ProviderIds,
    /// Context window, absent for legacy models.
    pub context: Option<Context>,
    /// `max_tokens` limits.
    pub max_output_tokens: OutputLimit,
    /// Name of the pricing tier, see [`pricing`].
    pub pricing: &'static str,
    /// Capability names, see [`Model::has`].
    pub capabilities: &'static [&'static str],
    /// Effort the client sends when none is chosen.
    pub default_effort: Option<&'static str>,
    /// Relative cost per effort level.
    pub effort_cost_index: Option<EffortCost>,
    /// Eager tool-input streaming support.
    pub eager_input_streaming: EagerStreaming,
    /// Model the client falls back to on third-party providers.
    pub fallback_3p: Option<&'static str>,
    /// Environment variable that overrides the Vertex region.
    pub vertex_region_env_var: Option<&'static str>,
    /// Image limits, absent for legacy models.
    pub image_limits: Option<ImageLimits>,
    /// Rank among advisor models, lower is preferred.
    pub advisor_rank: Option<u32>,
}

/// Per-million-token prices in USD.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PricingTier {
    /// Input tokens.
    pub input: f64,
    /// Output tokens.
    pub output: f64,
    /// Cache writes with the 5 minute TTL.
    pub cache_write_5m: f64,
    /// Cache writes with the 1 hour TTL.
    pub cache_write_1h: f64,
    /// Cache reads.
    pub cache_read: f64,
    /// Price of one web search in USD.
    pub web_search: f64,
    /// Prices that replace the above past a prompt size.
    pub long_prompt: Option<LongPrompt>,
}

/// Prices applying once a prompt exceeds a size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LongPrompt {
    /// Prompt tokens above which these prices apply.
    pub above_prompt_tokens: u64,
    /// Input tokens.
    pub input: f64,
    /// Output tokens.
    pub output: f64,
    /// Cache writes with the 5 minute TTL.
    pub cache_write_5m: f64,
    /// Cache writes with the 1 hour TTL.
    pub cache_write_1h: f64,
    /// Cache reads.
    pub cache_read: f64,
}

/// A model alias and its per-provider targets (catalog model ids).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Alias {
    /// Alias name, such as `opus`.
    pub name: &'static str,
    /// Target on providers without an entry of their own.
    pub default: &'static str,
    /// Provider-specific targets.
    pub per_provider: &'static [(CatalogProvider, &'static str)],
}

/// Every model of the catalog, in the client's order.
pub fn models() -> &'static [Model] {
    &catalog_data::MODELS
}

/// Every alias of the catalog.
pub fn aliases() -> &'static [Alias] {
    &catalog_data::ALIASES
}

/// The model whose id or any provider id is `id`, ignoring a trailing `[1m]`.
pub fn find(id: &str) -> Option<&'static Model> {
    let id = strip_1m_suffix(id);
    models().iter().find(|model| model.matches(id))
}

/// Strips a trailing `[1m]`, case-insensitively.
pub fn strip_1m_suffix(id: &str) -> &str {
    id.len()
        .checked_sub(SUFFIX_1M.len())
        .and_then(|cut| {
            id.get(cut..)
                .filter(|tail| tail.eq_ignore_ascii_case(SUFFIX_1M))
                .map(|_| &id[..cut])
        })
        .unwrap_or(id)
}

/// The catalog model id an alias names on a provider (`None` provider: the default).
pub fn resolve_alias(alias: &str, provider: Option<CatalogProvider>) -> Option<&'static str> {
    let alias = aliases().iter().find(|entry| entry.name == alias)?;
    let specific = provider.and_then(|p| alias.per_provider.iter().find(|(key, _)| *key == p));
    Some(specific.map_or(alias.default, |(_, id)| id))
}

/// The pricing tier called `tier`.
pub fn pricing(tier: &str) -> Option<&'static PricingTier> {
    catalog_data::PRICING_TIERS
        .iter()
        .find(|(name, _)| *name == tier)
        .map(|(_, prices)| prices)
}

impl Model {
    /// Whether the model has the capability named `capability`.
    pub fn has(&self, capability: &str) -> bool {
        self.capabilities.contains(&capability)
    }

    /// The model's id on `provider`; the gateway falls back to the first-party id.
    pub fn id_for(&self, provider: CatalogProvider) -> Option<&'static str> {
        let ids = &self.provider_ids;
        match provider {
            CatalogProvider::FirstParty => Some(ids.first_party),
            CatalogProvider::Bedrock => ids.bedrock,
            CatalogProvider::Vertex => ids.vertex,
            CatalogProvider::Foundry => ids.foundry,
            CatalogProvider::AnthropicAws => ids.anthropic_aws,
            CatalogProvider::AnthropicGoogleCloud => ids.anthropic_google_cloud,
            CatalogProvider::Mantle => ids.mantle,
            CatalogProvider::Gateway => ids.gateway.or(Some(ids.first_party)),
        }
    }

    fn matches(&self, id: &str) -> bool {
        let ids = &self.provider_ids;
        self.id == id
            || ids.first_party == id
            || [
                ids.bedrock,
                ids.vertex,
                ids.foundry,
                ids.anthropic_aws,
                ids.anthropic_google_cloud,
                ids.mantle,
                ids.gateway,
            ]
            .contains(&Some(id))
    }
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
