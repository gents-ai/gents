//! The SQL engine: Apache DataFusion running single-threaded over this
//! plugin's own table scans, inside a fixed memory pool and with no disk.
//!
//! Only queries are accepted. A query that has no `ORDER BY` still returns its
//! rows in a documented, repeatable order: a plain scan (filters, projections
//! and `LIMIT` over one table) keeps the file's row order, and any other shape
//! (aggregates, joins, `DISTINCT`, windows, set operations) is sorted by all
//! its output columns, ascending, NULLs last, so a paged read of it is stable.
use std::ops::ControlFlow;
use std::sync::Arc;

use arrow::array::{Array, ArrayRef, RecordBatch};
use arrow::compute::cast;
use arrow::datatypes::{DataType, SchemaRef};
use async_trait::async_trait;
use datafusion::catalog::{CatalogProvider, MemoryCatalogProvider, SchemaProvider, Session};
use datafusion::common::{Column, DataFusionError};
use datafusion::datasource::{TableProvider, TableType};
use datafusion::execution::SessionStateBuilder;
use datafusion::execution::disk_manager::{DiskManagerBuilder, DiskManagerMode};
use datafusion::execution::memory_pool::GreedyMemoryPool;
use datafusion::execution::runtime_env::RuntimeEnvBuilder;
use datafusion::execution::{SendableRecordBatchStream, TaskContext};
use datafusion::logical_expr::registry::FunctionRegistry;
use datafusion::logical_expr::{Expr, LogicalPlan, LogicalPlanBuilder, SortExpr};
use datafusion::physical_plan::ExecutionPlan;
use datafusion::physical_plan::stream::RecordBatchStreamAdapter;
use datafusion::physical_plan::streaming::{PartitionStream, StreamingTableExec};
use datafusion::prelude::{SessionConfig, SessionContext};
use datafusion::sql::parser::Statement;
use datafusion::sql::sqlparser::ast::{Expr as SqlExpr, Statement as Ast, Visit, Visitor};

use crate::Res;
use crate::catalog::Catalog;
use crate::checked::{CheckedOps, CheckedSum};
use crate::table::{BATCH_ROWS, ScanError, TableSource};

/// The longest SQL text accepted.
pub const MAX_SQL_BYTES: usize = 64 * 1024;
/// The deepest an expression may nest (a chain of 300 additions is 300 deep).
pub const MAX_EXPR_DEPTH: usize = 256;

/// How the rows of a result are ordered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    /// The query has its own `ORDER BY`.
    Query,
    /// A plain scan: the file's row order.
    File,
    /// Sorted by every output column, ascending, NULLs last.
    Columns,
    /// No sortable output column, so no order is guaranteed.
    None,
}

/// A query ready to run.
pub struct Prepared {
    /// The plan, with the ordering rule applied.
    pub plan: LogicalPlan,
    /// Which ordering rule applies.
    pub order: Order,
}

/// DataFusion over a [`Catalog`].
pub struct Engine {
    ctx: SessionContext,
}

impl Engine {
    /// An engine whose queries may hold at most `pool_bytes` in their operators.
    pub fn new(catalog: Arc<Catalog>, pool_bytes: usize) -> Res<Self> {
        let runtime = RuntimeEnvBuilder::new()
            .with_memory_pool(Arc::new(GreedyMemoryPool::new(pool_bytes)))
            .with_disk_manager_builder(
                DiskManagerBuilder::default().with_mode(DiskManagerMode::Disabled),
            )
            .build_arc()
            .map_err(|e| format!("the SQL engine could not start: {e}"))?;
        let mut config = SessionConfig::new()
            .with_target_partitions(1)
            .with_batch_size(BATCH_ROWS)
            .with_information_schema(false)
            .with_collect_statistics(false);
        config.options_mut().sql_parser.enable_ident_normalization = false;
        config.options_mut().sql_parser.recursion_limit =
            datafusion::common::config::ConfigNonZeroUsize::try_new(MAX_EXPR_DEPTH)
                .map_err(|e| format!("the SQL engine could not start: {e}"))?;
        let state = SessionStateBuilder::new()
            .with_config(config)
            .with_runtime_env(runtime)
            .with_default_features()
            .with_analyzer_rule(Arc::new(CheckedSum))
            .build();
        let mut ctx = SessionContext::new_with_state(state);
        ctx.register_expr_planner(Arc::new(CheckedOps))
            .map_err(|e| format!("the SQL engine could not start: {e}"))?;
        let provider = Arc::new(MemoryCatalogProvider::new());
        provider
            .register_schema("public", Arc::new(Schema { catalog }))
            .map_err(|e| format!("the SQL engine could not start: {e}"))?;
        ctx.register_catalog("datafusion", provider);
        Ok(Self { ctx })
    }

    /// Plans `sql`, which must be one query, and applies the ordering rule.
    pub async fn prepare(&self, sql: &str) -> Res<Prepared> {
        if sql.len() > MAX_SQL_BYTES {
            return Err(format!(
                "the SQL is over {} KiB; shorten it or move literal data into a table",
                MAX_SQL_BYTES / 1024
            ));
        }
        let state = self.ctx.state();
        let dialect = state.config().options().sql_parser.dialect;
        let statement = state
            .sql_to_statement(sql, &dialect)
            .map_err(|e| explain(&e))?;
        check_select(&statement)?;
        let plan = state
            .statement_to_plan(statement)
            .await
            .map_err(|e| explain(&e))?;
        order(plan)
    }

    /// Starts executing a prepared query.
    pub async fn stream(&self, p: Prepared) -> Result<SendableRecordBatchStream, DataFusionError> {
        self.ctx
            .execute_logical_plan(p.plan)
            .await?
            .execute_stream()
            .await
    }
}

/// Measures how deep expressions nest, stopping as soon as one is too deep.
struct Depth {
    now: usize,
    max: usize,
}

impl Visitor for Depth {
    type Break = ();

    fn pre_visit_expr(&mut self, _: &SqlExpr) -> ControlFlow<()> {
        self.now += 1;
        self.max = self.max.max(self.now);
        if self.max > MAX_EXPR_DEPTH {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    }

    fn post_visit_expr(&mut self, _: &SqlExpr) -> ControlFlow<()> {
        self.now -= 1;
        ControlFlow::Continue(())
    }
}

/// Accepts a plain query, or an EXPLAIN of one; anything else (even wrapped in an EXPLAIN,
/// which would run it) is refused.
fn check_select(statement: &Statement) -> Res<()> {
    match statement {
        Statement::Statement(s) if matches!(**s, Ast::Query(_)) => check_depth(s),
        Statement::Explain(e) => check_select(&e.statement),
        _ => Err(
            "only SELECT queries are accepted; to save a result as a file use the export mode"
                .into(),
        ),
    }
}

fn depth_message() -> String {
    format!(
        "an expression nests more than {MAX_EXPR_DEPTH} levels deep; split it, or use IN (...) for a long list of OR conditions"
    )
}

fn check_depth(statement: &Ast) -> Res<()> {
    let mut depth = Depth { now: 0, max: 0 };
    match statement.visit(&mut depth) {
        ControlFlow::Break(()) => Err(depth_message()),
        ControlFlow::Continue(()) => Ok(()),
    }
}

fn sortable(t: &DataType) -> bool {
    use DataType::*;
    matches!(
        t,
        Boolean
            | Int8
            | Int16
            | Int32
            | Int64
            | UInt8
            | UInt16
            | UInt32
            | UInt64
            | Float32
            | Float64
            | Utf8
            | LargeUtf8
            | Utf8View
            | Binary
            | LargeBinary
            | BinaryView
            | Date32
            | Date64
            | Time32(_)
            | Time64(_)
            | Timestamp(..)
            | Duration(_)
            | Decimal128(..)
            | Decimal256(..)
    )
}

fn user_sorted(p: &LogicalPlan) -> bool {
    match p {
        LogicalPlan::Sort(_) => true,
        LogicalPlan::Projection(x) => user_sorted(&x.input),
        LogicalPlan::SubqueryAlias(x) => user_sorted(&x.input),
        LogicalPlan::Limit(x) => user_sorted(&x.input),
        _ => false,
    }
}

fn file_order(p: &LogicalPlan) -> bool {
    matches!(
        p,
        LogicalPlan::TableScan(_)
            | LogicalPlan::Projection(_)
            | LogicalPlan::Filter(_)
            | LogicalPlan::Limit(_)
            | LogicalPlan::SubqueryAlias(_)
            | LogicalPlan::EmptyRelation(_)
            | LogicalPlan::Values(_)
    ) && p.inputs().into_iter().all(file_order)
}

fn order(plan: LogicalPlan) -> Res<Prepared> {
    // An EXPLAIN is a plan description, not rows to page, and must stay the root of the plan.
    if matches!(plan, LogicalPlan::Explain(_) | LogicalPlan::Analyze(_)) {
        return Ok(Prepared {
            plan,
            order: Order::File,
        });
    }
    if user_sorted(&plan) {
        return Ok(Prepared {
            plan,
            order: Order::Query,
        });
    }
    if file_order(&plan) {
        return Ok(Prepared {
            plan,
            order: Order::File,
        });
    }
    let mut seen = std::collections::HashSet::new();
    let keys: Vec<SortExpr> = plan
        .schema()
        .iter()
        .filter(|(_, f)| sortable(f.data_type()))
        .filter(|(q, f)| seen.insert((q.map(ToString::to_string), f.name().clone())))
        .map(|(q, f)| Expr::Column(Column::new(q.cloned(), f.name())).sort(true, false))
        .collect();
    if keys.is_empty() {
        return Ok(Prepared {
            plan,
            order: Order::None,
        });
    }
    let plan = LogicalPlanBuilder::from(plan)
        .sort(keys)
        .and_then(LogicalPlanBuilder::build)
        .map_err(|e| explain(&e))?;
    Ok(Prepared {
        plan,
        order: Order::Columns,
    })
}

/// The scan error inside `e`, if one caused it.
pub fn scan_error(e: &DataFusionError) -> Option<&ScanError> {
    match e {
        DataFusionError::External(b) => b.downcast_ref::<ScanError>(),
        DataFusionError::Shared(inner) => scan_error(inner),
        DataFusionError::Context(_, inner) | DataFusionError::Diagnostic(_, inner) => {
            scan_error(inner)
        }
        DataFusionError::ArrowError(arrow, _) => match &**arrow {
            arrow::error::ArrowError::ExternalError(b) => b.downcast_ref::<ScanError>(),
            _ => None,
        },
        _ => None,
    }
}

/// One plain sentence for an engine error.
pub fn explain(e: &DataFusionError) -> String {
    crate::table::scrub(&explain_raw(e))
}

fn explain_raw(e: &DataFusionError) -> String {
    if let Some(s) = scan_error(e) {
        return s.to_string();
    }
    let root = {
        let mut cur = e;
        loop {
            match cur {
                DataFusionError::Shared(i) => cur = i,
                DataFusionError::Context(_, i) | DataFusionError::Diagnostic(_, i) => cur = i,
                _ => break cur,
            }
        }
    };
    if matches!(root, DataFusionError::ResourcesExhausted(_)) {
        return "the query needs more memory than this tool may use; narrow it with WHERE or LIMIT, select fewer columns, or aggregate before joining".into();
    }
    let text = root.to_string();
    if text.contains("RecursionLimitExceeded") {
        return depth_message();
    }
    let first = text.lines().next().unwrap_or_default();
    let first = first.replace("datafusion.public.", "");
    let first = first
        .trim_start_matches("Compute error: ")
        .trim_start_matches("Error during planning: ")
        .trim_start_matches("Execution error: ")
        .trim_start_matches("SQL error: ")
        .trim_start_matches("Arrow error: ")
        .trim_start_matches("Schema error: ")
        .trim_start_matches("External error: ")
        .trim_start_matches("Optimizer rule ");
    let lower = first.to_ascii_lowercase();
    let note = if lower.contains("divide by zero") || lower.contains("division by zero") {
        " (guard the divisor with NULLIF)"
    } else if lower.contains("overflow") || lower.contains("out of range") {
        " (cast the values to a wider type, such as DECIMAL(38,0), first)"
    } else {
        ""
    };
    format!("the query failed: {}{note}", first.trim_end_matches('.'))
}

/// The columns of `b` with view types turned into plain ones, which everything downstream reads.
pub fn plain(b: &RecordBatch) -> Result<RecordBatch, arrow::error::ArrowError> {
    fn fix(a: &ArrayRef) -> Result<ArrayRef, arrow::error::ArrowError> {
        match a.data_type() {
            DataType::Utf8View => cast(a, &DataType::Utf8),
            DataType::BinaryView => cast(a, &DataType::Binary),
            _ => Ok(Arc::clone(a)),
        }
    }
    if !b
        .schema()
        .fields()
        .iter()
        .any(|f| matches!(f.data_type(), DataType::Utf8View | DataType::BinaryView))
    {
        return Ok(b.clone());
    }
    let cols = b.columns().iter().map(fix).collect::<Result<Vec<_>, _>>()?;
    let fields: Vec<_> = b
        .schema()
        .fields()
        .iter()
        .zip(&cols)
        .map(|(f, c)| Arc::new(f.as_ref().clone().with_data_type(c.data_type().clone())))
        .collect();
    let schema = Arc::new(arrow::datatypes::Schema::new(fields));
    RecordBatch::try_new_with_options(
        schema,
        cols,
        &arrow::record_batch::RecordBatchOptions::new().with_row_count(Some(b.num_rows())),
    )
}

#[derive(Debug)]
struct Schema {
    catalog: Arc<Catalog>,
}

#[async_trait]
impl SchemaProvider for Schema {
    fn table_names(&self) -> Vec<String> {
        self.catalog
            .specs()
            .iter()
            .map(|s| s.name.clone())
            .collect()
    }

    async fn table(&self, name: &str) -> Result<Option<Arc<dyn TableProvider>>, DataFusionError> {
        if self.catalog.spec(name).is_none() {
            return Ok(None);
        }
        let source = self
            .catalog
            .open(name)
            .map_err(|e| DataFusionError::External(Box::new(ScanError::Failed(e))))?;
        Ok(Some(Arc::new(Scan { source })))
    }

    fn table_exist(&self, name: &str) -> bool {
        self.catalog.spec(name).is_some()
    }
}

impl std::fmt::Debug for Catalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Catalog").finish_non_exhaustive()
    }
}

struct Scan {
    source: Arc<dyn TableSource>,
}

impl std::fmt::Debug for Scan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scan").finish_non_exhaustive()
    }
}

#[async_trait]
impl TableProvider for Scan {
    fn schema(&self) -> SchemaRef {
        self.source.schema()
    }

    fn table_type(&self) -> TableType {
        TableType::Base
    }

    async fn scan(
        &self,
        _state: &dyn Session,
        projection: Option<&Vec<usize>>,
        _filters: &[Expr],
        limit: Option<usize>,
    ) -> datafusion::common::Result<Arc<dyn ExecutionPlan>> {
        let full = self.source.schema();
        let schema = match projection {
            Some(p) => Arc::new(full.project(p)?),
            None => full,
        };
        let part = Arc::new(Part::new(
            Arc::clone(&schema),
            Arc::clone(&self.source),
            projection,
        ));
        Ok(Arc::new(StreamingTableExec::try_new(
            schema,
            vec![part],
            None,
            Vec::new(),
            false,
            limit,
        )?))
    }
}

/// One scan of a table. Sources read an ascending, unique list of columns; the order and
/// repeats the plan asked for are restored here.
struct Part {
    schema: SchemaRef,
    source: Arc<dyn TableSource>,
    read: Option<Vec<usize>>,
    /// For each output column, its place among the columns read.
    pick: Option<Vec<usize>>,
}

impl Part {
    fn new(
        schema: SchemaRef,
        source: Arc<dyn TableSource>,
        projection: Option<&Vec<usize>>,
    ) -> Self {
        let Some(p) = projection else {
            return Self {
                schema,
                source,
                read: None,
                pick: None,
            };
        };
        let mut read = p.clone();
        read.sort_unstable();
        read.dedup();
        let pick = p
            .iter()
            .map(|c| read.binary_search(c).unwrap_or(0))
            .collect();
        Self {
            schema,
            source,
            read: Some(read),
            pick: Some(pick),
        }
    }
}

impl std::fmt::Debug for Part {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Part").finish_non_exhaustive()
    }
}

impl PartitionStream for Part {
    fn schema(&self) -> &SchemaRef {
        &self.schema
    }

    fn execute(&self, _ctx: Arc<TaskContext>) -> SendableRecordBatchStream {
        let schema = Arc::clone(&self.schema);
        let batches = self.source.scan(self.read.as_deref());
        let typed = Arc::clone(&schema);
        let pick = self.pick.clone();
        let stream = futures::stream::iter(batches.map(move |r| match r {
            Ok(b) => {
                let columns = match &pick {
                    Some(pick) => pick.iter().map(|&i| Arc::clone(b.column(i))).collect(),
                    None => b.columns().to_vec(),
                };
                RecordBatch::try_new_with_options(
                    Arc::clone(&typed),
                    columns,
                    &arrow::record_batch::RecordBatchOptions::new()
                        .with_row_count(Some(b.num_rows())),
                )
                .map_err(DataFusionError::from)
            }
            Err(e) => Err(DataFusionError::External(Box::new(e))),
        }));
        Box::pin(RecordBatchStreamAdapter::new(schema, stream))
    }
}
