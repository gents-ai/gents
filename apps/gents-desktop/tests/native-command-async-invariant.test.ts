import { readFileSync, readdirSync } from "node:fs";
import { join, resolve } from "node:path";
import { describe, expect, it } from "vitest";

const COMMANDS_DIR = resolve(
  process.cwd(),
  "../../crates/gents-desktop-bridge/src/tauri_commands",
);

const STARTUP_SOURCE = join(COMMANDS_DIR, "lifecycle", "core_start.rs");

function rustSources(directory: string): string[] {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) return rustSources(path);
    return entry.name.endsWith(".rs") ? [path] : [];
  });
}

describe("native command async invariants", () => {
  it("never blocks async work on a Tauri command thread", () => {
    const offenders = rustSources(COMMANDS_DIR)
      .filter((path) => path !== STARTUP_SOURCE)
      .filter((path) =>
        readFileSync(path, "utf8").includes(
          "tauri::async_runtime::block_on",
        ),
      );

    expect(offenders).toEqual([]);
  });

  it("only blocks runtime startup on its dedicated background thread", () => {
    const source = readFileSync(STARTUP_SOURCE, "utf8");
    const occurrences = source.match(/tauri::async_runtime::block_on/g) ?? [];

    expect(occurrences).toHaveLength(1);
    expect(source).toContain(
      "ClientCore::start_with_paths_reporting_stages(paths, options, stage_tx).await",
    );
    expect(source).toContain("start_client_core_async");
    expect(source).toContain("std::thread::Builder::new()");
    expect(source).toContain('.name("desktop-client-start".to_string())');
    expect(source).toContain(".spawn(move || {");
    const coordinator = readFileSync(
      join(COMMANDS_DIR, "lifecycle", "single_flight.rs"),
      "utf8",
    );
    expect(coordinator).toContain("single-flight");
    expect(coordinator).toContain("start_client_core_async(");
    expect(source).not.toContain(".join()");
  });
});
