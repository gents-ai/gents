export function isDirty<T>(current: T, baseline: T): boolean {
  return JSON.stringify(current) !== JSON.stringify(baseline);
}
