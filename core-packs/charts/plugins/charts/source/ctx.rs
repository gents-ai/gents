//! What every chart type shares while it is built: the validated spec, the
//! theme, the canvas size and the warnings so far.

use crate::cols::Notes;
use crate::palette::{Theme, series_color};
use crate::spec::Spec;

/// Build state of one chart.
#[derive(Debug)]
pub struct Ctx<'a> {
    /// The request.
    pub spec: &'a Spec,
    /// Colours.
    pub theme: Theme,
    /// Canvas width.
    pub w: f64,
    /// Canvas height.
    pub h: f64,
    /// Warnings collected so far.
    pub notes: Notes,
}

impl<'a> Ctx<'a> {
    /// A context for `spec`.
    pub fn new(spec: &'a Spec) -> Self {
        let mut notes = Notes::default();
        for n in &spec.notes {
            notes.add(n.clone());
        }
        Self {
            spec,
            theme: spec.theme,
            w: f64::from(spec.width),
            h: f64::from(spec.height),
            notes,
        }
    }

    /// Colour of series `i`.
    pub fn color(&self, i: usize) -> String {
        series_color(i, &self.theme, self.spec.colors.as_deref())
    }
}
