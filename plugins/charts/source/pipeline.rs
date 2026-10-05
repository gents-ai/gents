//! From a request to a rendered chart: pick the data, read it within the
//! bounds, draw the chart type, add the accessible text and the PNG.

use std::path::{Path, PathBuf};

use crate::cols::Notes;
use crate::common::Built;
use crate::ctx::Ctx;
use crate::err::{Res, fail};
use crate::load;
use crate::output::{Rendered, Written};
use crate::raster;
use crate::save;
use crate::spec::{Kind, Output, Request, Spec};
use crate::table::{MAX_COLUMNS, MAX_ROWS, Stop, Table};

/// Where the data is.
enum Source {
    Inline(String),
    File(PathBuf),
}

fn inline_text(raw: &serde_json::value::RawValue) -> Res<String> {
    let text = raw.get();
    if text.trim_start().starts_with('"') {
        // A JSON string holding JSON or CSV text, as a graph record carries it.
        return serde_json::from_str::<String>(text).or_else(|_| fail("data is not valid text"));
    }
    Ok(text.to_owned())
}

fn source(req: &Request<'_>) -> Res<Source> {
    match (&req.data, &req.path, &req.file) {
        (Some(_), _, Some(_)) => fail("give either data or a file, not both"),
        (Some(raw), _, None) => Ok(Source::Inline(inline_text(raw)?)),
        (None, None, _) => {
            fail("there is no data; send rows in data, or name a CSV or JSON file in path")
        }
        (None, Some(p), file) => {
            let path = Path::new(p);
            if path.is_dir() {
                match file {
                    Some(f) => Ok(Source::File(load::resolve(path, f)?)),
                    None => fail("path is a folder; name the data file inside it in file"),
                }
            } else if path.exists() {
                if file.is_some() {
                    return fail("path is already a file; leave file out");
                }
                Ok(Source::File(path.to_path_buf()))
            } else {
                fail(format!("{p:?} does not exist; check the path"))
            }
        }
    }
}

fn table_notes(t: &Table, notes: &mut Notes) {
    match t.stopped {
        Some(Stop::Rows) => notes.add(format!("only the first {} rows were read; the data has more (the limit is {MAX_ROWS} rows)", t.rows)),
        Some(Stop::Bytes) => notes.add(format!("only the first {} rows were read; the data has more (the memory budget for one table was reached)", t.rows)),
        None => {}
    }
    if t.ragged > 0 {
        notes.add(format!("{} rows have a different number of fields than the header; missing fields are empty and extra fields are ignored", t.ragged));
    }
    if t.bad_utf8 {
        notes.add("some text was not valid UTF-8; the unreadable bytes were replaced");
    }
    if t.nested > 0 {
        notes.add(format!(
            "{} values were lists or objects and were treated as empty",
            t.nested
        ));
    }
    if t.dropped_columns > 0 {
        notes.add(format!(
            "the data has more than {MAX_COLUMNS} columns; the extra ones were ignored"
        ));
    }
}

fn draw(ctx: &mut Ctx<'_>, t: &Table) -> Res<Built> {
    match ctx.spec.kind {
        Kind::Line | Kind::Area | Kind::StackedArea => crate::line::render(ctx, t),
        Kind::Bar | Kind::Combo => crate::bars::render(ctx, t),
        Kind::Scatter | Kind::Bubble => crate::scatter::render(ctx, t),
        Kind::Histogram => crate::hist::render(ctx, t),
        Kind::Box => crate::boxplot::render(ctx, t),
        Kind::Pie | Kind::Donut => crate::pie::render(ctx, t),
        Kind::Heatmap => crate::heatmap::render(ctx, t),
    }
}

fn chart_title(spec: &Spec) -> String {
    match &spec.title {
        Some(t) => t.clone(),
        None => format!("{} chart", spec.kind.name().replace('_', " ")),
    }
}

/// Renders the chart a request describes. Returns the chart and the files
/// written when the request asked to save.
pub fn render(req: &Request<'_>) -> Res<(Rendered, Vec<Written>)> {
    let spec = req.resolve()?;
    let keep = spec.needed_columns();
    let table = match source(req)? {
        Source::Inline(text) => load::inline(&text, keep)?,
        Source::File(path) => load::file(&path, keep)?,
    };
    if table.rows == 0 {
        return fail("the data has no rows to draw");
    }
    let mut ctx = Ctx::new(&spec);
    table_notes(&table, &mut ctx.notes);
    let built = draw(&mut ctx, &table)?;
    let (svg_text, missing) = built.svg.finish(&chart_title(&spec), &built.alt);
    if !missing.is_empty() {
        let shown: String = missing
            .iter()
            .take(8)
            .collect::<Vec<_>>()
            .iter()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        let more = if missing.len() > 8 {
            format!(" and {} more", missing.len() - 8)
        } else {
            String::new()
        };
        ctx.notes.add(format!(
            "{} characters have no glyph in the built-in font and are drawn as empty boxes: {shown}{more}",
            missing.len()
        ));
    }
    let want_png =
        spec.output != Output::Svg || spec.save.as_ref().is_some_and(|s| s.png.is_some());
    let png = if want_png {
        Some(raster::render(&svg_text, spec.scale)?)
    } else {
        None
    };
    let mut files = Vec::new();
    if let Some(s) = &spec.save {
        let dir = req
            .path
            .as_deref()
            .map(Path::new)
            .ok_or("to save files, give the folder in path")?;
        if let Some(name) = &s.svg {
            save::write(dir, name, svg_text.as_bytes())?;
            files.push(Written {
                path: name.clone(),
                bytes: svg_text.len(),
            });
        }
        if let (Some(name), Some(p)) = (&s.png, &png) {
            save::write(dir, name, &p.bytes)?;
            files.push(Written {
                path: name.clone(),
                bytes: p.bytes.len(),
            });
        }
    }
    let rendered = Rendered {
        chart: spec.kind.name(),
        svg: svg_text,
        png: if spec.output == Output::Svg {
            None
        } else {
            png
        },
        scale: spec.scale,
        width: spec.width,
        height: spec.height,
        alt: built.alt,
        series: built.series,
        warnings: ctx.notes.into_vec(),
        plot: built.plot,
    };
    Ok((rendered, files))
}
