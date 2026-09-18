import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

// Exercise Wasmtime in the signed shipping CLI, not an unsigned test executable.
const binary = resolve(process.argv[2]);
const signature = spawnSync(
  "codesign",
  ["-d", "--entitlements", ":-", binary],
  { encoding: "utf8" },
);
assert.equal(signature.status, 0, signature.stderr);
const entitlements = spawnSync(
  "plutil",
  ["-convert", "json", "-o", "-", "--", "-"],
  { input: signature.stdout, encoding: "utf8" },
);
assert.equal(entitlements.status, 0, entitlements.stderr);
assert.equal(
  JSON.parse(entitlements.stdout)[
    "com.apple.security.cs.allow-unsigned-executable-memory"
  ],
  true,
);
const root = mkdtempSync(
  join(process.env.RUNNER_TEMP ?? tmpdir(), "gents-signed-wasm-"),
);
const source = join(root, "source");
const home = join(root, "home");
mkdirSync(join(source, "source"), { recursive: true });
const env = { ...process.env };
delete env.CARGO_TARGET_DIR;
env.RUSTUP_TOOLCHAIN ??= execFileSync("rustup", ["show", "active-toolchain"], {
  encoding: "utf8",
}).split(/\s/)[0];
function run(...args) {
  const result = spawnSync(binary, args, {
    env,
    encoding: "utf8",
    timeout: 120_000,
  });
  assert.equal(
    result.status,
    0,
    `${args.join(" ")}: ${result.error ?? ""}\n${result.stderr}\n${result.stdout}`,
  );
  return result.stdout;
}
writeFileSync(
  join(source, "afb.toml"),
  `[format]
version = "1.0"
[package]
name = "release-echo"
namespace = "gents"
version = "0.1.0"
language = "rust"
entry = "source/main.rs"
[runtime]
min = "0.1.0"
`,
);
writeFileSync(
  join(source, "Cargo.toml"),
  `[workspace]
[package]
name = "release-echo"
version = "0.1.0"
edition = "2021"
[[bin]]
name = "release-echo"
path = "source/main.rs"
`,
);
writeFileSync(
  join(source, "source/main.rs"),
  "fn main() { std::io::copy(&mut std::io::stdin(), &mut std::io::stdout()).unwrap(); }\n",
);
const manifold = {
  fs: "None",
  net: "None",
  env: "None",
  crypto: false,
  child_process: false,
};
writeFileSync(join(source, "manifold.json"), JSON.stringify(manifold));
const artifact = join(root, "echo.afb");
run("plugin", "build", source, "--out", artifact);
const bytes = readFileSync(artifact);
const digest = createHash("sha256").update(bytes).digest("hex");
// Seed the same installed-plugin fixture used by plugin::run tests, offline.
mkdirSync(join(home, "plugins/store"), { recursive: true });
mkdirSync(join(home, "plugins/installed/gents"), { recursive: true });
writeFileSync(join(home, `plugins/store/${digest}.afb`), bytes);
writeFileSync(
  join(home, "plugins/installed/gents/release-echo.json"),
  JSON.stringify({
    namespace: "gents",
    name: "release-echo",
    version: "0.1.0",
    language: "rust",
    digest: `sha256:${digest}`,
    declaration: {
      name: "release-echo",
      description: "Release signing smoke",
      artifact: "plugins/release-echo.afb",
      language: "rust",
      input_schema: { type: "object" },
      manifold,
    },
  }),
);
const input = { signed_wasm_execution: "ok" };
const output = run(
  "plugin",
  "run",
  "gents/release-echo",
  "--home",
  home,
  "--input",
  JSON.stringify(input),
);
assert.deepEqual(JSON.parse(output), input);
console.log(
  `Signed runtime executed a WASM plugin successfully; evidence: ${root}`,
);
