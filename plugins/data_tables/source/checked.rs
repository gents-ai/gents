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
use datafusion::common::{DFSchema, DataFusionError, Result, ScalarValue, plan_err};
use datafusion::logical_expr::expr::AggregateFunction;
use datafusion::logical_expr::planner::{ExprPlanner, PlannerResult, RawBinaryExpr};
use datafusion::logical_expr::type_coercion::binary::binary_numeric_coercion;
use datafusion::logical_expr::{
    Aggregate, ColumnarValue, Expr, ExprSchemable, LogicalPlan, ScalarFunctionArgs, ScalarUDF,
    ScalarUDFImpl, Signature, Volatility, cast,
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
        if is_integer(&l) && is_integer(&r) {
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

fn widen(e: Expr, input: &DFSchema) -> Result<Transformed<Expr>> {
    let Expr::AggregateFunction(af) = &e else {
        return Ok(Transformed::no(e));
    };
    if af.func.name() != "sum" || af.params.args.len() != 1 {
        return Ok(Transformed::no(e));
    }
    let ty = af.params.args[0].get_type(input)?;
    if !is_integer(&ty) {
        return Ok(Transformed::no(e));
    }
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
    let back = if matches!(
        ty,
        DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64
    ) {
        DataType::UInt64
    } else {
        DataType::Int64
    };
    Ok(Transformed::yes(
        cast(Expr::AggregateFunction(wide), back).alias(name),
    ))
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
            other => Ok(Transformed::no(other)),
        })
        .map(|t| t.data)
    }
}
