//! data_tables plugin: SQL over data files with bounded memory. One JSON value
//! on stdin, one on stdout; a failure is one sentence on stderr and a non-zero
//! exit (see TOOL.md).
use std::io::{Read, Write};
use std::sync::Arc;

mod catalog;
mod checked;
mod csv;
mod csvparse;
mod cursor;
mod engine;
mod export;
mod graph;
mod inline;
mod input;
mod ipc;
mod json;
mod names;
mod ods;
mod parquet_src;
mod query;
mod render;
mod sheet;
mod table;
mod tables;
#[cfg(test)]
mod testkit;
mod typed;
mod xlsx;
mod zipread;

use catalog::Catalog;
use input::{Input, Mode};

/// The result of a step: a value, or one plain sentence.
pub type Res<T> = Result<T, String>;

fn main() {
    let mut raw = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut raw) {
        fail(&format!("reading stdin: {e}"));
    }
    match run(&raw) {
        Ok(out) => {
            let mut stdout = std::io::stdout().lock();
            if let Err(e) = stdout
                .write_all(out.as_bytes())
                .and_then(|()| stdout.write_all(b"\n"))
            {
                fail(&format!("writing the result: {e}"));
            }
        }
        Err(e) => fail(&e),
    }
}

fn fail(message: &str) -> ! {
    eprintln!("data_tables: {message}");
    std::process::exit(1);
}

/// Runs one request and returns the JSON text of its result.
pub fn run(raw: &str) -> Res<String> {
    if let Some(out) = graph::run(raw) {
        return out;
    }
    let input: Input = serde_json::from_str(raw).map_err(|e| format!("invalid input: {e}"))?;
    serde_json::to_string(&call(&input)?).map_err(|e| format!("serializing the result: {e}"))
}

/// Runs a checked input.
pub fn call(input: &Input) -> Res<serde_json::Value> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(|e| format!("the SQL engine could not start: {e}"))?;
    runtime.block_on(call_async(input))
}

async fn call_async(input: &Input) -> Res<serde_json::Value> {
    let catalog = Arc::new(Catalog::discover(
        input.path.as_deref(),
        input.files.as_deref(),
        input.tables.as_ref(),
        input.csv_options()?,
    )?);
    match input.mode() {
        Mode::Tables => tables::list(input, &catalog),
        Mode::Describe => tables::describe(input, &catalog).await,
        Mode::Query => query::query(input, &catalog).await,
        Mode::Export => export::export(input, &catalog).await,
    }
}
