import { readFileSync, readdirSync } from "node:fs";
import { join, resolve } from "node:path";
import { describe, expect, it } from "vitest";

const COMMANDS_DIR = resolve(
  process.cwd(),
  "../../crates/gents-desktop-bridge/src/tauri_commands",
);

describe("native command async invariants", () => {
  it("never blocks async work on a Tauri command thread", () => {
    const offenders = readdirSync(COMMANDS_DIR)
      .filter((name) => name.endsWith(".rs") && name !== "lifecycle.rs")
      .filter((name) =>
        readFileSync(join(COMMANDS_DIR, name), "utf8").includes(
          "tauri::async_runtime::block_on",
        ),
      );

    expect(offenders).toEqual([]);
  });

  it("only blocks runtime startup on its dedicated background thread", () => {
    const source = readFileSync(join(COMMANDS_DIR, "lifecycle.rs"), "utf8");
    const occurrences = source.match(/tauri::async_runtime::block_on/g) ?? [];

    expect(occurrences).toHaveLength(1);
    // Large-stack OS thread still runs block_on; the async command awaits a
    // oneshot instead of thread::join so Tokio workers stay unblocked.
    expect(source).toContain(
      "tauri::async_runtime::block_on(ClientCore::start_with_paths(paths))",
    );
    expect(source).toContain("start_client_core_async");
    expect(source).toContain("single-flight");
    expect(source).not.toContain(".join()");
  });
});
