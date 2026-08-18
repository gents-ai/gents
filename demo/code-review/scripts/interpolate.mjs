import { readFileSync } from "node:fs";
import { join } from "node:path";

/** Expand ${VAR} and ${VAR:-default} the same way gents desired-state does. */
export function interpolate(input, lookup = (name) => process.env[name]) {
  let out = "";
  const missing = [];
  let i = 0;
  while (i < input.length) {
    if (input[i] !== "$") {
      const start = i;
      while (i < input.length && input[i] !== "$") {
        i += 1;
      }
      out += input.slice(start, i);
      continue;
    }
    if (input[i + 1] === "$") {
      out += "$";
      i += 2;
      continue;
    }
    if (input[i + 1] !== "{") {
      out += "$";
      i += 1;
      continue;
    }
    const close = input.indexOf("}", i + 2);
    if (close < 0) {
      out += input.slice(i);
      break;
    }
    const spec = input.slice(i + 2, close);
    const sep = spec.indexOf(":-");
    const name = (sep >= 0 ? spec.slice(0, sep) : spec).trim();
    const fallback = sep >= 0 ? spec.slice(sep + 2) : null;
    const raw = lookup(name);
    const value = raw && raw.length > 0 ? raw : null;
    if (value !== null) {
      out += value;
    } else if (fallback !== null) {
      out += fallback;
    } else if (!missing.includes(name)) {
      missing.push(name);
    }
    i = close + 1;
  }
  if (missing.length > 0) {
    throw new Error(`unset interpolation variables: ${missing.join(", ")}`);
  }
  return out;
}

export function loadPackManifest(packRoot) {
  const text = readFileSync(join(packRoot, "experiment.json"), "utf8");
  return JSON.parse(interpolate(text));
}
