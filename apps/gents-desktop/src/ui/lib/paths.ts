/* How a filesystem path reads on screen. Only for display: an exact path
   (an input's value, a copy, a delete confirmation) stays whole, and
   whatever shortens a path puts the full one in the title. */

type Parsed = {
  sep: "/" | "\\";
  /** "~", "/", a drive such as "C:", a UNC share, or "" for a relative path */
  root: string;
  segments: string[];
};

/* a machine that separates with a backslash has the same long prefix to
   lose, and a drive letter is as absolute as a leading slash */
const separator = (path: string): "/" | "\\" =>
  path.includes("\\") && !path.includes("/") ? "\\" : "/";

const segmentsOf = (path: string) => path.split(/[\\/]/).filter(Boolean);

/* Windows and macOS compare paths without regard to case by default */
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

/** The last segment of a path: the folder or file it names. */
export const pathName = (path: string): string => segmentsOf(path).pop() ?? path;

/**
 * A path as it reads on screen. With `home`, a path under it starts at `~`;
 * pass it only for paths on this computer, since a remote host's home is
 * not this one. A long absolute path loses folders from the left, the end
 * that every path in a list shares, and keeps at least its own name:
 * folders go while more than `keep` remain or it is longer than `max`
 * characters. A relative path is already short and stays whole.
 */
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

/**
 * Each path's name, and for names two paths share, a short form of the
 * folder it sits in, so the rows can be told apart.
 */
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
