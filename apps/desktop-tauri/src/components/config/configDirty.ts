/// Dirty tracking for config editors, timing-free: each editor expresses its
/// view→form hydration as a pure function, used both by the id-keyed reset
/// effect and to compare current form values against the persisted document.
/// After a save, the snapshot refresh updates the view and the comparison
/// heals itself — and a backend that normalizes values keeps the editor
/// honestly marked dirty until the form matches what is actually persisted.
export function isDirty<T>(current: T, baseline: T): boolean {
  return JSON.stringify(current) !== JSON.stringify(baseline);
}
