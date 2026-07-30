//! Parser-backed reads-collector and cache-safety guard (#497).

use std::collections::BTreeSet;

use minijinja::machinery::ast::{CallArg, Expr, Stmt};
use minijinja::machinery::parse;
use minijinja::syntax::SyntaxConfig;

use super::catalog::{Catalog, Site, Volatility};
use super::TemplateError;

pub fn collect_system_reads(template: &str) -> Result<BTreeSet<String>, TemplateError> {
    let ast = parse(template, "system_prompt", SyntaxConfig, Default::default())
        .map_err(|e| TemplateError::Parse(e.to_string()))?;
    let mut reads = BTreeSet::new();
    walk_stmt_system(&ast, &mut reads)?;
    Ok(reads)
}

pub fn validate_system_template(template: &str, cat: &Catalog) -> Result<(), TemplateError> {
    let reads = collect_system_reads(template)?;
    for r in &reads {
        match cat.volatility(r) {
            Some(Volatility::RunConstant) => {
                // Cache-safety is about volatility, but the catalog also models
                // available in the system preamble must not silently enter the
                if !cat.is_available_at(r, Site::System) {
                    return Err(TemplateError::Render(format!(
                        "system template references `{r}`, which is run-constant but not \
                         available in the system preamble; wrap literal braces in \
                         {{% raw %}}...{{% endraw %}} if intended as text"
                    )));
                }
            }
            Some(Volatility::PerRequest) => {
                return Err(TemplateError::Render(format!(
                    "system template may not read per-request variable `{r}`; move it to \
                     request_context_template, or wrap literal braces in {{% raw %}}...{{% endraw %}}"
                )));
            }
            None => {
                return Err(TemplateError::Render(format!(
                    "system template references unknown variable `{r}`; wrap literal braces in \
                     {{% raw %}}...{{% endraw %}} if intended as text"
                )));
            }
        }
    }
    Ok(())
}

pub fn collect_request_reads(template: &str) -> Result<BTreeSet<String>, TemplateError> {
    let ast = parse(
        template,
        "request_context",
        SyntaxConfig,
        Default::default(),
    )
    .map_err(|e| TemplateError::Parse(e.to_string()))?;
    let mut reads = BTreeSet::new();
    walk_stmt_request(&ast, &mut reads);
    Ok(reads)
}

fn walk_stmt_system(stmt: &Stmt<'_>, reads: &mut BTreeSet<String>) -> Result<(), TemplateError> {
    match stmt {
        Stmt::Template(t) => {
            for child in &t.children {
                walk_stmt_system(child, reads)?;
            }
            Ok(())
        }
        Stmt::EmitRaw(_) => Ok(()),
        Stmt::EmitExpr(e) => collect_expr(&e.expr, reads),
        _ => Err(TemplateError::Render(
            "system template may only use literal text and {{ variable }} substitutions \
             (no control flow, loops, set, macros, or filter blocks); wrap literal braces \
             in {% raw %}...{% endraw %}"
                .to_string(),
        )),
    }
}

fn walk_stmt_request(stmt: &Stmt<'_>, reads: &mut BTreeSet<String>) {
    match stmt {
        Stmt::Template(t) => {
            for child in &t.children {
                walk_stmt_request(child, reads);
            }
        }
        Stmt::EmitRaw(_) => {}
        Stmt::EmitExpr(e) => collect_request_expr(&e.expr, reads),
        Stmt::ForLoop(f) => {
            collect_request_expr(&f.iter, reads);
            if let Some(filter) = &f.filter_expr {
                collect_request_expr(filter, reads);
            }
            for child in &f.body {
                walk_stmt_request(child, reads);
            }
            for child in &f.else_body {
                walk_stmt_request(child, reads);
            }
        }
        Stmt::IfCond(i) => {
            collect_request_expr(&i.expr, reads);
            for child in &i.true_body {
                walk_stmt_request(child, reads);
            }
            for child in &i.false_body {
                walk_stmt_request(child, reads);
            }
        }
        Stmt::WithBlock(w) => {
            for (_target, value) in &w.assignments {
                collect_request_expr(value, reads);
            }
            for child in &w.body {
                walk_stmt_request(child, reads);
            }
        }
        Stmt::Set(s) => collect_request_expr(&s.expr, reads),
        Stmt::SetBlock(s) => {
            if let Some(filter) = &s.filter {
                collect_request_expr(filter, reads);
            }
            for child in &s.body {
                walk_stmt_request(child, reads);
            }
        }
        Stmt::AutoEscape(a) => {
            collect_request_expr(&a.enabled, reads);
            for child in &a.body {
                walk_stmt_request(child, reads);
            }
        }
        Stmt::FilterBlock(f) => {
            collect_request_expr(&f.filter, reads);
            for child in &f.body {
                walk_stmt_request(child, reads);
            }
        }
        Stmt::Do(d) => {
            collect_request_expr(&d.call.expr, reads);
            collect_request_call_args(&d.call.args, reads);
        }
    }
}

fn collect_request_expr(expr: &Expr<'_>, reads: &mut BTreeSet<String>) {
    if let Some(path) = dotted_path(expr) {
        reads.insert(path);
        return;
    }
    match expr {
        Expr::Var(_) | Expr::Const(_) => {}
        Expr::GetAttr(g) => collect_request_expr(&g.expr, reads),
        Expr::GetItem(g) => {
            collect_request_expr(&g.expr, reads);
            collect_request_expr(&g.subscript_expr, reads);
        }
        Expr::Slice(s) => {
            collect_request_expr(&s.expr, reads);
            for part in [&s.start, &s.stop, &s.step].into_iter().flatten() {
                collect_request_expr(part, reads);
            }
        }
        Expr::UnaryOp(u) => collect_request_expr(&u.expr, reads),
        Expr::BinOp(b) => {
            collect_request_expr(&b.left, reads);
            collect_request_expr(&b.right, reads);
        }
        Expr::Compare(c) => {
            collect_request_expr(&c.expr, reads);
            for op in &c.ops {
                collect_request_expr(&op.expr, reads);
            }
        }
        Expr::IfExpr(i) => {
            collect_request_expr(&i.test_expr, reads);
            collect_request_expr(&i.true_expr, reads);
            if let Some(false_expr) = &i.false_expr {
                collect_request_expr(false_expr, reads);
            }
        }
        Expr::Filter(f) => {
            if let Some(inner) = &f.expr {
                collect_request_expr(inner, reads);
            }
            collect_request_call_args(&f.args, reads);
        }
        Expr::Test(t) => {
            collect_request_expr(&t.expr, reads);
            collect_request_call_args(&t.args, reads);
        }
        Expr::Call(c) => {
            collect_request_expr(&c.expr, reads);
            collect_request_call_args(&c.args, reads);
        }
        Expr::List(l) => {
            for item in &l.items {
                collect_request_expr(item, reads);
            }
        }
        Expr::Map(m) => {
            for key in &m.keys {
                collect_request_expr(key, reads);
            }
            for value in &m.values {
                collect_request_expr(value, reads);
            }
        }
    }
}

fn collect_request_call_args(args: &[CallArg<'_>], reads: &mut BTreeSet<String>) {
    for arg in args {
        match arg {
            CallArg::Pos(expr)
            | CallArg::Kwarg(_, expr)
            | CallArg::PosSplat(expr)
            | CallArg::KwargSplat(expr) => collect_request_expr(expr, reads),
        }
    }
}

fn collect_expr(expr: &Expr<'_>, reads: &mut BTreeSet<String>) -> Result<(), TemplateError> {
    if let Some(path) = dotted_path(expr) {
        reads.insert(path);
        return Ok(());
    }

    match expr {
        Expr::GetAttr(g) => collect_expr(&g.expr, reads),
        Expr::GetItem(g) => {
            collect_expr(&g.expr, reads)?;
            collect_expr(&g.subscript_expr, reads)
        }
        Expr::Filter(f) => {
            if let Some(expr) = &f.expr {
                collect_expr(expr, reads)?;
            }
            collect_call_args(&f.args, reads)
        }
        Expr::Test(t) => {
            collect_expr(&t.expr, reads)?;
            collect_call_args(&t.args, reads)
        }
        Expr::Call(c) => {
            collect_expr(&c.expr, reads)?;
            collect_call_args(&c.args, reads)
        }
        Expr::BinOp(b) => {
            collect_expr(&b.left, reads)?;
            collect_expr(&b.right, reads)
        }
        Expr::UnaryOp(u) => collect_expr(&u.expr, reads),
        Expr::Var(_) | Expr::Const(_) => Ok(()),
        _ => Err(TemplateError::Render(
            "system template uses an unsupported expression; keep system templates to plain \
             {{ variable }} substitutions"
                .to_string(),
        )),
    }
}

fn collect_call_args(
    args: &[CallArg<'_>],
    reads: &mut BTreeSet<String>,
) -> Result<(), TemplateError> {
    for arg in args {
        match arg {
            CallArg::Pos(expr)
            | CallArg::Kwarg(_, expr)
            | CallArg::PosSplat(expr)
            | CallArg::KwargSplat(expr) => collect_expr(expr, reads)?,
        }
    }
    Ok(())
}

fn dotted_path(expr: &Expr<'_>) -> Option<String> {
    match expr {
        Expr::Var(v) => Some(v.id.to_string()),
        Expr::GetAttr(g) => {
            let base = dotted_path(&g.expr)?;
            Some(format!("{base}.{}", g.name))
        }
        Expr::GetItem(g) => {
            let base = dotted_path(&g.expr)?;
            let Expr::Const(subscript) = &g.subscript_expr else {
                return None;
            };
            let key = subscript.value.as_str()?;
            Some(format!("{base}.{key}"))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::catalog::Catalog;

    #[test]
    fn rejects_run_constant_var_not_available_in_system_preamble() {
        // A run-constant var that the catalog does NOT make available at the
        // system site must be rejected: cache-safety is about volatility, but
        // the guard must also honor the catalog's availability model so it stays
        // correct as the catalog grows (fail-closed, not safe-by-coincidence).
        let cat = Catalog::from_entries(&[(
            "node.future_var",
            Volatility::RunConstant,
            &[Site::RequestContext],
        )]);
        let err = validate_system_template("{{ node.future_var }}", &cat).unwrap_err();
        assert!(
            format!("{err}").contains("node.future_var"),
            "guard must reject run-constant vars not available in the system preamble: {err}"
        );
    }

    #[test]
    fn accepts_run_constant_var_available_in_system_preamble() {
        let cat = Catalog::from_entries(&[(
            "node.node_did",
            Volatility::RunConstant,
            &[Site::System, Site::RequestContext, Site::Task],
        )]);
        validate_system_template("agent {{ node.node_did }}", &cat)
            .expect("run-constant + system-available must pass");
    }

    #[test]
    fn request_reads_are_collected_inside_set_with_filter_bodies() {
        // The complete request-context walker must see refs hidden in statement
        // forms the old best-effort scan dropped (otherwise apply validation is
        // bypassable and the var only fails at first request).
        for template in [
            "{% set x = ctx.bogus %}{{ x }}",
            "{% with y = ctx.bogus %}{{ y }}{% endwith %}",
            "{% filter upper %}{{ ctx.bogus }}{% endfilter %}",
            "{% for i in node.list %}{{ ctx.bogus }}{% endfor %}",
            "{{ [ctx.bogus, node.node_did] }}",
        ] {
            let reads = collect_request_reads(template).unwrap();
            assert!(
                reads.contains("ctx.bogus"),
                "ctx.bogus must be collected from {template:?}, got {reads:?}"
            );
        }
    }
}
