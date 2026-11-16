//! Refuses a call whose literal size would build a value too large for the sandbox.
//!
//! The engine's memory pool sees the operators' buffers, not what a single function builds:
//! `repeat('ab', 1000000000)` allocates a gigabyte in one step and the host kills the plugin.
//! The sizes of the functions that build a value from a count are checked while the plan is
//! analyzed, before constant folding would run them. A size that comes from a column is not
//! known here and is left to the pool and the host's memory limit.
use datafusion::common::config::ConfigOptions;
use datafusion::common::tree_node::{TreeNode, TreeNodeRecursion};
use datafusion::common::{Result, ScalarValue, plan_err};
use datafusion::logical_expr::{Expr, LogicalPlan, Operator};
use datafusion::optimizer::analyzer::AnalyzerRule;

/// The most bytes of text one `repeat`, `lpad` or `rpad` call may build.
pub const MAX_TEXT_BYTES: i128 = 64 * 1024 * 1024;
/// The most elements one `range`, `generate_series` or `array_repeat` call may build.
pub const MAX_ELEMENTS: i128 = 20_000_000;

/// The analyzer rule that applies the caps.
#[derive(Debug)]
pub struct BoundedSizes;

/// The integer an expression of literals comes to, if it is one.
fn constant(e: &Expr) -> Option<i128> {
    match e {
        Expr::Literal(v, _) => match v {
            ScalarValue::Int8(Some(n)) => Some(i128::from(*n)),
            ScalarValue::Int16(Some(n)) => Some(i128::from(*n)),
            ScalarValue::Int32(Some(n)) => Some(i128::from(*n)),
            ScalarValue::Int64(Some(n)) => Some(i128::from(*n)),
            ScalarValue::UInt8(Some(n)) => Some(i128::from(*n)),
            ScalarValue::UInt16(Some(n)) => Some(i128::from(*n)),
            ScalarValue::UInt32(Some(n)) => Some(i128::from(*n)),
            ScalarValue::UInt64(Some(n)) => Some(i128::from(*n)),
            _ => None,
        },
        Expr::Cast(c) => constant(&c.expr),
        Expr::TryCast(c) => constant(&c.expr),
        Expr::Alias(a) => constant(&a.expr),
        Expr::Negative(inner) => constant(inner)?.checked_neg(),
        // Integer `+`, `-` and `*` are planned as checked calls (see the checked module).
        Expr::ScalarFunction(f) if f.args.len() == 2 => {
            let (l, r) = (constant(&f.args[0])?, constant(&f.args[1])?);
            match f.name() {
                "checked_add" => l.checked_add(r),
                "checked_sub" => l.checked_sub(r),
                "checked_mul" => l.checked_mul(r),
                _ => None,
            }
        }
        Expr::BinaryExpr(b) => {
            let (l, r) = (constant(&b.left)?, constant(&b.right)?);
            match b.op {
                Operator::Plus => l.checked_add(r),
                Operator::Minus => l.checked_sub(r),
                Operator::Multiply => l.checked_mul(r),
                Operator::Divide => l.checked_div(r),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The length of a text literal, or 1 when the text is not a literal.
fn text_len(e: &Expr) -> i128 {
    match e {
        Expr::Literal(ScalarValue::Utf8(Some(s)) | ScalarValue::LargeUtf8(Some(s)), _) => {
            s.len().max(1) as i128
        }
        Expr::Cast(c) => text_len(&c.expr),
        _ => 1,
    }
}

fn too_big(name: &str, size: i128, max: i128, unit: &str) -> Result<TreeNodeRecursion> {
    plan_err!(
        "{name} would build {size} {unit}, over the {max} this tool allows; use a smaller size"
    )
}

fn check(e: &Expr) -> Result<TreeNodeRecursion> {
    let Expr::ScalarFunction(f) = e else {
        return Ok(TreeNodeRecursion::Continue);
    };
    let name = f.name();
    let arg = |i: usize| f.args.get(i).and_then(constant);
    match name {
        "repeat" => {
            if let (Some(n), Some(text)) = (arg(1), f.args.first()) {
                let size = n.saturating_mul(text_len(text));
                if size > MAX_TEXT_BYTES {
                    return too_big(name, size, MAX_TEXT_BYTES, "bytes");
                }
            }
        }
        "lpad" | "rpad" => {
            if let Some(n) = arg(1)
                && n > MAX_TEXT_BYTES
            {
                return too_big(name, n, MAX_TEXT_BYTES, "characters");
            }
        }
        "array_repeat" => {
            if let Some(n) = arg(1)
                && n > MAX_ELEMENTS
            {
                return too_big(name, n, MAX_ELEMENTS, "elements");
            }
        }
        "range" | "generate_series" => {
            let count = match f.args.len() {
                1 => arg(0),
                2 => arg(0).zip(arg(1)).map(|(a, b)| b.saturating_sub(a)),
                3 => match (arg(0), arg(1), arg(2)) {
                    (Some(a), Some(b), Some(step)) if step != 0 => Some((b - a) / step),
                    _ => None,
                },
                _ => None,
            };
            if let Some(n) = count
                && n > MAX_ELEMENTS
            {
                return too_big(name, n, MAX_ELEMENTS, "elements");
            }
        }
        _ => {}
    }
    Ok(TreeNodeRecursion::Continue)
}

impl AnalyzerRule for BoundedSizes {
    fn name(&self) -> &str {
        "bounded_sizes"
    }

    fn analyze(&self, plan: LogicalPlan, _config: &ConfigOptions) -> Result<LogicalPlan> {
        plan.apply_with_subqueries(|p| {
            p.apply_expressions(|e| e.apply(check))?;
            Ok(TreeNodeRecursion::Continue)
        })?;
        Ok(plan)
    }
}
