export function optionalString(value?: string | null) {
  const trimmed = value?.trim();
  return trimmed && trimmed.length > 0 ? trimmed : "";
}

export function linesToArray(value: string) {
  return value
    .split(/\r?\n|,/)
    .map((item) => item.trim())
    .filter((item) => item.length > 0);
}

export function parseOptionalInt(value: string) {
  const trimmed = value.trim();
  if (!trimmed) {
    return null;
  }
  const parsed = Number(trimmed);
  return Number.isInteger(parsed) ? parsed : null;
}

export function parseOptionalFloat(value: string) {
  const trimmed = value.trim();
  if (!trimmed) {
    return null;
  }
  const parsed = Number(trimmed);
  return Number.isFinite(parsed) ? parsed : null;
}

export function boolText(value?: boolean | null) {
  return value === false ? "disabled" : "enabled";
}

export function isOptionalInt(
  value: string,
  options: { min?: number; max?: number } = {},
) {
  const trimmed = value.trim();
  if (!trimmed) {
    return true;
  }
  const parsed = parseOptionalInt(trimmed);
  return (
    parsed != null &&
    (options.min == null || parsed >= options.min) &&
    (options.max == null || parsed <= options.max)
  );
}

export function isOptionalFloat(
  value: string,
  options: { min?: number; max?: number } = {},
) {
  const trimmed = value.trim();
  if (!trimmed) {
    return true;
  }
  const parsed = parseOptionalFloat(trimmed);
  return (
    parsed != null &&
    (options.min == null || parsed >= options.min) &&
    (options.max == null || parsed <= options.max)
  );
}

export function ignoreHandledActionError(_error: unknown) {
}
