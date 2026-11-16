//! Integer arithmetic that fails instead of wrapping.
//!
//! The engine's `+`, `-`, `*` and `SUM` on integers wrap around silently
//! (`9223372036854775807 + 1` is a negative number). A wrong number is worse
//! than an error, so integer `+`, `-` and `*` are planned as checked calls to
//! Arrow's overflow-detecting kernels, and an integer `SUM` is computed in a
//! 128-bit decimal and cast back, which fails when the total does not fit.
//! Division by zero and out-of-range casts already fail in the engine.
use std::sync::Arc;

use arrow::array::ArrayRef;
use arrow::compute::kernels::numeric::{add, mul, sub};
use arrow::datatypes::DataType;
use datafusion::common::config::ConfigOptions;
use datafusion::common::tree_node::Transformed;
use datafusion::common::{Column, DFSchema, DataFusionError, Result, ScalarValue, plan_err};
use datafusion::logical_expr::expr::AggregateFunction;
use datafusion::logical_expr::planner::{ExprPlanner, PlannerResult, RawBinaryExpr};
use datafusion::logical_expr::type_coercion::binary::binary_numeric_coercion;
use datafusion::logical_expr::{
    Aggregate, ColumnarValue, Expr, ExprSchemable, LogicalPlan, Projection, ScalarFunctionArgs,
    ScalarUDF, ScalarUDFImpl, Signature, Volatility, Window, WindowFunctionDefinition, cast,
};
use datafusion::optimizer::analyzer::AnalyzerRule;
use datafusion::sql::sqlparser::ast::BinaryOperator;

fn is_integer(t: &DataType) -> bool {
    use DataType::*;
    matches!(
        t,
        Int8 | Int16 | Int32 | Int64 | UInt8 | UInt16 | UInt32 | UInt64
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Op {
    Add,
    Sub,
    Mul,
}

impl Op {
    fn symbol(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
        }
    }
}

#[derive(Debug, PartialEq, Eq, Hash)]
struct Checked {
    op: Op,
    signature: Signature,
}

impl Checked {
    fn udf(op: Op) -> Arc<ScalarUDF> {
        Arc::new(ScalarUDF::new_from_impl(Self {
            op,
            signature: Signature::user_defined(Volatility::Immutable),
        }))
    }
}

impl ScalarUDFImpl for Checked {
    fn name(&self) -> &str {
        match self.op {
            Op::Add => "checked_add",
            Op::Sub => "checked_sub",
            Op::Mul => "checked_mul",
        }
    }

    fn signature(&self) -> &Signature {
        &self.signature
    }

    // Named like the operator, so a result column reads `a + b`.
    fn schema_name(&self, args: &[Expr]) -> Result<String> {
        Ok(format!(
            "{} {} {}",
            args[0].schema_name(),
            self.op.symbol(),
            args[1].schema_name()
        ))
    }

    fn display_name(&self, args: &[Expr]) -> Result<String> {
        self.schema_name(args)
    }

    fn coerce_types(&self, arg_types: &[DataType]) -> Result<Vec<DataType>> {
        match arg_types {
            [a, b] => match binary_numeric_coercion(a, b) {
                Some(t) if is_integer(&t) => Ok(vec![t.clone(), t]),
                _ => plan_err!("integer arithmetic needs two integers"),
            },
            _ => plan_err!("integer arithmetic needs two integers"),
        }
    }

    fn return_type(&self, arg_types: &[DataType]) -> Result<DataType> {
        Ok(arg_types[0].clone())
    }

    fn invoke_with_args(&self, args: ScalarFunctionArgs) -> Result<ColumnarValue> {
        let scalar = args
            .args
            .iter()
            .all(|a| matches!(a, ColumnarValue::Scalar(_)));
        let arrays: Vec<ArrayRef> = ColumnarValue::values_to_arrays(&args.args)?;
        let kernel = match self.op {
            Op::Add => add,
            Op::Sub => sub,
            Op::Mul => mul,
        };
        let out = kernel(&arrays[0], &arrays[1]).map_err(DataFusionError::from)?;
        if scalar {
            return Ok(ColumnarValue::Scalar(ScalarValue::try_from_array(&out, 0)?));
        }
        Ok(ColumnarValue::Array(out))
    }
}

/// Plans integer `+`, `-` and `*` as checked calls.
#[derive(Debug)]
pub struct CheckedOps;

impl ExprPlanner for CheckedOps {
    fn plan_binary_op(
        &self,
        expr: RawBinaryExpr,
        schema: &DFSchema,
    ) -> Result<PlannerResult<RawBinaryExpr>> {
        let op = match expr.op {
            BinaryOperator::Plus => Op::Add,
            BinaryOperator::Minus => Op::Sub,
            BinaryOperator::Multiply => Op::Mul,
            _ => return Ok(PlannerResult::Original(expr)),
        };
        let (l, r) = (expr.left.get_type(schema)?, expr.right.get_type(schema)?);
        // Mixed signedness widens to a decimal, which the engine's own checked arithmetic covers.
        if is_integer(&l)
            && is_integer(&r)
            && binary_numeric_coercion(&l, &r).is_some_and(|t| is_integer(&t))
        {
            return Ok(PlannerResult::Planned(
                Checked::udf(op).call(vec![expr.left, expr.right]),
            ));
        }
        Ok(PlannerResult::Original(expr))
    }
}

/// Rewrites an integer `SUM` into a wide decimal sum cast back to the integer type.
#[derive(Debug)]
pub struct CheckedSum;

/// The integer type a `SUM` of `ty` is cast back to, if `ty` is an integer.
fn sum_back(ty: &DataType) -> Option<DataType> {
    use DataType::*;
    match ty {
        UInt8 | UInt16 | UInt32 | UInt64 => Some(UInt64),
        Int8 | Int16 | Int32 | Int64 => Some(Int64),
        _ => None,
    }
}

fn is_sum(f: &str, args: &[Expr]) -> bool {
    f == "sum" && args.len() == 1
}

fn widen(e: Expr, input: &DFSchema) -> Result<Transformed<Expr>> {
    let Expr::AggregateFunction(af) = &e else {
        return Ok(Transformed::no(e));
    };
    if !is_sum(af.func.name(), &af.params.args) {
        return Ok(Transformed::no(e));
    }
    let Some(back) = sum_back(&af.params.args[0].get_type(input)?) else {
        return Ok(Transformed::no(e));
    };
    let name = e.schema_name().to_string();
    let Expr::AggregateFunction(af) = e else {
        unreachable!("matched above")
    };
    let p = af.params;
    let wide = AggregateFunction::new_udf(
        af.func,
        vec![cast(p.args[0].clone(), DataType::Decimal128(20, 0))],
        p.distinct,
        p.filter,
        p.order_by,
        p.null_treatment,
    );
    Ok(Transformed::yes(
        cast(Expr::AggregateFunction(wide), back).alias(name),
    ))
}

/// Widens the integer `SUM` window function `e` (under its alias, if any), returning the new
/// expression and the type to cast its result back to. The result keeps the name of the
/// original so the plan above it still finds its column.
fn widen_window(e: Expr, input: &DFSchema) -> Result<(Expr, Option<DataType>)> {
    let name = e.schema_name().to_string();
    let (inner, alias) = match e {
        Expr::Alias(a) => (*a.expr, Some(a.name)),
        other => (other, None),
    };
    let Expr::WindowFunction(mut w) = inner else {
        return Ok((rewrap(inner, alias), None));
    };
    let is_sum_udf = matches!(&w.fun, WindowFunctionDefinition::AggregateUDF(f)
        if is_sum(f.name(), &w.params.args));
    let back = if is_sum_udf {
        sum_back(&w.params.args[0].get_type(input)?)
    } else {
        None
    };
    let Some(back) = back else {
        return Ok((rewrap(Expr::WindowFunction(w), alias), None));
    };
    w.params.args[0] = cast(w.params.args[0].clone(), DataType::Decimal128(20, 0));
    Ok((
        Expr::WindowFunction(w).alias(alias.unwrap_or(name)),
        Some(back),
    ))
}

fn rewrap(e: Expr, alias: Option<String>) -> Expr {
    match alias {
        Some(n) => e.alias(n),
        None => e,
    }
}

/// A window plan whose integer sums are wide, under a projection that casts them back.
fn widen_window_plan(w: Window) -> Result<Transformed<LogicalPlan>> {
    let input_width = w.input.schema().fields().len();
    let (exprs, backs): (Vec<Expr>, Vec<Option<DataType>>) = w
        .window_expr
        .iter()
        .map(|e| widen_window(e.clone(), w.input.schema()))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .unzip();
    if backs.iter().all(Option::is_none) {
        return Ok(Transformed::no(LogicalPlan::Window(w)));
    }
    let window = Window::try_new(exprs, Arc::clone(&w.input))?;
    let projected: Vec<Expr> = window
        .schema
        .iter()
        .enumerate()
        .map(|(i, (q, f))| {
            let column = Expr::Column(Column::new(q.cloned(), f.name()));
            match i.checked_sub(input_width).and_then(|k| backs[k].clone()) {
                Some(back) => cast(column, back).alias_qualified(q.cloned(), f.name()),
                None => column,
            }
        })
        .collect();
    let plan = LogicalPlan::Window(window);
    Ok(Transformed::yes(LogicalPlan::Projection(
        Projection::try_new(projected, Arc::new(plan))?,
    )))
}

impl AnalyzerRule for CheckedSum {
    fn name(&self) -> &str {
        "checked_integer_sum"
    }

    fn analyze(&self, plan: LogicalPlan, _config: &ConfigOptions) -> Result<LogicalPlan> {
        plan.transform_up_with_subqueries(|p| match p {
            LogicalPlan::Aggregate(a) => {
                let mut changed = false;
                let aggr = a
                    .aggr_expr
                    .iter()
                    .cloned()
                    .map(|e| match e {
                        // The alias a user wrote stays outside the rewritten call.
                        Expr::Alias(alias) => {
                            let t = widen(*alias.expr, a.input.schema())?;
                            changed |= t.transformed;
                            Ok(Expr::Alias(datafusion::logical_expr::expr::Alias {
                                expr: Box::new(t.data),
                                ..alias
                            }))
                        }
                        other => {
                            let t = widen(other, a.input.schema())?;
                            changed |= t.transformed;
                            Ok(t.data)
                        }
                    })
                    .collect::<Result<Vec<_>>>()?;
                if !changed {
                    return Ok(Transformed::no(LogicalPlan::Aggregate(a)));
                }
                let rebuilt = Aggregate::try_new(Arc::clone(&a.input), a.group_expr.clone(), aggr)?;
                Ok(Transformed::yes(LogicalPlan::Aggregate(rebuilt)))
            }
            LogicalPlan::Window(w) => widen_window_plan(w),
            other => Ok(Transformed::no(other)),
        })
        .map(|t| t.data)
    }
}
