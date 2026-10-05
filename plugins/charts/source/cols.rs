//! Typed views of table columns and the small grouping helpers every chart
//! type shares: numbers with gaps, dates, labels, category order, aggregation.
//! Whatever cannot be used is counted and reported, never turned into zero.

use crate::dates;
use crate::err::{Res, fail};
use crate::format::compact;
use crate::spec::{Agg, Sort};
use crate::table::{Cell, Table, plain_number};

const MAX_WARNINGS: usize = 40;

/// Warnings collected while building a chart, in order, without repeats.
#[derive(Debug, Default)]
pub struct Notes {
    items: Vec<String>,
    dropped: usize,
}

impl Notes {
    /// Adds a warning unless it is already there.
    pub fn add(&mut self, text: impl Into<String>) {
        let text = crate::text::limit_chars(&text.into(), 500).into_owned();
        if self.items.contains(&text) {
            return;
        }
        if self.items.len() < MAX_WARNINGS {
            self.items.push(text);
        } else {
            self.dropped += 1;
        }
    }

    /// The warnings, with a closing line when some were left out.
    pub fn into_vec(mut self) -> Vec<String> {
        if self.dropped > 0 {
            self.items
                .push(format!("{} more warnings were left out", self.dropped));
        }
        self.items
    }

    /// Number of warnings so far.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// True when there are none.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.dropped == 0
    }
}

/// Index of the column called `name`.
pub fn column(t: &Table, name: &str) -> Res<usize> {
    let name = name.trim();
    match t.col(name) {
        Some(i) => Ok(i),
        None => {
            let shown: Vec<&str> = t.names.iter().take(12).map(String::as_str).collect();
            let more = if t.names.len() > 12 { ", ..." } else { "" };
            if t.names.is_empty() {
                fail(format!(
                    "column {name:?} is not in the data, which has no columns"
                ))
            } else {
                fail(format!(
                    "column {name:?} is not in the data; the columns are {}{more}",
                    shown.join(", ")
                ))
            }
        }
    }
}

/// What a column holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColKind {
    /// Mostly numbers.
    Numeric,
    /// Dates or times.
    Time,
    /// Labels.
    Text,
    /// Nothing but empty cells.
    Empty,
}

/// Classifies column `c`.
pub fn kind(t: &Table, c: usize) -> ColKind {
    let (mut nums, mut strs, mut dates_n) = (0usize, 0usize, 0usize);
    for cell in &t.cols[c] {
        match cell {
            Cell::Null => {}
            Cell::Num(_) => nums += 1,
            Cell::Str(i) => {
                strs += 1;
                dates_n += usize::from(dates::parse(t.pool.get(*i)).is_some());
            }
        }
    }
    if nums + strs == 0 {
        ColKind::Empty
    } else if nums > 0 && strs == 0 || nums >= strs && nums > 0 {
        ColKind::Numeric
    } else if dates_n == strs {
        ColKind::Time
    } else {
        ColKind::Text
    }
}

/// Numbers of one column; gaps are `NaN`.
#[derive(Debug, Clone, Default)]
pub struct Numeric {
    /// One value per row.
    pub v: Vec<f64>,
    /// Empty cells.
    pub nulls: usize,
    /// Cells holding text that is not a number.
    pub text: usize,
    /// First such cell: its 1-based data row and its text.
    pub first_text: Option<(usize, String)>,
}

/// Reads column `c` as numbers.
pub fn numeric(t: &Table, c: usize) -> Numeric {
    let mut n = Numeric {
        v: Vec::with_capacity(t.rows),
        ..Numeric::default()
    };
    for (row, cell) in t.cols[c].iter().enumerate() {
        match cell {
            Cell::Num(v) => n.v.push(*v),
            Cell::Null => {
                n.nulls += 1;
                n.v.push(f64::NAN);
            }
            Cell::Str(i) => {
                n.text += 1;
                if n.first_text.is_none() {
                    n.first_text = Some((row + 1, shorten(t.pool.get(*i))));
                }
                n.v.push(f64::NAN);
            }
        }
    }
    n
}

fn shorten(s: &str) -> String {
    let mut out: String = s.chars().take(24).collect();
    if s.chars().count() > 24 {
        out.push('\u{2026}');
    }
    out
}

/// Adds the warnings for empty and non-numeric cells of `name`.
pub fn report_numeric(notes: &mut Notes, name: &str, n: &Numeric) {
    if n.nulls > 0 {
        notes.add(format!(
            "column {name:?}: {} empty values are drawn as gaps, not zeros",
            n.nulls
        ));
    }
    if let Some((row, text)) = &n.first_text {
        notes.add(format!(
            "column {name:?}: {} values are not numbers (first is {text:?} in row {row}) and are drawn as gaps, not zeros",
            n.text
        ));
    }
}

/// Dates of column `c` as epoch seconds, `NaN` for empty cells; `None` when
/// the column has no dates or holds anything that is not a date.
pub fn times(t: &Table, c: usize) -> Option<Vec<f64>> {
    let mut out = Vec::with_capacity(t.rows);
    let mut any = false;
    for cell in &t.cols[c] {
        match cell {
            Cell::Null => out.push(f64::NAN),
            Cell::Str(i) => {
                out.push(dates::parse(t.pool.get(*i))?);
                any = true;
            }
            Cell::Num(_) => return None,
        }
    }
    any.then_some(out)
}

/// Text of a number as a label: whole numbers without decimals or separators.
pub fn number_label(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        compact(v).replace(',', "")
    }
}

/// Label of each row; `None` for empty cells.
pub fn labels(t: &Table, c: usize) -> Vec<Option<String>> {
    t.cols[c]
        .iter()
        .map(|cell| match cell {
            Cell::Null => None,
            Cell::Num(v) => Some(number_label(*v)),
            Cell::Str(i) => Some(t.pool.get(*i).to_owned()),
        })
        .collect()
}

/// The distinct labels in order of first appearance and each row's index into
/// them (`None` for rows without a label).
pub fn distinct(labels: &[Option<String>]) -> (Vec<String>, Vec<Option<usize>>) {
    let mut names: Vec<String> = Vec::new();
    let mut index = std::collections::HashMap::<&str, usize>::new();
    let rows = labels
        .iter()
        .map(|l| {
            l.as_deref().map(|s| {
                *index.entry(s).or_insert_with(|| {
                    names.push(s.to_owned());
                    names.len() - 1
                })
            })
        })
        .collect();
    (names, rows)
}

fn label_order(a: &str, b: &str) -> std::cmp::Ordering {
    match (plain_number(a), plain_number(b)) {
        (Some(x), Some(y)) => x.total_cmp(&y),
        _ => match (dates::parse(a), dates::parse(b)) {
            (Some(x), Some(y)) => x.total_cmp(&y),
            _ => a.cmp(b),
        },
    }
}

/// The order of `names` under `sort`, as a list of indices. `totals` holds
/// each name's value for the value orders. Ties keep their first-seen order.
pub fn order(names: &[String], totals: &[f64], sort: Sort) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..names.len()).collect();
    let by_total = |a: &usize, b: &usize| {
        let (x, y) = (totals[*a], totals[*b]);
        match (x.is_nan(), y.is_nan()) {
            (true, true) => std::cmp::Ordering::Equal,
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            _ => x.total_cmp(&y),
        }
    };
    match sort {
        Sort::None => {}
        Sort::X => idx.sort_by(|a, b| label_order(&names[*a], &names[*b])),
        Sort::XDesc => idx.sort_by(|a, b| label_order(&names[*b], &names[*a])),
        Sort::Value => idx.sort_by(by_total),
        Sort::ValueDesc => idx.sort_by(|a, b| {
            let (x, y) = (totals[*a], totals[*b]);
            match (x.is_nan(), y.is_nan()) {
                (true, true) => std::cmp::Ordering::Equal,
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
                _ => y.total_cmp(&x),
            }
        }),
    }
    idx
}

/// Combines `values` (finite numbers only) with `agg`; `NaN` when there are none.
pub fn aggregate(values: &[f64], agg: Agg) -> f64 {
    if agg == Agg::Count {
        return values.len() as f64;
    }
    if values.is_empty() {
        return f64::NAN;
    }
    match agg {
        Agg::Sum => values.iter().sum(),
        Agg::Mean => values.iter().sum::<f64>() / values.len() as f64,
        Agg::Min => values.iter().copied().fold(f64::INFINITY, f64::min),
        Agg::Max => values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        Agg::Median => {
            let mut v = values.to_vec();
            v.sort_by(f64::total_cmp);
            let n = v.len();
            if n % 2 == 1 {
                v[n / 2]
            } else {
                (v[n / 2 - 1] + v[n / 2]) / 2.0
            }
        }
        Agg::Count => unreachable!("handled above"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::Builder;

    fn table(csv: &str) -> Table {
        let mut b = Builder::new(None);
        crate::csvio::read(csv.as_bytes(), &mut b).unwrap();
        b.finish()
    }

    #[test]
    fn unknown_columns_list_the_known_ones() {
        let t = table("a,b,c\n1,2,3\n");
        assert_eq!(column(&t, " b ").unwrap(), 1);
        assert_eq!(
            column(&t, "z").unwrap_err().0,
            "column \"z\" is not in the data; the columns are a, b, c"
        );
        let wide = table(&format!(
            "{}\n{}\n",
            (0..20)
                .map(|i| format!("c{i}"))
                .collect::<Vec<_>>()
                .join(","),
            "1,".repeat(19) + "1"
        ));
        let e = column(&wide, "zz").unwrap_err().0;
        assert!(e.ends_with("c11, ..."), "{e}");
        assert!(
            column(&Table::default(), "x")
                .unwrap_err()
                .0
                .contains("no columns")
        );
    }

    #[test]
    fn kinds_are_decided_from_the_cells() {
        let t = table("n,d,s,e,m\n1,2024-01-01,a,,1\n2,2024-02-01,b,,x\n3,,c,,2\n");
        assert_eq!(kind(&t, 0), ColKind::Numeric);
        assert_eq!(kind(&t, 1), ColKind::Time);
        assert_eq!(kind(&t, 2), ColKind::Text);
        assert_eq!(kind(&t, 3), ColKind::Empty);
        assert_eq!(
            kind(&t, 4),
            ColKind::Numeric,
            "mostly numbers with one stray text is numeric"
        );
    }

    #[test]
    fn a_column_of_mostly_text_with_one_number_is_text() {
        let t = table("m\nx\ny\n1\n");
        assert_eq!(kind(&t, 0), ColKind::Text);
    }

    #[test]
    fn numeric_view_marks_gaps_and_remembers_the_first_bad_cell() {
        let t = table("v,z\n1,0\n,0\nN/A,0\n4,0\nbad,0\n");
        let n = numeric(&t, 0);
        assert_eq!(n.v[0], 1.0);
        assert!(n.v[1].is_nan() && n.v[2].is_nan() && n.v[4].is_nan());
        assert_eq!((n.nulls, n.text), (2, 1));
        assert_eq!(n.first_text, Some((5, "bad".into())));
    }

    #[test]
    fn na_markers_count_as_empty_and_other_text_as_non_numeric() {
        let t = table("v\nN/A\nnull\nunknown\n");
        let n = numeric(&t, 0);
        assert_eq!((n.nulls, n.text), (2, 1));
    }

    #[test]
    fn reports_name_the_column_and_never_say_zero_was_used() {
        let t = table("v,z\n1,0\n,0\nbad,0\n");
        let mut notes = Notes::default();
        report_numeric(&mut notes, "v", &numeric(&t, 0));
        let w = notes.into_vec();
        assert_eq!(w.len(), 2);
        assert!(w[0].contains("1 empty values") && w[0].contains("not zeros"));
        assert!(w[1].contains("\"bad\" in row 3") && w[1].contains("not zeros"));
        let mut quiet = Notes::default();
        report_numeric(&mut quiet, "v", &numeric(&table("v\n1\n2\n"), 0));
        assert!(quiet.is_empty());
    }

    #[test]
    fn long_bad_text_is_shortened_in_the_report() {
        let t = table(&format!("v\n{}\n", "x".repeat(80)));
        let n = numeric(&t, 0);
        assert_eq!(n.first_text.unwrap().1.chars().count(), 25);
    }

    #[test]
    fn dates_convert_and_one_bad_cell_rejects_the_column() {
        let t = table("d,e,f\n2024-01-01,2024-01-01,5\n,not a date,6\n2024-01-03,2024-01-03,7\n");
        let d = times(&t, 0).unwrap();
        assert_eq!(d[0], 1_704_067_200.0);
        assert!(d[1].is_nan());
        assert_eq!(times(&t, 1), None);
        assert_eq!(times(&t, 2), None);
        assert_eq!(times(&table("d\n\n\n"), 0), None);
    }

    #[test]
    fn number_labels_drop_decimals_and_separators() {
        assert_eq!(number_label(2019.0), "2019");
        assert_eq!(number_label(-5.0), "-5");
        assert_eq!(number_label(12.5), "12.5");
        assert_eq!(number_label(1234567.5), "1234568");
        assert_eq!(number_label(0.1235), "0.1235");
    }

    #[test]
    fn labels_cover_numbers_text_and_gaps() {
        let t = table("k,z\nnorth,0\n2020,0\n,0\n007,0\n");
        let l = labels(&t, 0);
        assert_eq!(
            l,
            [
                Some("north".into()),
                Some("2020".into()),
                None,
                Some("007".into())
            ]
        );
    }

    #[test]
    fn distinct_keeps_first_appearance_order() {
        let l: Vec<Option<String>> = ["b", "a", "b", "c", "a"]
            .iter()
            .map(|s| Some((*s).to_owned()))
            .chain([None])
            .collect();
        let (names, rows) = distinct(&l);
        assert_eq!(names, ["b", "a", "c"]);
        assert_eq!(rows, [Some(0), Some(1), Some(0), Some(2), Some(1), None]);
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn label_sorting_is_numeric_for_numbers_chronological_for_dates_and_bytewise_for_text() {
        assert_eq!(
            order(&names(&["10", "9", "100"]), &[0.0; 3], Sort::X),
            [1, 0, 2]
        );
        assert_eq!(
            order(
                &names(&["2024-03-01", "2023-12-31", "2024-01-15"]),
                &[0.0; 3],
                Sort::X
            ),
            [1, 2, 0]
        );
        assert_eq!(
            order(&names(&["b", "C", "a"]), &[0.0; 3], Sort::X),
            [1, 2, 0]
        );
        assert_eq!(
            order(&names(&["b", "C", "a"]), &[0.0; 3], Sort::XDesc),
            [0, 2, 1]
        );
        assert_eq!(order(&names(&["b", "a"]), &[0.0; 2], Sort::None), [0, 1]);
    }

    #[test]
    fn value_orders_use_totals_keep_ties_in_place_and_put_gaps_last() {
        let n = names(&["a", "b", "c", "d"]);
        let totals = [3.0, f64::NAN, 3.0, 9.0];
        assert_eq!(order(&n, &totals, Sort::Value), [0, 2, 3, 1]);
        assert_eq!(order(&n, &totals, Sort::ValueDesc), [3, 0, 2, 1]);
    }

    #[test]
    fn aggregation_covers_every_function_and_empty_input() {
        let v = [4.0, 1.0, 3.0, 2.0];
        assert_eq!(aggregate(&v, Agg::Sum), 10.0);
        assert_eq!(aggregate(&v, Agg::Mean), 2.5);
        assert_eq!(aggregate(&v, Agg::Count), 4.0);
        assert_eq!(aggregate(&v, Agg::Min), 1.0);
        assert_eq!(aggregate(&v, Agg::Max), 4.0);
        assert_eq!(aggregate(&v, Agg::Median), 2.5);
        assert_eq!(aggregate(&[5.0, 1.0, 3.0], Agg::Median), 3.0);
        assert!(aggregate(&[], Agg::Sum).is_nan() && aggregate(&[], Agg::Median).is_nan());
        assert_eq!(aggregate(&[], Agg::Count), 0.0);
    }

    #[test]
    fn notes_deduplicate_and_cap_with_a_closing_line() {
        let mut n = Notes::default();
        n.add("same");
        n.add("same");
        assert_eq!(n.len(), 1);
        for i in 0..60 {
            n.add(format!("w{i}"));
        }
        let v = n.into_vec();
        assert_eq!(v.len(), MAX_WARNINGS + 1);
        assert_eq!(v.last().unwrap(), "21 more warnings were left out");
    }
}
