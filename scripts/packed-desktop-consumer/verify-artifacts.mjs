import { readFileSync, statSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const modules = [
  "@source-inc/gents-desktop-client",
  "@source-inc/gents-desktop-client/testing",
  "@source-inc/gents-desktop-ui",
  "@source-inc/gents-desktop-chat",
  "@source-inc/gents-desktop-fleet",
  "@source-inc/gents-desktop-fleet/local-runtime",
  "@source-inc/gents-desktop-operations",
];

for (const moduleName of modules) {
  await import(moduleName);
}

const styles = [
  "@source-inc/gents-desktop-tokens/semantic.css",
  "@source-inc/gents-desktop-ui/styles.css",
  "@source-inc/gents-desktop-chat/styles.css",
  "@source-inc/gents-desktop-fleet/styles.css",
  "@source-inc/gents-desktop-fleet/local-runtime.css",
  "@source-inc/gents-desktop-operations/styles.css",
];

const checkedCss = new Set();

function verifyCssFile(path) {
  const absolute = resolve(path);
  if (checkedCss.has(absolute)) return;
  checkedCss.add(absolute);

  const stat = statSync(absolute);
  if (!stat.isFile()) {
    throw new Error(`CSS export is not a file: ${absolute}`);
  }

  const source = readFileSync(absolute, "utf8");
  for (const match of source.matchAll(
    /@import\s+(?:url\(\s*)?["']([^"']+)["']\s*\)?/g,
  )) {
    const imported = match[1];
    if (
      imported.startsWith("http:") ||
      imported.startsWith("https:") ||
      imported.startsWith("data:")
    ) {
      continue;
    }
    verifyCssFile(resolve(dirname(absolute), imported));
  }
}

for (const style of styles) {
  const resolved = import.meta.resolve(style);
  if (!resolved.startsWith("file:")) {
    throw new Error(`Expected local CSS export for ${style}, got ${resolved}`);
  }
  verifyCssFile(fileURLToPath(resolved));
}

console.log(
  `Executed ${modules.length} package entrypoints and verified ${checkedCss.size} CSS files.`,
);
