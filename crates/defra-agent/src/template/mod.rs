//! Template rendering for event-driven tasks.
//!
//! Trigger configurations can embed MiniJinja templates whose variables are
//! bound from a [`TemplateScope`] — the firing event, and optionally the
//! originating document and user-supplied arguments. Rendering uses strict
//! undefined semantics (missing variables raise errors) with auto-escape
//! disabled so that rendered output stays literal.

#[cfg(test)]
mod tests;

use minijinja::{AutoEscape, Environment, UndefinedBehavior};

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

/// Render `template` against `scope` using MiniJinja with strict-undefined
/// semantics and auto-escape disabled.
///
/// The input template is rejected if it exceeds [`MAX_TEMPLATE_BYTES`]; the
/// rendered output is rejected if it exceeds [`MAX_RENDERED_BYTES`]. Both
/// caps keep trigger evaluation bounded regardless of event payload size.
pub(crate) fn render_template(
    template: &str,
    scope: &TemplateScope,
) -> Result<String, TemplateError> {
    if template.len() > MAX_TEMPLATE_BYTES {
        return Err(TemplateError::Parse(format!(
            "template exceeds {} bytes",
            MAX_TEMPLATE_BYTES
        )));
    }

    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    env.set_auto_escape_callback(|_| AutoEscape::None);

    let context = {
        let mut ctx = serde_json::Map::new();
        ctx.insert("event".to_string(), scope.event.clone());
        if let Some(doc) = scope.doc.clone() {
            ctx.insert("doc".to_string(), doc);
        }
        if let Some(args) = scope.args.clone() {
            ctx.insert("args".to_string(), args);
        }
        serde_json::Value::Object(ctx)
    };

    let tmpl = env
        .template_from_str(template)
        .map_err(|e| TemplateError::Parse(e.to_string()))?;
    let rendered = tmpl
        .render(&context)
        .map_err(|e| TemplateError::Render(e.to_string()))?;

    if rendered.len() > MAX_RENDERED_BYTES {
        return Err(TemplateError::SizeCap(rendered.len()));
    }
    Ok(rendered)
}
