export function fileReference(
  value: string | undefined,
): { path: string; line?: number } | null {
  if (
    !value ||
    value.startsWith("#") ||
    value.startsWith("//") ||
    /^[a-z][a-z\d+.-]*:/i.test(value)
  )
    return null;
  const location =
    /^(.*?)(?:#L(\d+)(?:C\d+)?(?:-L?\d+)?|:(\d+)(?::\d+)?)$/.exec(value);
  const path = location?.[1] ?? value;
  if (!path || path.includes("\0") || path.includes("?")) return null;
  const line = location ? Number(location[2] ?? location[3]) : undefined;
  if (line !== undefined && (!Number.isSafeInteger(line) || line < 1))
    return null;
  try {
    return { path: decodeURIComponent(path), line };
  } catch {
    return null;
  }
}
