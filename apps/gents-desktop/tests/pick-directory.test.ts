import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";

const open = vi.fn();
vi.mock("@tauri-apps/plugin-dialog", () => ({ open }));

import { canPickDirectory, pickDirectory } from "../src/ui/lib/pickDirectory";

const uiRoot = join(__dirname, "../src/ui");

function sources(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) return sources(path);
    return /\.tsx?$/.test(name) ? [path] : [];
  });
}

describe("pickDirectory", () => {
  afterEach(() => {
    delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
    open.mockReset();
  });

  it("imports the dialog plugin with a literal specifier so the bundle carries it", () => {
    const source = readFileSync(join(uiRoot, "lib/pickDirectory.ts"), "utf8");
    expect(source).toContain('await import("@tauri-apps/plugin-dialog")');
    expect(source).not.toContain("@vite-ignore");
  });

  it("never hides a module from the bundler anywhere in the UI", () => {
    for (const file of sources(uiRoot)) {
      expect(readFileSync(file, "utf8"), file).not.toContain("@vite-ignore");
    }
  });

  it("returns null without the desktop shell and never loads the plugin", async () => {
    expect(canPickDirectory()).toBe(false);
    await expect(pickDirectory({ title: "Choose" })).resolves.toBeNull();
    expect(open).not.toHaveBeenCalled();
  });

  it("opens a single-directory picker in the desktop shell", async () => {
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
    open.mockResolvedValue("/Users/A Person/Projects");
    await expect(
      pickDirectory({ defaultPath: "/Users/A Person", title: "Choose" }),
    ).resolves.toBe("/Users/A Person/Projects");
    expect(open).toHaveBeenCalledWith({
      directory: true,
      multiple: false,
      defaultPath: "/Users/A Person",
      title: "Choose",
    });
  });
});
