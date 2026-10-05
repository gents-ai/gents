//! charts: turns a small chart spec and data into SVG and PNG.
//!
//! One request in, one result out. The data is read in a single bounded pass
//! ([`load`], [`table`]), drawn by one module per chart type on a shared frame
//! ([`frame`], [`axes`]), and delivered as SVG text, a PNG drawn from that same
//! SVG, a text description and warnings ([`pipeline`], [`output`]). Everything
//! is deterministic: fixed decimals, an embedded font, no clock or randomness.
#![deny(missing_docs)]
pub mod axes;
pub mod bars;
#[cfg(test)]
mod bars_tests;
pub mod boxplot;
#[cfg(test)]
mod case_files_tests;
pub mod cli;
pub mod cols;
pub mod common;
pub mod csvio;
pub mod ctx;
pub mod dates;
pub mod det;
#[cfg(test)]
mod dist_tests;
pub mod err;
#[cfg(test)]
mod fixtures_tests;
pub mod format;
pub mod frame;
#[cfg(test)]
mod frame_tests;
#[cfg(test)]
mod fuzz_tests;
pub mod graph;
pub mod heatmap;
pub mod hist;
pub mod jsonio;
pub mod line;
#[cfg(test)]
mod line_tests;
pub mod load;
pub mod marks;
pub mod num;
pub mod output;
pub mod palette;
pub mod pie;
#[cfg(test)]
mod pie_heat_tests;
pub mod pipeline;
#[cfg(test)]
mod pipeline_tests;
#[cfg(test)]
mod pixel_tests;
#[cfg(test)]
mod proptests;
pub mod raster;
pub mod reduce;
pub mod save;
pub mod scale;
pub mod scatter;
#[cfg(test)]
mod scatter_tests;
pub mod shape;
pub mod spec;
pub mod stats;
pub mod svg;
pub mod table;
#[cfg(test)]
mod testkit;
#[cfg(test)]
mod testutil;
pub mod text;
#[cfg(test)]
mod text_tests;

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
        return format!(
            "the field {name:?} is not known; check the field names in the tool description"
        );
    }
    if e.is_eof() {
        return "the request ends early; it looks truncated".into();
    }
    format!("the request is not valid: {msg}")
}
