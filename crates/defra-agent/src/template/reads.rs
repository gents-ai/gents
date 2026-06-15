//! Parser-backed reads-collector and cache-safety guard (#497).

use std::collections::BTreeSet;

use minijinja::machinery::ast::{CallArg, Expr, Stmt};
use minijinja::machinery::parse;
use minijinja::syntax::SyntaxConfig;

use super::catalog::{Catalog, Volatility};
use super::TemplateError;

/// Collect the complete set of full variable refs a system template reads,
/// rejecting constructs that introduce bindings or control flow.
pub fn collect_system_reads(template: &str) -> Result<BTreeSet<String>, TemplateError> {
    let ast = parse(
        template,
        "system_prompt",
        SyntaxConfig::default(),
        Default::default(),
    )
    .map_err(|e| TemplateError::Parse(e.to_string()))?;
    let mut reads = BTreeSet::new();
    walk_stmt_system(&ast, &mut reads)?;
    Ok(reads)
}

/// Validate a system template against the runtime-owned catalog.
pub fn validate_system_template(template: &str, cat: &Catalog) -> Result<(), TemplateError> {
    let reads = collect_system_reads(template)?;
    for r in &reads {
        match cat.volatility(r) {
            Some(Volatility::RunConstant) => {}
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

/// Collect refs from a request-context/task template without the system-only
/// statement restriction. This is best-effort and is used only for lazy
/// provider evaluation, never as a guard.
pub fn collect_request_reads(template: &str) -> Result<BTreeSet<String>, TemplateError> {
    let ast = parse(
        template,
        "request_context",
        SyntaxConfig::default(),
        Default::default(),
    )
    .map_err(|e| TemplateError::Parse(e.to_string()))?;
    let mut reads = BTreeSet::new();
    walk_stmt_any(&ast, &mut reads);
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

fn walk_stmt_any(stmt: &Stmt<'_>, reads: &mut BTreeSet<String>) {
    match stmt {
        Stmt::Template(t) => {
            for child in &t.children {
                walk_stmt_any(child, reads);
            }
        }
        Stmt::EmitExpr(e) => {
            let _ = collect_expr(&e.expr, reads);
        }
        Stmt::ForLoop(f) => {
            let _ = collect_expr(&f.iter, reads);
            if let Some(filter) = &f.filter_expr {
                let _ = collect_expr(filter, reads);
            }
            for child in &f.body {
                walk_stmt_any(child, reads);
            }
            for child in &f.else_body {
                walk_stmt_any(child, reads);
            }
        }
        Stmt::IfCond(i) => {
            let _ = collect_expr(&i.expr, reads);
            for child in &i.true_body {
                walk_stmt_any(child, reads);
            }
            for child in &i.false_body {
                walk_stmt_any(child, reads);
            }
        }
        _ => {}
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
