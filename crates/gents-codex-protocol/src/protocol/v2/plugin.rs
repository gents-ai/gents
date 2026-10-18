use super::AppSummary;
use super::HookEventName;
use super::HookHandlerType;
use super::HookSource;
use super::HookTrustStatus;
use serde::Deserialize;
use serde::Serialize;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkillsListParams {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cwds: Vec<PathBuf>,

    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub force_reload: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkillsListResponse {
    pub data: Vec<SkillsListEntry>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HooksListParams {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cwds: Vec<PathBuf>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HooksListResponse {
    pub data: Vec<HooksListEntry>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceAddParams {
    pub source: String,
    pub ref_name: Option<String>,
    pub sparse_paths: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceAddResponse {
    pub marketplace_name: String,
    pub installed_root: AbsolutePathBuf,
    pub already_added: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceRemoveParams {
    pub marketplace_name: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceRemoveResponse {
    pub marketplace_name: String,
    pub installed_root: Option<AbsolutePathBuf>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceUpgradeParams {
    pub marketplace_name: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceUpgradeResponse {
    pub selected_marketplaces: Vec<String>,
    pub upgraded_roots: Vec<AbsolutePathBuf>,
    pub errors: Vec<MarketplaceUpgradeErrorInfo>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceUpgradeErrorInfo {
    pub marketplace_name: String,
    pub message: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginListParams {
    pub cwds: Option<Vec<AbsolutePathBuf>>,
    pub marketplace_kinds: Option<Vec<PluginListMarketplaceKind>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginInstalledParams {
    pub cwds: Option<Vec<AbsolutePathBuf>>,
    pub install_suggestion_plugin_names: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginListMarketplaceKind {
    #[serde(rename = "local")]
    Local,
    #[serde(rename = "vertical")]
    Vertical,
    #[serde(rename = "workspace-directory")]
    WorkspaceDirectory,
    #[serde(rename = "shared-with-me")]
    SharedWithMe,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginListResponse {
    pub marketplaces: Vec<PluginMarketplaceEntry>,
    #[serde(default)]
    pub marketplace_load_errors: Vec<MarketplaceLoadErrorInfo>,
    #[serde(default)]
    pub featured_plugin_ids: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginInstalledResponse {
    pub marketplaces: Vec<PluginMarketplaceEntry>,
    #[serde(default)]
    pub marketplace_load_errors: Vec<MarketplaceLoadErrorInfo>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceLoadErrorInfo {
    pub marketplace_path: AbsolutePathBuf,
    pub message: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginReadParams {
    pub marketplace_path: Option<AbsolutePathBuf>,
    pub remote_marketplace_name: Option<String>,
    pub plugin_name: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginReadResponse {
    pub plugin: PluginDetail,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginSkillReadParams {
    pub remote_marketplace_name: String,
    pub remote_plugin_id: String,
    pub skill_name: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginSkillReadResponse {
    pub contents: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginShareSaveParams {
    pub plugin_path: AbsolutePathBuf,
    pub remote_plugin_id: Option<String>,
    pub discoverability: Option<PluginShareDiscoverability>,
    pub share_targets: Option<Vec<PluginShareTarget>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginShareSaveResponse {
    pub remote_plugin_id: String,
    pub share_url: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginShareUpdateTargetsParams {
    pub remote_plugin_id: String,
    pub discoverability: PluginShareUpdateDiscoverability,
    pub share_targets: Vec<PluginShareTarget>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginShareUpdateTargetsResponse {
    pub principals: Vec<PluginSharePrincipal>,
    pub discoverability: PluginShareDiscoverability,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginShareListParams {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginShareListResponse {
    pub data: Vec<PluginShareListItem>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginShareCheckoutParams {
    pub remote_plugin_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginShareCheckoutResponse {
    pub remote_plugin_id: String,
    pub plugin_id: String,
    pub plugin_name: String,
    pub plugin_path: AbsolutePathBuf,
    pub marketplace_name: String,
    pub marketplace_path: AbsolutePathBuf,
    pub remote_version: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginShareDeleteParams {
    pub remote_plugin_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginShareDeleteResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginShareListItem {
    pub plugin: PluginSummary,
    pub local_plugin_path: Option<AbsolutePathBuf>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginShareDiscoverability {
    #[serde(rename = "LISTED")]
    Listed,
    #[serde(rename = "UNLISTED")]
    Unlisted,
    #[serde(rename = "PRIVATE")]
    Private,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginShareUpdateDiscoverability {
    #[serde(rename = "UNLISTED")]
    Unlisted,
    #[serde(rename = "PRIVATE")]
    Private,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginSharePrincipalType {
    #[serde(rename = "user")]
    User,
    #[serde(rename = "group")]
    Group,
    #[serde(rename = "workspace")]
    Workspace,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginShareTarget {
    pub principal_type: PluginSharePrincipalType,
    pub principal_id: String,
    pub role: PluginShareTargetRole,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginSharePrincipal {
    pub principal_type: PluginSharePrincipalType,
    pub principal_id: String,
    pub role: PluginSharePrincipalRole,
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PluginShareTargetRole {
    Reader,
    Editor,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PluginSharePrincipalRole {
    Reader,
    Editor,
    Owner,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkillScope {
    User,
    Repo,
    System,
    Admin,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkillMetadata {
    pub name: String,
    pub description: String,
    /// Legacy short_description from SKILL.md. Prefer SKILL.json interface.short_description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<SkillInterface>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<SkillDependencies>,
    pub path: AbsolutePathBuf,
    pub scope: SkillScope,
    pub enabled: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkillInterface {
    pub display_name: Option<String>,
    pub short_description: Option<String>,
    pub icon_small: Option<AbsolutePathBuf>,
    pub icon_large: Option<AbsolutePathBuf>,
    pub brand_color: Option<String>,
    pub default_prompt: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkillDependencies {
    pub tools: Vec<SkillToolDependency>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkillToolDependency {
    #[serde(rename = "type")]
    pub r#type: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkillErrorInfo {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkillsListEntry {
    pub cwd: PathBuf,
    pub skills: Vec<SkillMetadata>,
    pub errors: Vec<SkillErrorInfo>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HooksListEntry {
    pub cwd: PathBuf,
    pub hooks: Vec<HookMetadata>,
    pub warnings: Vec<String>,
    pub errors: Vec<HookErrorInfo>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HookMetadata {
    pub key: String,
    pub event_name: HookEventName,
    pub handler_type: HookHandlerType,
    pub matcher: Option<String>,
    pub command: Option<String>,
    pub timeout_sec: u64,
    pub status_message: Option<String>,
    pub source_path: AbsolutePathBuf,
    pub source: HookSource,
    pub plugin_id: Option<String>,
    pub display_order: i64,
    pub enabled: bool,
    pub is_managed: bool,
    pub current_hash: String,
    pub trust_status: HookTrustStatus,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HookErrorInfo {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginMarketplaceEntry {
    pub name: String,
    pub path: Option<AbsolutePathBuf>,
    pub interface: Option<MarketplaceInterface>,
    pub plugins: Vec<PluginSummary>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceInterface {
    pub display_name: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginInstallPolicy {
    #[serde(rename = "NOT_AVAILABLE")]
    NotAvailable,
    #[serde(rename = "AVAILABLE")]
    Available,
    #[serde(rename = "INSTALLED_BY_DEFAULT")]
    InstalledByDefault,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginAuthPolicy {
    #[serde(rename = "ON_INSTALL")]
    OnInstall,
    #[serde(rename = "ON_USE")]
    OnUse,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PluginAvailability {
    #[serde(rename = "AVAILABLE", alias = "ENABLED")]
    #[default]
    Available,
    #[serde(rename = "DISABLED_BY_ADMIN")]
    DisabledByAdmin,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginSummary {
    pub id: String,
    pub remote_plugin_id: Option<String>,
    #[serde(default)]
    pub local_version: Option<String>,
    pub name: String,
    pub share_context: Option<PluginShareContext>,
    pub source: PluginSource,
    pub installed: bool,
    pub enabled: bool,
    pub install_policy: PluginInstallPolicy,
    pub auth_policy: PluginAuthPolicy,
    #[serde(default)]
    pub availability: PluginAvailability,
    pub interface: Option<PluginInterface>,
    #[serde(default)]
    pub keywords: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginShareContext {
    pub remote_plugin_id: String,
    #[serde(default)]
    pub remote_version: Option<String>,
    pub discoverability: Option<PluginShareDiscoverability>,
    pub share_url: Option<String>,
    pub creator_account_user_id: Option<String>,
    pub creator_name: Option<String>,
    pub share_principals: Option<Vec<PluginSharePrincipal>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginDetail {
    pub marketplace_name: String,
    pub marketplace_path: Option<AbsolutePathBuf>,
    pub summary: PluginSummary,
    pub description: Option<String>,
    pub skills: Vec<SkillSummary>,
    pub hooks: Vec<PluginHookSummary>,
    pub apps: Vec<AppSummary>,
    pub mcp_servers: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginHookSummary {
    pub key: String,
    pub event_name: HookEventName,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
    pub short_description: Option<String>,
    pub interface: Option<SkillInterface>,
    pub path: Option<AbsolutePathBuf>,
    pub enabled: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginInterface {
    pub display_name: Option<String>,
    pub short_description: Option<String>,
    pub long_description: Option<String>,
    pub developer_name: Option<String>,
    pub category: Option<String>,
    pub capabilities: Vec<String>,
    pub website_url: Option<String>,
    pub privacy_policy_url: Option<String>,
    pub terms_of_service_url: Option<String>,
    pub default_prompt: Option<Vec<String>>,
    pub brand_color: Option<String>,
    pub composer_icon: Option<AbsolutePathBuf>,
    pub composer_icon_url: Option<String>,
    pub logo: Option<AbsolutePathBuf>,
    pub logo_url: Option<String>,
    pub screenshots: Vec<AbsolutePathBuf>,
    pub screenshot_urls: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PluginSource {
    #[serde(rename_all = "camelCase")]
    Local { path: AbsolutePathBuf },
    #[serde(rename_all = "camelCase")]
    Git {
        url: String,
        path: Option<String>,
        ref_name: Option<String>,
        sha: Option<String>,
    },
    /// The plugin is available in the remote catalog. Download metadata is
    /// kept server-side and is not exposed through the app-server API.
    Remote,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkillsConfigWriteParams {
    pub path: Option<AbsolutePathBuf>,
    pub name: Option<String>,
    pub enabled: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkillsConfigWriteResponse {
    pub effective_enabled: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginInstallParams {
    pub marketplace_path: Option<AbsolutePathBuf>,
    pub remote_marketplace_name: Option<String>,
    pub plugin_name: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginInstallResponse {
    pub auth_policy: PluginAuthPolicy,
    pub apps_needing_auth: Vec<AppSummary>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginUninstallParams {
    pub plugin_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginUninstallResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkillsChangedNotification {}
use crate::core_types::AbsolutePathBuf;
