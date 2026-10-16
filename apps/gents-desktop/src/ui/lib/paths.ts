type Parsed = {
  sep: "/" | "\\";
  root: string;
  segments: string[];
};

const separator = (path: string): "/" | "\\" =>
  path.includes("\\") && !path.includes("/") ? "\\" : "/";

const segmentsOf = (path: string) => path.split(/[\\/]/).filter(Boolean);

const sameRoot = (sep: "/" | "\\", a: string, b: string) =>
  sep === "\\" ? a.toLowerCase() === b.toLowerCase() : a === b;

function parse(path: string, home: string | null | undefined): Parsed {
  const sep = separator(path);
  const segments = segmentsOf(path);
  if (home) {
    const base = segmentsOf(home);
    const under =
      base.length > 0 &&
      segments.length >= base.length &&
      base.every((part, i) => sameRoot(sep, part, segments[i]!));
    if (under && separator(home) === sep) {
      return { sep, root: "~", segments: segments.slice(base.length) };
    }
  }
  if (path.startsWith("\\\\")) {
    return {
      sep,
      root: `\\\\${segments.slice(0, 2).join("\\")}`,
      segments: segments.slice(2),
    };
  }
  if (/^[A-Za-z]:[\\/]/.test(path)) {
    return { sep, root: segments[0]!, segments: segments.slice(1) };
  }
  return { sep, root: path.startsWith("/") ? "/" : "", segments };
}

const join = ({ sep, root, segments }: Parsed) => {
  if (root === "") return segments.join(sep);
  if (root === "/") return `/${segments.join(sep)}`;
  return segments.length ? `${root}${sep}${segments.join(sep)}` : root;
};

export const pathName = (path: string): string => segmentsOf(path).pop() ?? path;

/** Display-only shortening; `home` must belong to this computer. */
export function shortPath(
  path: string,
  {
    home = null,
    keep = Infinity,
    max = Infinity,
  }: { home?: string | null; keep?: number; max?: number } = {},
): string {
  const parsed = parse(path, home);
  const whole = join(parsed);
  if (parsed.root === "" || (parsed.segments.length <= keep && whole.length <= max)) {
    return whole;
  }
  const { sep, root, segments } = parsed;
  const lead = root === "~" ? `~${sep}…` : "…";
  let kept = Math.min(segments.length, keep);
  while (kept > 1 && `${lead}${sep}${segments.slice(-kept).join(sep)}`.length > max)
    kept--;
  if (kept === segments.length) return whole;
  return `${lead}${sep}${segments.slice(-kept).join(sep)}`;
}

export function distinctNames(
  paths: readonly string[],
  home?: string | null,
): Map<string, { name: string; where: string | null }> {
  const counts = new Map<string, number>();
  for (const path of paths)
    counts.set(pathName(path), (counts.get(pathName(path)) ?? 0) + 1);
  return new Map(
    paths.map((path) => {
      const name = pathName(path);
      const parsed = parse(path, null);
      const parent = join({ ...parsed, segments: parsed.segments.slice(0, -1) });
      return [
        path,
        {
          name,
          where: counts.get(name)! > 1 ? shortPath(parent, { home, max: 24 }) : null,
        },
      ];
    }),
  );
}
