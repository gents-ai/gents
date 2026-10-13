/* The runtime loader: a plugin as code, loaded after build time. Every
   non-bundled plugin takes this pipeline:

     module text -> import allowlist (`@gents/ux-sdk` / `react*` only)
       -> bare-specifier rewrite to live shim blobs (sdk/runtime.ts)
       -> blob import() with a deadline -> validate the default export
       -> activate (register(ctx), rollback on throw)

   Loading the same id again disposes the previous incarnation first, so a
   rewritten file is a clean reload. Failures land on the plugin's own
   inventory row; a broken plugin can never take the app down.

   This is error isolation, not a capability boundary: a loaded plugin runs
   in the webview with the app's full authority. The allowlist is an
   honest-author lint over a lexer that cannot settle every `)` followed by
   `/`; the webview's CSP (`script-src 'self' blob:`) is what keeps a module
   from loading code this loader did not hand it. */
import { activateUxPlugin, unloadUxPlugin, type ActivateOptions } from "./activate";
import { isGentsUxPlugin, type GentsUxPlugin } from "./plugin";
import { dropUxPlugin, publishUxPlugin, uxPluginRecords } from "./plugins-store";
import { installUxSdk, sdkImportMap } from "@/sdk/runtime";

/* a top-level await that never settles would otherwise hang import()
   forever, and through a sequential scan, every plugin after it */
export const IMPORT_TIMEOUT_MS = 10_000;

/* the specifier of a static `from '…'`, a side-effect `import '…'`, or a
   dynamic `import('…')`: the quoted literal up to its own closing quote (the
   other quote is a plain character inside it); a match counts only when it
   sits in code (see scan), never in a string or comment */
const importSpecifierRe = () =>
  /(from\s*|import\s*\(\s*|import\s+)(['"])((?:(?!\2)[^\n])+)\2/g;

/* a dynamic import whose argument is not a quoted literal alone: the
   specifier is computed, which no allowlist can read */
const computedImportRe = () => /\bimport\s*\((?!\s*(['"])(?:(?!\1)[^\n])*\1\s*\))/g;

const regexKeywordRe =
  /^(?:await|case|delete|do|else|in|instanceof|new|of|return|throw|typeof|void|yield)$/;

/* whether the `/` at `slash` opens a regex literal: the previous significant
   character cannot end a value (the standard division-vs-regex heuristic) */
function isRegexStart(source: string, slash: number): boolean {
  let j = slash - 1;
  while (j >= 0 && /\s/.test(source[j]!)) j -= 1;
  if (j < 0) return true;
  const prev = source[j]!;
  if (prev === "+" || prev === "-") return source[j - 1] !== prev;
  if (prev === ")" || prev === "]" || prev === "'" || prev === '"' || prev === "`")
    return false;
  if (prev === "}") return true;
  if (/[A-Za-z0-9_$]/.test(prev)) {
    let k = j;
    while (k >= 0 && /[A-Za-z0-9_$]/.test(source[k]!)) k -= 1;
    if (source[k] === ".") return false;
    return regexKeywordRe.test(source.slice(k + 1, j + 1));
  }
  return true;
}

/* the end of the regex literal opened at `slash`, or -1 when it never
   closes on this line (so the `/` was a division) */
function regexEnd(source: string, slash: number): number {
  let j = slash + 1;
  let inClass = false;
  while (j < source.length) {
    const c = source[j];
    if (c === "\\") j += 2;
    else if (c === "\n") return -1;
    else if (c === "[") {
      inClass = true;
      j += 1;
    } else if (c === "]") {
      inClass = false;
      j += 1;
    } else if (c === "/" && !inClass) {
      j += 1;
      while (j < source.length && /[A-Za-z]/.test(source[j]!)) j += 1;
      return j;
    } else j += 1;
  }
  return -1;
}

/* one pass over `source`: the character ranges that are code (strings,
   template text, comments and regex literals excluded; a template's ${…} is
   code), and `text`, the source with comments, template text and regex
   literals blanked, index for index, so the import regexes read specifiers
   with nothing between `from` and the quote but whitespace */
export function scan(source: string): {
  ranges: Array<[number, number]>;
  text: string;
} {
  const ranges: Array<[number, number]> = [];
  const blanked: Array<[number, number]> = [];
  const stack: Array<"expr" | "template"> = [];
  let state:
    "block-comment" | "code" | "double" | "line-comment" | "single" | "template" =
    "code";
  let codeStart = 0;
  let blankStart = -1;
  /* set when a slash that could open a regex found no close on its line:
     the rest of the line divides, without a rescan per slash, so a line of
     slashes costs one pass; code stays code, which only adds findings */
  let deadLine = false;
  let i = 0;
  const closeCode = (end: number, keep: boolean) => {
    if (end > codeStart) ranges.push([codeStart, end]);
    blankStart = keep ? -1 : end;
  };
  const openCode = (at: number) => {
    if (blankStart >= 0 && at > blankStart) blanked.push([blankStart, at]);
    blankStart = -1;
    codeStart = at;
  };
  while (i < source.length) {
    const ch = source[i]!;
    const next = i + 1 < source.length ? source[i + 1]! : "";
    if (ch === "\n") deadLine = false;
    if (state === "code") {
      if (ch === "/" && next === "/") {
        closeCode(i, false);
        state = "line-comment";
        i += 2;
      } else if (ch === "/" && next === "*") {
        closeCode(i, false);
        state = "block-comment";
        i += 2;
      } else if (ch === "'") {
        closeCode(i, true);
        state = "single";
        i += 1;
      } else if (ch === '"') {
        closeCode(i, true);
        state = "double";
        i += 1;
      } else if (ch === "`") {
        closeCode(i, false);
        stack.push("template");
        state = "template";
        i += 1;
      } else if (ch === "/") {
        const opens = !deadLine && isRegexStart(source, i);
        const end = opens ? regexEnd(source, i) : -1;
        if (end > 0) {
          closeCode(i, false);
          i = end;
          openCode(i);
        } else {
          if (opens) deadLine = true;
          i += 1;
        }
      } else if (ch === "}" && stack[stack.length - 1] === "expr") {
        closeCode(i, false);
        stack.pop();
        state = "template";
        i += 1;
      } else i += 1;
      continue;
    }
    if (state === "line-comment") {
      if (ch === "\n") {
        state = "code";
        openCode(i);
      }
      i += 1;
      continue;
    }
    if (state === "block-comment") {
      if (ch === "*" && next === "/") {
        i += 2;
        state = "code";
        openCode(i);
      } else i += 1;
      continue;
    }
    if (state === "single" || state === "double") {
      if (ch === "\\") i += 2;
      else if (ch === (state === "single" ? "'" : '"')) {
        i += 1;
        state = "code";
        openCode(i);
      } else if (ch === "\n") {
        /* unterminated: recover as code so one stray quote cannot swallow
           the rest of the file */
        i += 1;
        state = "code";
        openCode(i);
      } else i += 1;
      continue;
    }
    /* template text */
    if (ch === "\\") i += 2;
    else if (ch === "$" && next === "{") {
      stack.push("expr");
      state = "code";
      i += 2;
      openCode(i);
    } else if (ch === "`") {
      stack.pop();
      state = "code";
      i += 1;
      openCode(i);
    } else i += 1;
  }
  if (state === "code") closeCode(source.length, true);
  else openCode(source.length);
  let text = source;
  if (blanked.length) {
    const chars = source.split("");
    for (const [start, end] of blanked) {
      for (let k = start; k < end && k < chars.length; k += 1) {
        if (chars[k] !== "\n") chars[k] = " ";
      }
    }
    text = chars.join("");
  }
  return { ranges, text };
}

export function codeRanges(source: string): Array<[number, number]> {
  return scan(source).ranges;
}

function inCode(ranges: Array<[number, number]>, at: number): boolean {
  for (const [start, end] of ranges) {
    if (at < start) return false;
    if (at < end) return true;
  }
  return false;
}

/* only mapped specifiers are rewritten, and only in code; the match is
   found on the blanked text and replaced at the same index in the source */
export function rewriteSpecifiers(source: string, map: Record<string, string>): string {
  const { ranges, text } = scan(source);
  const edits: Array<{ start: number; end: number; replacement: string }> = [];
  for (const m of text.matchAll(importSpecifierRe())) {
    const [whole, pre, quote, spec] = m;
    const at = m.index ?? 0;
    if (!spec || !map[spec] || !inCode(ranges, at)) continue;
    edits.push({
      start: at + pre!.length,
      end: at + whole.length,
      replacement: `${quote}${map[spec]}${quote}`,
    });
  }
  let out = "";
  let last = 0;
  for (const edit of edits) {
    out += source.slice(last, edit.start) + edit.replacement;
    last = edit.end;
  }
  return out + source.slice(last);
}

/** what a computed dynamic import is reported as */
export const COMPUTED_IMPORT = "import(<computed>)";

/* every import specifier outside the allowlist: a bare package would only
   fail later as a cryptic native error, a relative path cannot resolve
   against a blob: base, and a URL scheme is a second stage; a dynamic
   import whose argument is anything but one quoted literal is reported as
   computed, since no list can read it */
export function unsupportedImports(
  source: string,
  allowed: readonly string[],
): string[] {
  const unsupported = new Set<string>();
  const { ranges, text } = scan(source);
  for (const m of text.matchAll(importSpecifierRe())) {
    const spec = m[3];
    if (!spec || !inCode(ranges, m.index ?? 0)) continue;
    if (!allowed.includes(spec)) unsupported.add(spec);
  }
  for (const m of text.matchAll(computedImportRe())) {
    if (inCode(ranges, m.index ?? 0)) unsupported.add(COMPUTED_IMPORT);
  }
  return [...unsupported];
}

/* a <style> per plugin, replaced on reload and removed on unload */
const styles = new Map<string, HTMLStyleElement>();

/* the one dynamic import of a URL the bundler cannot see; isolated so the
   evaluation site reads as what it is */
function importBlob(url: string): Promise<{ default?: unknown }> {
  return import(url) as Promise<{ default?: unknown }>;
}

function installCss(id: string, css: string | null | undefined): () => void {
  styles.get(id)?.remove();
  styles.delete(id);
  if (!css || typeof document === "undefined") return () => {};
  const el = document.createElement("style");
  el.dataset.gentsUx = id;
  el.textContent = css;
  document.head.appendChild(el);
  styles.set(id, el);
  return () => {
    if (styles.get(id) === el) {
      el.remove();
      styles.delete(id);
    }
  };
}

export interface RuntimeModule {
  /** the ESM source of the plugin */
  source: string;
  /** optional stylesheet installed beside it */
  css?: string | null;
}

export interface LoadOptions extends Omit<ActivateOptions, "door"> {
  door?: ActivateOptions["door"];
  /** the inventory name for a module that never yields a plugin id */
  origin: string;
}

/* evaluate one runtime module through the whole pipeline; the loaded
   plugin id, or null on failure (the failure is on an inventory row) */
export async function loadRuntimeUxPlugin(
  module: RuntimeModule,
  options: LoadOptions,
): Promise<string | null> {
  installUxSdk();
  const door = options.door ?? "dev";
  try {
    const map = sdkImportMap();
    const unsupported = unsupportedImports(module.source, Object.keys(map));
    if (unsupported.length > 0) {
      throw new Error(
        `unsupported import${unsupported.length > 1 ? "s" : ""}: ${unsupported.join(", ")}; ` +
          `a UX plugin may only import @gents/ux-sdk, react and react/jsx-runtime`,
      );
    }
    const url = URL.createObjectURL(
      new Blob([rewriteSpecifiers(module.source, map)], { type: "text/javascript" }),
    );
    let mod: { default?: unknown };
    let deadline: ReturnType<typeof setTimeout> | undefined;
    try {
      /* a blob: URL built at runtime: not a module the bundler could ever
         resolve, so it is left as a plain dynamic import (the repo's
         no-hidden-imports ratchet in tests/pick-directory.test.ts guards
         real module specifiers, which this is not) */
      mod = await Promise.race([
        importBlob(url),
        new Promise<never>((_, reject) => {
          deadline = setTimeout(
            () =>
              reject(
                new Error(
                  `import timed out after ${IMPORT_TIMEOUT_MS / 1000}s; module evaluation never settled`,
                ),
              ),
            IMPORT_TIMEOUT_MS,
          );
        }),
      ]);
    } finally {
      clearTimeout(deadline);
      URL.revokeObjectURL(url);
    }
    const plugin = mod.default;
    if (!isGentsUxPlugin(plugin)) {
      throw new Error(`${options.origin} has no valid default GentsUxPlugin export`);
    }
    /* a runtime copy of a plugin that ships bundled must not register a
       second time; the bundled copy wins, visibly */
    const existing = uxPluginRecords()[plugin.id];
    if (existing?.door === "bundled") {
      publishUxPlugin({
        id: `${plugin.id}:shadowed`,
        name: `${plugin.name ?? plugin.id} (shadowed)`,
        description: `Shadowed by the bundled "${plugin.id}" plugin; this copy is not used.`,
        door,
        file: options.file,
        status: "disabled",
      });
      return null;
    }
    /* two sources claiming one id: the first loaded owns it */
    if (existing && existing.file !== options.file) {
      throw new Error(
        `duplicate id "${plugin.id}", already loaded from ${existing.file ?? existing.door}`,
      );
    }
    const disposeCss = installCss(plugin.id, module.css);
    const wrapped: GentsUxPlugin = {
      ...plugin,
      register: (ctx) => {
        ctx.onDispose(disposeCss);
        return plugin.register(ctx);
      },
    };
    activateUxPlugin(wrapped, { ...options, door });
    return plugin.id;
  } catch (error) {
    console.error(`[ux-plugins] runtime load failed (${options.origin})`, error);
    publishUxPlugin({
      id: options.origin,
      name: options.origin,
      door,
      file: options.file,
      pack: options.pack,
      status: "error",
      error: error instanceof Error ? error.message : String(error),
    });
    return null;
  }
}

/** forget a runtime plugin entirely: registrations, styles, inventory row */
export function retireRuntimeUxPlugin(id: string): void {
  unloadUxPlugin(id);
  styles.get(id)?.remove();
  styles.delete(id);
  dropUxPlugin(id);
}
