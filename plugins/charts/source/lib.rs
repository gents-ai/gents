//! charts: turns a small chart spec and data into SVG and PNG.
pub mod axes;
pub mod bars;
pub mod boxplot;
pub mod cli;
pub mod cols;
pub mod common;
pub mod csvio;
pub mod ctx;
pub mod dates;
pub mod det;
pub mod err;
pub mod format;
pub mod frame;
pub mod graph;
pub mod heatmap;
pub mod hist;
pub mod jsonio;
pub mod line;
pub mod load;
pub mod marks;
pub mod num;
pub mod output;
pub mod palette;
pub mod pie;
pub mod pipeline;
pub mod raster;
pub mod reduce;
pub mod save;
pub mod scale;
pub mod scatter;
pub mod shape;
pub mod spec;
pub mod stats;
pub mod svg;
pub mod table;
pub mod text;
#[cfg(test)]
mod testutil;

use spec::Request;

/// Runs one request given as JSON text and returns the result JSON.
pub fn run(raw: &str) -> err::Res<String> {
    if graph::is_node(raw) {
        return Ok(graph::run(raw));
    }
    let req: Request = serde_json::from_str(raw).map_err(|e| err::ChartError(request_error(&e)))?;
    let (rendered, files) = pipeline::render(&req)?;
    let output = req.resolve()?.output;
    output::deliver(rendered, output, &files)
}

pub(crate) fn request_error(e: &serde_json::Error) -> String {
    let msg = e.to_string();
    let msg = msg.split(" at line ").next().unwrap_or(&msg);
    if let Some(rest) = msg.strip_prefix("unknown field `") {
        let name = rest.split('`').next().unwrap_or(rest);
        return format!("the field {name:?} is not known; check the field names in the tool description");
    }
    if e.is_eof() {
        return "the request ends early; it looks truncated".into();
    }
    format!("the request is not valid: {msg}")
}
