#!/usr/bin/env node

import { basename, dirname, join } from "node:path";
import { rmSync } from "node:fs";

const packageRoot = process.cwd();
if (
  basename(dirname(packageRoot)) !== "packages" ||
  !basename(packageRoot).startsWith("gents-desktop-")
) {
  throw new Error(
    `Refusing to clean dist outside a packages/gents-desktop-* directory: ${packageRoot}`,
  );
}

rmSync(join(packageRoot, "dist"), { force: true, recursive: true });
