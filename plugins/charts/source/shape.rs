//! Turns a table and a spec into what the drawing code needs: the x values
//! (numbers, dates or categories), and one group of rows per series. Series
//! come from several value columns (wide data) or from a column that names
//! the series (long data). Anything unusable is counted and reported.

use std::collections::HashMap;

use crate::cols::{self, ColKind, Numeric};
use crate::ctx::Ctx;
use crate::err::{Res, fail};
use crate::spec::{Agg, MAX_CATEGORIES, MAX_SERIES, ScaleKind, Sort};
use crate::table::Table;

/// What the x column holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XKind {
    /// Numbers.
    Num,
    /// Dates, as epoch seconds.
    Time,
    /// Labels.
    Cat,
}

/// How an x column may be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XPolicy {
    /// Numbers, dates or labels, from the data.
    Auto,
    /// Always labels.
    Cat,
    /// Numbers or dates only.
    Cont,
}

/// The x values of every row.
#[derive(Debug, Clone)]
pub struct XData {
    /// Kind.
    pub kind: XKind,
    /// Column name.
    pub name: String,
    /// Per row: the number, the epoch seconds or the category index; `NaN`
    /// for a row without a usable x.
    pub v: Vec<f64>,
    /// Category labels in drawing order (empty unless `kind` is `Cat`).
    pub cats: Vec<String>,
    /// True when every x is a whole number from 1000 to 2999, which reads as
    /// a year and is written without a thousands separator.
    pub years: bool,
}

/// The rows of one series.
#[derive(Debug, Clone)]
pub struct Group {
    /// Series name.
    pub name: String,
    /// Value column index.
    pub col: usize,
    /// Rows of the series; `None` means every row.
    pub rows: Option<Vec<u32>>,
}

/// Columns resolved against a table.
#[derive(Debug)]
pub struct Resolved {
    /// The x values.
    pub x: XData,
    /// The series.
    pub groups: Vec<Group>,
    /// The name of the value column, for axis titles.
    pub y_name: String,
    nums: HashMap<usize, Numeric>,
}

fn x_column(ctx: &Ctx<'_>, t: &Table) -> Res<usize> {
    match &ctx.spec.x {
        Some(n) => cols::column(t, n),
        None if t.names.is_empty() => fail("the data has no columns"),
        None => Ok(0),
    }
}

/// Columns to use as values: the named ones, else every numeric column not
/// used for something else.
pub fn value_columns(
    ctx: &mut Ctx<'_>,
    t: &Table,
    named: &[String],
    skip: &[usize],
) -> Res<Vec<usize>> {
    if !named.is_empty() {
        return named.iter().map(|n| cols::column(t, n)).collect();
    }
    let mut found: Vec<usize> = (0..t.names.len())
        .filter(|c| !skip.contains(c) && cols::kind(t, *c) == ColKind::Numeric)
        .collect();
    if found.is_empty() {
        return fail("no column of numbers to draw; name the value column in y");
    }
    if found.len() > MAX_SERIES {
        ctx.notes.add(format!(
            "the data has {} numeric columns; only the first {MAX_SERIES} are drawn",
            found.len()
        ));
        found.truncate(MAX_SERIES);
    }
    Ok(found)
}

/// Resolves the x column, the series and the value columns of a chart.
pub fn resolve(
    ctx: &mut Ctx<'_>,
    t: &Table,
    policy: XPolicy,
    named_y: &[String],
    extra_skip: &[usize],
) -> Res<Resolved> {
    let xc = x_column(ctx, t)?;
    let series_col = ctx
        .spec
        .series
        .as_deref()
        .map(|n| cols::column(t, n))
        .transpose()?;
    let mut skip = vec![xc];
    skip.extend(series_col);
    skip.extend(extra_skip);
    let ycols = value_columns(ctx, t, named_y, &skip)?;
    let x = build_x(ctx, t, xc, policy)?;

    let mut groups = Vec::new();
    let y_name;
    match series_col {
        Some(sc) => {
            if ycols.len() != 1 {
                return fail("with series, give exactly one value column in y");
            }
            let yc = ycols[0];
            y_name = t.names[yc].clone();
            let labels = cols::labels(t, sc);
            let (names, index) = cols::distinct(&labels);
            let mut per: Vec<Vec<u32>> = vec![Vec::new(); names.len()];
            let mut unlabeled = 0usize;
            for (row, g) in index.iter().enumerate() {
                match g {
                    Some(g) => per[*g].push(row as u32),
                    None => unlabeled += 1,
                }
            }
            if unlabeled > 0 {
                ctx.notes.add(format!(
                    "{unlabeled} rows have no value in column {:?} and are not drawn",
                    t.names[sc]
                ));
            }
            if names.len() > MAX_SERIES {
                ctx.notes.add(format!(
                    "column {:?} has {} values; only the first {MAX_SERIES} series are drawn",
                    t.names[sc],
                    names.len()
                ));
            }
            for (name, rows) in names.into_iter().zip(per).take(MAX_SERIES) {
                groups.push(Group {
                    name,
                    col: yc,
                    rows: Some(rows),
                });
            }
        }
        None => {
            y_name = if ycols.len() == 1 {
                t.names[ycols[0]].clone()
            } else {
                String::new()
            };
            for yc in ycols {
                groups.push(Group {
                    name: t.names[yc].clone(),
                    col: yc,
                    rows: None,
                });
            }
        }
    }
    let mut r = Resolved {
        x,
        groups,
        y_name,
        nums: HashMap::new(),
    };
    let cols_used: Vec<usize> = r.groups.iter().map(|g| g.col).collect();
    for c in cols_used {
        r.ensure_numeric(ctx, t, c);
    }
    if r.x.kind == XKind::Cat {
        r.order_categories(ctx, t);
    }
    Ok(r)
}

fn build_x(ctx: &mut Ctx<'_>, t: &Table, xc: usize, policy: XPolicy) -> Res<XData> {
    let name = t.names[xc].clone();
    let detected = cols::kind(t, xc);
    if detected == ColKind::Empty {
        return fail(format!(
            "column {name:?} has no values; name a column that holds the x values"
        ));
    }
    let scale = ctx.spec.x_scale;
    let kind = match (policy, scale) {
        (XPolicy::Cat, s) => {
            if !matches!(s, ScaleKind::Auto | ScaleKind::Category) {
                ctx.notes
                    .add("x_scale is ignored for this chart type: its x axis lists categories");
            }
            XKind::Cat
        }
        (_, ScaleKind::Category) => XKind::Cat,
        (_, ScaleKind::Time) => XKind::Time,
        (_, ScaleKind::Linear | ScaleKind::Log) => XKind::Num,
        (_, ScaleKind::Auto) => match detected {
            ColKind::Numeric => XKind::Num,
            ColKind::Time => XKind::Time,
            _ => XKind::Cat,
        },
    };
    if policy == XPolicy::Cont && kind == XKind::Cat {
        return fail(format!(
            "this chart needs numbers or dates on x, but column {name:?} holds text; use a bar chart for categories"
        ));
    }
    match kind {
        XKind::Num => {
            if detected == ColKind::Time || detected == ColKind::Text {
                return fail(format!(
                    "x_scale needs numbers but column {name:?} holds {}",
                    if detected == ColKind::Time {
                        "dates"
                    } else {
                        "text"
                    }
                ));
            }
            let n = cols::numeric(t, xc);
            let bad = n.nulls + n.text;
            if bad > 0 {
                ctx.notes.add(format!("column {name:?}: {bad} values are empty or not numbers; those rows are not drawn"));
            }
            let years = crate::common::years_like(&n.v);
            Ok(XData {
                kind,
                name,
                v: n.v,
                cats: Vec::new(),
                years,
            })
        }
        XKind::Time => match cols::times(t, xc) {
            Some(v) => {
                let bad = v.iter().filter(|x| x.is_nan()).count();
                if bad > 0 {
                    ctx.notes.add(format!(
                        "column {name:?}: {bad} values are empty; those rows are not drawn"
                    ));
                }
                Ok(XData {
                    kind,
                    name,
                    v,
                    cats: Vec::new(),
                    years: false,
                })
            }
            None => fail(format!(
                "x_scale is time but column {name:?} does not hold dates; use dates like 2024-01-31 or 2024-01-31T10:00:00Z"
            )),
        },
        XKind::Cat => {
            let labels = cols::labels(t, xc);
            let (cats, idx) = cols::distinct(&labels);
            let missing = idx.iter().filter(|i| i.is_none()).count();
            if missing > 0 {
                ctx.notes.add(format!(
                    "column {name:?}: {missing} rows have no value and are not drawn"
                ));
            }
            let v = idx
                .iter()
                .map(|i| i.map_or(f64::NAN, |i| i as f64))
                .collect();
            Ok(XData {
                kind,
                name,
                v,
                cats,
                years: false,
            })
        }
    }
}

impl Resolved {
    fn ensure_numeric(&mut self, ctx: &mut Ctx<'_>, t: &Table, col: usize) {
        if self.nums.contains_key(&col) {
            return;
        }
        let n = cols::numeric(t, col);
        cols::report_numeric(&mut ctx.notes, &t.names[col], &n);
        self.nums.insert(col, n);
    }

    /// The numbers of a value column.
    pub fn numbers(&self, col: usize) -> &[f64] {
        &self.nums[&col].v
    }

    /// Rows of `g` in table order.
    pub fn rows<'a>(&'a self, g: &'a Group, total: usize) -> Box<dyn Iterator<Item = usize> + 'a> {
        match &g.rows {
            Some(r) => Box::new(r.iter().map(|r| *r as usize)),
            None => Box::new(0..total),
        }
    }

    /// The points of `g`: rows with a usable x, as (x, y) with `NaN` for a gap.
    pub fn points(&self, g: &Group, total: usize) -> Vec<(f64, f64)> {
        let y = self.numbers(g.col);
        self.rows(g, total)
            .filter(|r| !self.x.v[*r].is_nan())
            .map(|r| (self.x.v[r], y[r]))
            .collect()
    }

    /// Orders and caps the categories as the spec says, renumbering `x.v`.
    fn order_categories(&mut self, ctx: &mut Ctx<'_>, t: &Table) {
        let n = self.x.cats.len();
        let mut totals = vec![0.0_f64; n];
        let mut seen = vec![false; n];
        for g in &self.groups {
            let y = &self.nums[&g.col].v;
            for r in self.rows(g, t.rows) {
                let c = self.x.v[r];
                if !c.is_nan() && !y[r].is_nan() {
                    totals[c as usize] += y[r];
                    seen[c as usize] = true;
                }
            }
        }
        for (i, s) in seen.iter().enumerate() {
            if !s {
                totals[i] = f64::NAN;
            }
        }
        let order = cols::order(&self.x.cats, &totals, ctx.spec.sort);
        let keep = order.len().min(MAX_CATEGORIES);
        if order.len() > keep {
            ctx.notes.add(format!(
                "there are {} categories; only the first {keep} {} are drawn",
                order.len(),
                if ctx.spec.sort == Sort::None {
                    "in data order"
                } else {
                    "in the chosen order"
                }
            ));
        }
        let mut new_index = vec![f64::NAN; n];
        for (pos, old) in order.iter().take(keep).enumerate() {
            new_index[*old] = pos as f64;
        }
        for v in &mut self.x.v {
            if !v.is_nan() {
                *v = new_index[*v as usize];
            }
        }
        self.x.cats = order
            .iter()
            .take(keep)
            .map(|i| self.x.cats[*i].clone())
            .collect();
    }

    /// For category charts: the aggregate of each series at each category,
    /// `NaN` where the series has no number. A count counts the numbers.
    pub fn grid(&self, t: &Table, agg: Agg) -> Vec<Vec<f64>> {
        let ncat = self.x.cats.len();
        self.groups
            .iter()
            .map(|g| {
                let y = self.numbers(g.col);
                let mut bucket: Vec<Vec<f64>> = vec![Vec::new(); ncat];
                for r in self.rows(g, t.rows) {
                    let c = self.x.v[r];
                    if c.is_nan() {
                        continue;
                    }
                    if !y[r].is_nan() {
                        bucket[c as usize].push(y[r]);
                    }
                }
                bucket.iter().map(|b| cols::aggregate(b, agg)).collect()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::Request;
    use crate::table::Builder;

    fn table(csv: &str) -> Table {
        let mut b = Builder::new(None);
        crate::csvio::read(csv.as_bytes(), &mut b).unwrap();
        b.finish()
    }

    fn spec(json: &str) -> crate::spec::Spec {
        serde_json::from_str::<Request>(json)
            .unwrap()
            .resolve()
            .unwrap()
    }

    fn resolve_with(json: &str, csv: &str, policy: XPolicy) -> (Res<Resolved>, Vec<String>) {
        let s = spec(json);
        let mut ctx = Ctx::new(&s);
        let t = table(csv);
        let y: Vec<String> = s.y.clone();
        let r = resolve(&mut ctx, &t, policy, &y, &[]);
        (r, ctx.notes.into_vec())
    }

    const WIDE: &str = "month,a,b\nJan,1,10\nFeb,2,20\nMar,3,30\n";

    #[test]
    fn wide_data_makes_one_series_per_value_column_and_categories_from_text() {
        let (r, n) = resolve_with(r#"{"chart":"line"}"#, WIDE, XPolicy::Auto);
        let r = r.unwrap();
        assert!(n.is_empty(), "{n:?}");
        assert_eq!(r.x.kind, XKind::Cat);
        assert_eq!(r.x.cats, ["Jan", "Feb", "Mar"]);
        assert_eq!(
            r.groups.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(
            r.points(&r.groups[1], 3),
            [(0.0, 10.0), (1.0, 20.0), (2.0, 30.0)]
        );
    }

    #[test]
    fn named_columns_select_and_order_the_series() {
        let (r, _) = resolve_with(
            r#"{"chart":"line","x":"month","y":["b","a"]}"#,
            WIDE,
            XPolicy::Auto,
        );
        let r = r.unwrap();
        assert_eq!(
            r.groups.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(),
            ["b", "a"]
        );
        assert_eq!(r.y_name, "");
    }

    #[test]
    fn long_data_makes_one_series_per_label_with_its_own_rows() {
        let csv = "m,region,v\n1,N,5\n2,N,6\n1,S,7\n2,S,8\n3,S,9\n";
        let (r, _) = resolve_with(
            r#"{"chart":"line","x":"m","y":"v","series":"region"}"#,
            csv,
            XPolicy::Auto,
        );
        let r = r.unwrap();
        assert_eq!(r.x.kind, XKind::Num);
        assert_eq!(
            r.groups.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(),
            ["N", "S"]
        );
        assert_eq!(r.points(&r.groups[0], 5), [(1.0, 5.0), (2.0, 6.0)]);
        assert_eq!(
            r.points(&r.groups[1], 5),
            [(1.0, 7.0), (2.0, 8.0), (3.0, 9.0)]
        );
        assert_eq!(r.y_name, "v");
    }

    #[test]
    fn series_with_several_value_columns_is_refused() {
        let csv = "m,region,v,w\n1,N,5,1\n";
        let (r, _) = resolve_with(
            r#"{"chart":"line","x":"m","y":["v","w"],"series":"region"}"#,
            csv,
            XPolicy::Auto,
        );
        assert_eq!(
            r.unwrap_err().0,
            "with series, give exactly one value column in y"
        );
    }

    #[test]
    fn dates_and_numbers_are_detected_and_can_be_forced() {
        let csv = "d,v\n2024-01-01,1\n2024-01-02,2\n";
        let (r, _) = resolve_with(r#"{"chart":"line"}"#, csv, XPolicy::Auto);
        assert_eq!(r.unwrap().x.kind, XKind::Time);
        let (r, _) = resolve_with(
            r#"{"chart":"line","x_scale":"category"}"#,
            csv,
            XPolicy::Auto,
        );
        assert_eq!(r.unwrap().x.kind, XKind::Cat);
        let (r, _) = resolve_with(r#"{"chart":"line","x_scale":"linear"}"#, csv, XPolicy::Auto);
        assert!(r.unwrap_err().0.contains("holds dates"));
        let (r, _) = resolve_with(
            r#"{"chart":"line","x_scale":"time"}"#,
            "d,v\nabc,1\n",
            XPolicy::Auto,
        );
        assert!(r.unwrap_err().0.contains("does not hold dates"));
    }

    #[test]
    fn a_continuous_policy_refuses_text_x() {
        let (r, _) = resolve_with(r#"{"chart":"scatter"}"#, WIDE, XPolicy::Cont);
        let e = r.unwrap_err().0;
        assert!(e.contains("holds text") && e.contains("bar chart"), "{e}");
    }

    #[test]
    fn a_category_policy_reads_numbers_as_labels_and_says_when_x_scale_is_ignored() {
        let (r, n) = resolve_with(
            r#"{"chart":"bar","x_scale":"log"}"#,
            "year,v\n2020,1\n2021,2\n",
            XPolicy::Cat,
        );
        let r = r.unwrap();
        assert_eq!(r.x.cats, ["2020", "2021"]);
        assert!(n.iter().any(|w| w.contains("x_scale is ignored")), "{n:?}");
    }

    #[test]
    fn rows_without_x_or_with_bad_numbers_are_reported_and_left_out() {
        let csv = "x,y\n1,5\n,6\nabc,7\n4,8\n5,9\n";
        let (r, n) = resolve_with(r#"{"chart":"line"}"#, csv, XPolicy::Auto);
        let r = r.unwrap();
        assert_eq!(r.points(&r.groups[0], 5).len(), 3);
        assert!(
            n.iter()
                .any(|w| w.contains("2 values are empty or not numbers; those rows are not drawn")),
            "{n:?}"
        );
    }

    #[test]
    fn gaps_in_y_stay_gaps_and_are_reported_once() {
        let csv = "x,y\n1,5\n2,\n3,oops\n4,8\n";
        let (r, n) = resolve_with(r#"{"chart":"line"}"#, csv, XPolicy::Auto);
        let r = r.unwrap();
        let p = r.points(&r.groups[0], 4);
        assert_eq!(p.len(), 4);
        assert!(p[1].1.is_nan() && p[2].1.is_nan());
        assert_eq!(n.len(), 2, "{n:?}");
    }

    #[test]
    fn missing_columns_and_empty_x_are_named() {
        let (r, _) = resolve_with(r#"{"chart":"line","x":"nope"}"#, WIDE, XPolicy::Auto);
        assert!(
            r.unwrap_err()
                .0
                .contains("column \"nope\" is not in the data")
        );
        let (r, _) = resolve_with(
            r#"{"chart":"line","x":"e","y":"v"}"#,
            "e,v\n,1\n,2\n",
            XPolicy::Auto,
        );
        assert!(r.unwrap_err().0.contains("has no values"));
        let (r, _) = resolve_with(r#"{"chart":"line"}"#, "a,b\nx,y\n", XPolicy::Auto);
        assert!(r.unwrap_err().0.contains("no column of numbers"));
    }

    #[test]
    fn categories_follow_the_sort_option_and_are_renumbered() {
        let csv = "k,v\nb,1\na,9\nc,5\n";
        let cats = |sort: &str| {
            let (r, _) = resolve_with(
                &format!(r#"{{"chart":"bar","sort":"{sort}"}}"#),
                csv,
                XPolicy::Cat,
            );
            let r = r.unwrap();
            (r.x.cats.clone(), r.x.v.clone())
        };
        assert_eq!(cats("none").0, ["b", "a", "c"]);
        assert_eq!(cats("x").0, ["a", "b", "c"]);
        assert_eq!(cats("x_desc").0, ["c", "b", "a"]);
        assert_eq!(cats("value").0, ["b", "c", "a"]);
        let (names, v) = cats("value_desc");
        assert_eq!(names, ["a", "c", "b"]);
        assert_eq!(
            v,
            [2.0, 0.0, 1.0],
            "rows are renumbered to the new positions"
        );
    }

    #[test]
    fn too_many_categories_are_capped_with_a_warning() {
        let mut csv = String::from("k,v\n");
        for i in 0..150 {
            csv.push_str(&format!("c{i:03},1\n"));
        }
        let (r, n) = resolve_with(r#"{"chart":"bar"}"#, &csv, XPolicy::Cat);
        let r = r.unwrap();
        assert_eq!(r.x.cats.len(), MAX_CATEGORIES);
        assert_eq!(r.x.v.iter().filter(|v| v.is_nan()).count(), 50);
        assert!(
            n.iter()
                .any(|w| w.contains("there are 150 categories; only the first 100 in data order")),
            "{n:?}"
        );
    }

    #[test]
    fn more_series_than_allowed_are_capped_with_a_warning() {
        let mut csv = String::from("x,s,v\n");
        for i in 0..30 {
            csv.push_str(&format!("1,s{i:02},{i}\n"));
        }
        let (r, n) = resolve_with(
            r#"{"chart":"line","x":"x","y":"v","series":"s"}"#,
            &csv,
            XPolicy::Auto,
        );
        assert_eq!(r.unwrap().groups.len(), MAX_SERIES);
        assert!(
            n.iter().any(|w| w.contains("only the first 24 series")),
            "{n:?}"
        );
    }

    #[test]
    fn the_grid_aggregates_duplicates_and_leaves_missing_cells_empty() {
        let csv = "k,a\nx,1\nx,2\ny,5\nx,\n";
        let (r, _) = resolve_with(r#"{"chart":"bar"}"#, csv, XPolicy::Cat);
        let r = r.unwrap();
        let t = table(csv);
        assert_eq!(r.grid(&t, Agg::Sum), [[3.0, 5.0]]);
        assert_eq!(r.grid(&t, Agg::Mean), [[1.5, 5.0]]);
        assert_eq!(r.grid(&t, Agg::Count), [[2.0, 1.0]]);
        let csv2 = "k,a,b\nx,1,\ny,,2\n";
        let (r2, _) = resolve_with(r#"{"chart":"bar"}"#, csv2, XPolicy::Cat);
        let g = r2.unwrap().grid(&table(csv2), Agg::Sum);
        assert!(
            g[0][1].is_nan() && g[1][0].is_nan(),
            "a series with no value at a category has a gap, not zero"
        );
    }
}
