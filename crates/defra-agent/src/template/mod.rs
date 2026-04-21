//! Template rendering for event-driven tasks.
//!
//! Trigger configurations can embed MiniJinja templates whose variables are
//! bound from a [`TemplateScope`] — the firing event, and optionally the
//! originating document and user-supplied arguments. Rendering uses strict
//! undefined semantics (missing variables raise errors) with auto-escape
//! disabled so that rendered output stays literal.

#[cfg(test)]
mod tests;

pub(crate) struct TemplateScope {
    pub(crate) event: serde_json::Value,
    pub(crate) doc: Option<serde_json::Value>,
    pub(crate) args: Option<serde_json::Value>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum TemplateError {
    #[error("template parse error: {0}")]
    Parse(String),
    #[error("template render error: {0}")]
    Render(String),
    #[error("rendered output exceeds size cap ({0} bytes)")]
    SizeCap(usize),
}

pub(crate) const MAX_TEMPLATE_BYTES: usize = 64 * 1024;
pub(crate) const MAX_RENDERED_BYTES: usize = 1024 * 1024;
